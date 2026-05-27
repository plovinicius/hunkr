//! Filesystem watcher for hot reload.
//!
//! Watches the repo root recursively and posts a single debounced `Event::Fs`
//! per burst of changes. Two things are essential:
//!
//! * **`.git/` is filtered out.** git rewrites its index/refs/lock files
//!   constantly (including as a side effect of our own `git status`); watching
//!   them would create an infinite refresh loop.
//! * **Bursts are coalesced.** An AI agent rewriting many files fires a storm of
//!   raw events; we collapse them into one refresh after a short quiet period.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{RecvTimeoutError, Sender, unbounded};
use notify::{RecursiveMode, Watcher};

use crate::event::Event;

/// Quiet period after the last raw event before we emit a refresh.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Start watching `repo_root`. The returned watcher thread keeps the underlying
/// notify watcher alive and feeds debounced `Event::Fs` messages onto `bus`.
pub fn spawn(repo_root: PathBuf, bus: Sender<Event>) -> Result<()> {
    let (raw_tx, raw_rx) = unbounded::<notify::Result<notify::Event>>();

    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = raw_tx.send(res);
    })
    .context("failed to create filesystem watcher")?;
    watcher
        .watch(&repo_root, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", repo_root.display()))?;

    let git_dir = repo_root.join(".git");

    thread::spawn(move || {
        // Hold the watcher for the thread's lifetime; dropping it stops watching.
        let _watcher = watcher;
        loop {
            // Block for the first event of a burst.
            let Ok(first) = raw_rx.recv() else {
                return; // channel closed
            };
            let mut relevant = is_relevant(&first, &git_dir);

            // Drain everything that arrives within the debounce window.
            let deadline = Instant::now() + DEBOUNCE;
            loop {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                match raw_rx.recv_timeout(deadline - now) {
                    Ok(ev) => relevant |= is_relevant(&ev, &git_dir),
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }

            if relevant && bus.send(Event::Fs).is_err() {
                return; // UI gone
            }
        }
    });

    Ok(())
}

/// An event matters only if it touches something outside `.git/`.
fn is_relevant(res: &notify::Result<notify::Event>, git_dir: &Path) -> bool {
    match res {
        Ok(ev) => ev.paths.iter().any(|p| !p.starts_with(git_dir)),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::{Event as NotifyEvent, EventKind};

    fn ev(paths: &[&str]) -> notify::Result<NotifyEvent> {
        let mut e = NotifyEvent::new(EventKind::Any);
        for p in paths {
            e = e.add_path(PathBuf::from(p));
        }
        Ok(e)
    }

    #[test]
    fn ignores_git_internal_changes() {
        let git = Path::new("/repo/.git");
        // git's own index/ref/lock churn must never trigger a refresh.
        assert!(!is_relevant(&ev(&["/repo/.git/index"]), git));
        assert!(!is_relevant(&ev(&["/repo/.git/refs/heads/main"]), git));
        assert!(!is_relevant(&ev(&["/repo/.git/index.lock"]), git));
    }

    #[test]
    fn reacts_to_worktree_changes() {
        let git = Path::new("/repo/.git");
        assert!(is_relevant(&ev(&["/repo/src/main.rs"]), git));
    }

    #[test]
    fn mixed_burst_with_any_worktree_path_is_relevant() {
        let git = Path::new("/repo/.git");
        assert!(is_relevant(
            &ev(&["/repo/.git/index", "/repo/src/main.rs"]),
            git
        ));
    }

    #[test]
    fn pathless_or_errored_events_are_ignored() {
        let git = Path::new("/repo/.git");
        assert!(!is_relevant(&ev(&[]), git));
        assert!(!is_relevant(&Err(notify::Error::generic("boom")), git));
    }
}
