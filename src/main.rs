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

use std::collections::HashMap;
use std::io::{IsTerminal, Read};
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

    // Resolve the config path (CLI override → XDG/HOME default → cwd fallback),
    // load it, and surface any warnings once the app is built. Shared by both modes.
    let config_path = args
        .config
        .or_else(Config::default_path)
        .unwrap_or_else(|| PathBuf::from(".hunkr.toml"));
    let (config, warnings) = Config::load(&config_path);

    // Pager mode: a diff was piped in (stdin is not a TTY). Read and view it as a
    // read-only viewer rather than scanning a working tree. Live mode otherwise.
    let mut app = if !std::io::stdin().is_terminal() {
        match build_pager_app(config, config_path)? {
            Some(app) => app,
            None => return Ok(()), // empty input / non-TTY stdout already reported
        }
    } else {
        let start = match args.path {
            Some(p) => p,
            None => std::env::current_dir()?,
        };
        let repo_root = git::repo::discover(&start)?;
        App::new(repo_root, config, config_path)?
    };

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

/// Read a piped diff from stdin and build a read-only pager-mode [`App`]. Returns
/// `Ok(None)` (after reporting to stderr) when stdout isn't a terminal or the
/// input carries no reviewable file diff — the caller then exits cleanly.
fn build_pager_app(config: Config, config_path: PathBuf) -> Result<Option<App>> {
    // The TUI draws to stdout; if that's redirected (e.g. `git diff | hunkr > f`)
    // there's nothing to drive, so bail rather than spray escape codes into a file.
    if !std::io::stdout().is_terminal() {
        eprintln!("hunkr: pager mode needs a terminal on stdout (don't redirect output)");
        std::process::exit(2);
    }

    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;
    // Tolerate non-UTF-8 like the live path, and strip the ANSI colour git emits
    // to a pager by default so the parser sees raw `+`/`-`/` ` markers.
    let text = strip_ansi(&String::from_utf8_lossy(&buf));

    // The piped diff is fully read; now repoint stdin at the controlling terminal
    // so crossterm can read keyboard input (see `redirect_stdin_to_tty`).
    redirect_stdin_to_tty();

    let parsed = git::diff::split_unified(&text);
    if parsed.is_empty() {
        eprintln!("hunkr: no diff on stdin (expected `git diff`/`git show` output)");
        return Ok(None);
    }

    let files: Vec<_> = parsed.iter().map(|(f, _)| f.clone()).collect();
    let diffs: HashMap<_, _> = parsed.into_iter().map(|(f, d)| (f.path, d)).collect();
    let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    Ok(Some(App::from_diff(
        repo_root,
        files,
        diffs,
        config,
        config_path,
    )))
}

/// Point stdin at the controlling terminal so crossterm can read keys in pager
/// mode (where the real stdin is the diff pipe, already drained by now).
///
/// crossterm's input reader opens whatever `tty_fd()` resolves to: stdin if it's
/// a TTY, else `/dev/tty`. On macOS, kqueue (via mio) **cannot register the
/// `/dev/tty` magic device** — it returns `EINVAL` — so the `/dev/tty` path fails
/// and input is dead. We sidestep that by resolving the *real* terminal device
/// (via `ttyname` on stdout, which is the terminal in pager mode) and `dup2`-ing
/// it onto fd 0, so crossterm sees a registrable TTY on stdin. Best-effort: any
/// failure leaves stdin as-is (Linux's epoll handles `/dev/tty` fine regardless).
#[cfg(unix)]
fn redirect_stdin_to_tty() {
    use std::ffi::CStr;
    use std::os::fd::AsRawFd;

    // SAFETY: ttyname returns a pointer to a static/thread-local buffer valid
    // until the next ttyname call; we copy out of it immediately.
    let path = unsafe {
        let p = libc::ttyname(libc::STDOUT_FILENO);
        if p.is_null() {
            return;
        }
        match CStr::from_ptr(p).to_str() {
            Ok(s) => s.to_owned(),
            Err(_) => return,
        }
    };
    if let Ok(f) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
    {
        // dup2 onto fd 0; `f` then drops, but fd 0 keeps the duplicated descriptor.
        unsafe {
            libc::dup2(f.as_raw_fd(), libc::STDIN_FILENO);
        }
    }
}

#[cfg(not(unix))]
fn redirect_stdin_to_tty() {}

/// Strip ANSI CSI escape sequences (`ESC [ … <0x40–0x7E>`, e.g. SGR colour) from
/// `input`. Only ASCII escape bytes are dropped, so UTF-8 content is preserved.
fn strip_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    if !bytes.contains(&0x1b) {
        return input.to_string();
    }
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            if bytes.get(i + 1) == Some(&b'[') {
                i += 2;
                // Consume parameter/intermediate bytes up to the final byte.
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                i += 1; // consume the final byte (if any)
            } else {
                i += 1; // bare ESC or non-CSI escape introducer
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn run(tui: &mut terminal::Tui, app: &mut App) -> Result<()> {
    let (tx, rx) = unbounded::<Event>();

    // Off-thread git worker + filesystem watcher for hot reload. If watching
    // can't start, the app still works — it just won't auto-refresh. Pager mode
    // is a static viewer, so neither is spawned (and no events ever arrive).
    let git_req = if app.live {
        let req = event::spawn_git_worker(app.repo_root.clone(), app.base, tx.clone());
        if let Err(e) = watch::spawn(app.repo_root.clone(), tx.clone()) {
            app.error = Some(format!("watch disabled: {e}"));
        }
        Some(req)
    } else {
        None
    };

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
                    if let Some(req) = &git_req {
                        let _ = req.send(app.reviewed_paths());
                    }
                }
                Event::Refreshed(snapshot) => app.reconcile(snapshot),
                Event::Error(msg) => {
                    app.error = Some(msg);
                    app.dirty = true;
                }
            }
        }

        // Editor + config-edit only apply in live mode (pager mode is read-only
        // and never sets these requests).
        if app.live {
            // Fulfil an editor request (suspends the TUI for the editor session).
            if let Some(req) = app.take_editor_request() {
                open_editor(tui, app, req)?;
            }

            // Open the config in $EDITOR (creating a template first if needed),
            // then hot-reload it on return.
            if app.take_config_edit_request() {
                open_config(tui, app)?;
            }
        }

        // Auto-dismiss a transient toast once its lifetime elapses. The poll
        // above bounds how often this runs, so the toast clears on its own.
        app.expire_toast();
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
