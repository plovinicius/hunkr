//! Filesystem watcher for hot reload.
//!
//! Watches the repo root recursively and posts a single debounced `Event::Fs`
//! per burst of changes. Two things are essential:
//!
//! * **Most of `.git/` is filtered out, but ref/HEAD updates are kept.** git
//!   rewrites its index/lock files constantly (including as a side effect of
//!   our own `git status`); watching them would create an infinite refresh
//!   loop. We do let through changes to `HEAD`, `refs/`, `packed-refs`, and
//!   `ORIG_HEAD` so that committing/checking-out/resetting in another tab
//!   refreshes the view — those files don't move from a read-only `git
//!   status`, so they can't feed back.
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

/// An event matters if it touches the worktree, or one of the few `.git/`
/// paths that signal a real state change (commit / checkout / reset / fetch).
fn is_relevant(res: &notify::Result<notify::Event>, git_dir: &Path) -> bool {
    match res {
        Ok(ev) => ev.paths.iter().any(|p| is_relevant_path(p, git_dir)),
        Err(_) => false,
    }
}

/// True if `p` is either outside `.git/` (a worktree change) or one of the
/// inside-`.git/` files that flip on commit/checkout/reset but never on a
/// passive `git status` read.
fn is_relevant_path(p: &Path, git_dir: &Path) -> bool {
    match p.strip_prefix(git_dir) {
        // Worktree change: always interesting.
        Err(_) => true,
        // Inside .git/: only ref / HEAD updates qualify. Specifically,
        // `.git/index` (+ `.lock`) and `.git/objects/` are excluded — they
        // churn on every `git status`, which would create a feedback loop.
        Ok(rel) => {
            rel.starts_with("refs")
                || matches!(
                    rel.to_str(),
                    Some("HEAD") | Some("ORIG_HEAD") | Some("packed-refs")
                )
        }
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
    fn ignores_index_and_lock_churn() {
        let git = Path::new("/repo/.git");
        // `git status` itself touches these — watching them would loop.
        assert!(!is_relevant(&ev(&["/repo/.git/index"]), git));
        assert!(!is_relevant(&ev(&["/repo/.git/index.lock"]), git));
        assert!(!is_relevant(&ev(&["/repo/.git/objects/ab/cdef0123"]), git));
    }

    #[test]
    fn reacts_to_worktree_changes() {
        let git = Path::new("/repo/.git");
        assert!(is_relevant(&ev(&["/repo/src/main.rs"]), git));
    }

    #[test]
    fn reacts_to_ref_and_head_updates() {
        let git = Path::new("/repo/.git");
        // Commit / checkout / reset / fetch all surface through these paths,
        // and a passive `git status` never touches them.
        assert!(is_relevant(&ev(&["/repo/.git/HEAD"]), git));
        assert!(is_relevant(&ev(&["/repo/.git/refs/heads/main"]), git));
        assert!(is_relevant(&ev(&["/repo/.git/refs/tags/v1"]), git));
        assert!(is_relevant(&ev(&["/repo/.git/packed-refs"]), git));
        assert!(is_relevant(&ev(&["/repo/.git/ORIG_HEAD"]), git));
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
