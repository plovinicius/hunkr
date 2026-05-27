//! Event bus. The UI thread blocks on a single channel; producer threads feed
//! it: the terminal input reader (always), the filesystem watcher (`Fs`), and
//! the git worker (`Refreshed` / `Error`). The UI thread never touches git or
//! the filesystem directly, so it stays responsive under large repos.

use std::path::PathBuf;
use std::thread;

use crossbeam_channel::{Sender, unbounded};
use ratatui::crossterm::event::{self, Event as CtEvent};

use crate::git;
use crate::model::snapshot::GitSnapshot;

pub enum Event {
    /// A terminal event (key / mouse / resize).
    Input(CtEvent),
    /// A debounced burst of filesystem changes occurred — trigger a refresh.
    Fs,
    /// A freshly computed snapshot of the repo's changed files.
    Refreshed(GitSnapshot),
    /// A background error to surface in the status bar.
    Error(String),
}

/// Spawn a thread that forwards terminal events onto the bus. It exits when
/// `event::read()` errors or the receiver is dropped.
pub fn spawn_input(tx: Sender<Event>) {
    thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if tx.send(Event::Input(ev)).is_err() {
                break;
            }
        }
    });
}

/// Spawn the git worker. It owns all blocking git work: on each request it
/// recomputes the changed-file snapshot off the UI thread and posts it back.
/// Returns a sender used to request a refresh; requests coalesce so a burst of
/// `Fs` events collapses into at most one extra recompute.
pub fn spawn_git_worker(repo_root: PathBuf, bus: Sender<Event>) -> Sender<()> {
    let (req_tx, req_rx) = unbounded::<()>();
    thread::spawn(move || {
        while req_rx.recv().is_ok() {
            // Coalesce any requests queued while we were busy.
            while req_rx.try_recv().is_ok() {}
            match git::status::changed_files(&repo_root) {
                Ok(files) => {
                    if bus.send(Event::Refreshed(GitSnapshot { files })).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    if bus
                        .send(Event::Error(format!("git refresh failed: {e}")))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    req_tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn git_worker_returns_a_snapshot_on_request() {
        // Runs against this crate's repo (cargo test's CWD), proving the
        // request → `git status` → `Refreshed` plumbing end-to-end, with no
        // dependency on filesystem-event timing.
        let (tx, rx) = unbounded();
        let repo = std::env::current_dir().unwrap();
        let req = spawn_git_worker(repo, tx);
        req.send(()).unwrap();
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Event::Refreshed(_)) => {}
            Ok(_) => panic!("expected Event::Refreshed"),
            Err(e) => panic!("git worker produced nothing: {e}"),
        }
    }
}
