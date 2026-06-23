//! hunkr — a terminal git diff reviewer for AI coding workflows.
//!
//! Architecture: a single owned [`app::App`] is the source of truth, mutated
//! only here on the UI thread. The UI thread reads terminal input directly
//! (polling), and drains background events (filesystem watch + git worker) from
//! a channel. Rendering is dirty-flagged — we draw only when state changed.
//! Reading input on this thread (rather than a dedicated thread) lets us hand
//! the terminal entirely to `$EDITOR` without a second reader stealing input.

mod app;
mod cache;
mod cli;
mod config;
mod event;
mod git;
mod glyphs;
mod model;
mod persist;
mod reference;
mod render;
mod terminal;
mod ui;
mod watch;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossbeam_channel::unbounded;
use ratatui::crossterm::event::{Event as CtEvent, poll, read};

use crate::app::{App, EditorRequest};
use crate::config::Config;
use crate::event::Event;

/// How long to wait for input before checking for background events. Input
/// itself is handled the instant a key arrives; this only bounds how quickly
/// filesystem/git refreshes are picked up.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Editors known to accept the `+LINE` argument to jump to a line.
const PLUS_LINE_EDITORS: &[&str] = &["vi", "vim", "nvim", "gvim", "mvim", "nano", "emacs", "kak"];

fn main() -> Result<()> {
    let args = cli::Args::parse();
    let start = match args.path {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    let repo_root = git::repo::discover(&start)?;

    // Resolve the config path (CLI override → XDG/HOME default → cwd fallback),
    // load it, and surface any warnings once the app is built.
    let config_path = args
        .config
        .or_else(Config::default_path)
        .unwrap_or_else(|| PathBuf::from(".hunkr.toml"));
    let (config, warnings) = Config::load(&config_path);

    let mut app = App::new(repo_root, config, config_path)?;
    app.glyphs = if args.ascii {
        glyphs::Glyphs::ascii()
    } else {
        glyphs::Glyphs::unicode()
    };
    if !warnings.is_empty() {
        app.config_error = Some(warnings.join("; "));
    }

    terminal::install_panic_hook();
    let mut tui = terminal::init()?;
    let result = run(&mut tui, &mut app);
    terminal::restore()?;
    result
}

fn run(tui: &mut terminal::Tui, app: &mut App) -> Result<()> {
    let (tx, rx) = unbounded::<Event>();

    // Off-thread git worker + filesystem watcher for hot reload. If watching
    // can't start, the app still works — it just won't auto-refresh.
    let git_req = event::spawn_git_worker(app.repo_root.clone(), app.base, tx.clone());
    if let Err(e) = watch::spawn(app.repo_root.clone(), tx.clone()) {
        app.error = Some(format!("watch disabled: {e}"));
    }

    while !app.should_quit {
        if app.dirty {
            tui.draw(|f| ui::render(f, app))?;
            app.dirty = false;
        }

        // Handle terminal input. `poll` returns the instant a key is available,
        // so input stays responsive; the timeout bounds how soon we notice
        // background events.
        if poll(POLL_INTERVAL)? {
            handle_terminal_event(app, read()?);
            while poll(Duration::ZERO)? {
                handle_terminal_event(app, read()?);
            }
        }

        // Drain background events without blocking.
        while let Ok(ev) = rx.try_recv() {
            match ev {
                Event::Fs => {
                    // A change landed; recompute off-thread, re-hashing the
                    // files currently marked reviewed so we can flag changes.
                    let _ = git_req.send(app.reviewed_paths());
                }
                Event::Refreshed(snapshot) => app.reconcile(snapshot),
                Event::Error(msg) => {
                    app.error = Some(msg);
                    app.dirty = true;
                }
            }
        }

        // Fulfil an editor request (suspends the TUI for the editor session).
        if let Some(req) = app.take_editor_request() {
            open_editor(tui, app, req)?;
        }

        // Open the config in $EDITOR (creating a template first if needed), then
        // hot-reload it on return.
        if app.take_config_edit_request() {
            open_config(tui, app)?;
        }
    }
    Ok(())
}

fn handle_terminal_event(app: &mut App, ev: CtEvent) {
    match ev {
        CtEvent::Key(key) => app.on_key(key),
        CtEvent::Mouse(m) => app.on_mouse(m),
        CtEvent::Resize(_, _) => app.dirty = true,
        _ => {}
    }
}

/// Suspend the TUI, run `$EDITOR` on the requested file (at its line, for
/// editors that support `+LINE`), then restore the TUI and force a redraw.
fn open_editor(tui: &mut terminal::Tui, app: &mut App, req: EditorRequest) -> Result<()> {
    let abs = app.repo_root.join(&req.path);
    if let Err(e) = run_editor(tui, &abs, Some(req.line)) {
        app.error = Some(e);
    }
    app.dirty = true;
    Ok(())
}

/// Suspend the TUI and open the config file in `$EDITOR` (writing the commented
/// template first if it doesn't exist yet), then hot-reload it.
fn open_config(tui: &mut terminal::Tui, app: &mut App) -> Result<()> {
    let path = app.config_path.clone();
    if !path.exists()
        && let Err(e) = write_config_template(&path)
    {
        app.error = Some(e);
        app.dirty = true;
        return Ok(());
    }
    if let Err(e) = run_editor(tui, &path, None) {
        app.error = Some(e);
        app.dirty = true;
        return Ok(());
    }
    app.reload_config();
    Ok(())
}

/// Create the config's parent directory and seed it with the commented template.
fn write_config_template(path: &Path) -> std::result::Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, Config::default_template())
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// Hand the terminal to `$EDITOR` for `abs` (optionally jumping to `line`),
/// then take it back. Returns a human-readable message on failure.
fn run_editor(
    tui: &mut terminal::Tui,
    abs: &Path,
    line: Option<u32>,
) -> std::result::Result<(), String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());

    // `$EDITOR` may carry arguments (e.g. "code -w"); split program from args.
    let mut parts = editor.split_whitespace();
    let Some(program) = parts.next() else {
        return Err("$EDITOR is empty".into());
    };
    let pre_args: Vec<&str> = parts.collect();

    let supports_line = Path::new(program)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| PLUS_LINE_EDITORS.contains(&n))
        .unwrap_or(false);

    let mut cmd = Command::new(program);
    cmd.args(&pre_args);
    if let Some(line) = line
        && supports_line
    {
        cmd.arg(format!("+{line}"));
    }
    cmd.arg(abs);

    let status = run_suspended(tui, cmd)?;
    if let Err(e) = status {
        return Err(format!("could not run editor `{program}`: {e}"));
    }
    Ok(())
}

/// Leave the alternate screen, run `cmd` to completion, then re-enter the TUI.
/// Restoring the terminal can itself fail (it returns the real `Result`); the
/// inner `io::Result` is the command's own exit status.
fn run_suspended(
    tui: &mut terminal::Tui,
    mut cmd: Command,
) -> std::result::Result<std::io::Result<ExitStatus>, String> {
    terminal::restore().map_err(|e| format!("could not suspend terminal: {e}"))?;
    let status = cmd.status();
    *tui = terminal::init().map_err(|e| format!("could not restore terminal: {e}"))?;
    tui.clear()
        .map_err(|e| format!("could not clear terminal: {e}"))?;
    Ok(status)
}
