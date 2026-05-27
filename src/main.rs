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

use std::process::Command;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossbeam_channel::unbounded;
use ratatui::crossterm::event::{Event as CtEvent, poll, read};

use crate::app::{App, EditorRequest};
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
    let mut app = App::new(repo_root)?;
    app.glyphs = if args.unicode {
        glyphs::Glyphs::unicode()
    } else {
        glyphs::Glyphs::ascii()
    };

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
    }
    Ok(())
}

fn handle_terminal_event(app: &mut App, ev: CtEvent) {
    match ev {
        CtEvent::Key(key) => app.on_key(key),
        CtEvent::Resize(_, _) => app.dirty = true,
        _ => {}
    }
}

/// Suspend the TUI, run `$EDITOR` on the requested file (at its line, for
/// editors that support `+LINE`), then restore the TUI and force a redraw.
fn open_editor(tui: &mut terminal::Tui, app: &mut App, req: EditorRequest) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());

    // `$EDITOR` may carry arguments (e.g. "code -w"); split program from args.
    let mut parts = editor.split_whitespace();
    let Some(program) = parts.next() else {
        app.error = Some("$EDITOR is empty".into());
        app.dirty = true;
        return Ok(());
    };
    let pre_args: Vec<&str> = parts.collect();

    let abs = app.repo_root.join(&req.path);
    let supports_line = std::path::Path::new(program)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| PLUS_LINE_EDITORS.contains(&n))
        .unwrap_or(false);

    let mut cmd = Command::new(program);
    cmd.args(&pre_args);
    if supports_line {
        cmd.arg(format!("+{}", req.line));
    }
    cmd.arg(&abs);

    // Hand the terminal to the editor, then take it back.
    terminal::restore()?;
    let status = cmd.status();
    *tui = terminal::init()?;
    tui.clear()?;
    app.dirty = true;

    if let Err(e) = status {
        app.error = Some(format!("could not run editor `{program}`: {e}"));
    }
    Ok(())
}
