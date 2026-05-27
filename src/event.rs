//! Background event bus. Producer threads — the filesystem watcher (`Fs`) and
//! the git worker (`Refreshed` / `Error`) — feed a channel that the UI thread
//! drains. Terminal input is read directly on the UI thread (see
//! `main::run`) rather than on a thread, so that shelling out to `$EDITOR` can
//! take exclusive control of the terminal without a second reader competing for
//! keystrokes.

use std::collections::HashSet;
use std::path::PathBuf;
use std::thread;

use crossbeam_channel::{Sender, unbounded};

use crate::git::{self, diff::DiffBase};
use crate::model::snapshot::GitSnapshot;

pub enum Event {
    /// A debounced burst of filesystem changes occurred — trigger a refresh.
    Fs,
    /// A freshly computed snapshot of the repo's changed files.
    Refreshed(GitSnapshot),
    /// A background error to surface in the status bar.
    Error(String),
}

/// Spawn the git worker. It owns all blocking git work: on each request it
/// recomputes the changed-file snapshot off the UI thread and posts it back.
///
/// The request payload is the set of paths the UI cares about hashing (the
/// reviewed files); the worker computes current diff hashes only for those that
/// are actually changed, so cost scales with the (small) reviewed set, not the
/// whole repo. Returns a sender used to request a refresh; requests coalesce.
pub fn spawn_git_worker(
    repo_root: PathBuf,
    base: DiffBase,
    bus: Sender<Event>,
) -> Sender<Vec<PathBuf>> {
    let (req_tx, req_rx) = unbounded::<Vec<PathBuf>>();
    thread::spawn(move || {
        while let Ok(mut reviewed) = req_rx.recv() {
            // Coalesce queued requests; keep the most recent reviewed set.
            while let Ok(next) = req_rx.try_recv() {
                reviewed = next;
            }
            match git::status::changed_files(&repo_root) {
                Ok(files) => {
                    let wanted: HashSet<PathBuf> = reviewed.into_iter().collect();
                    let hashes = git::diff::diff_hashes_for(
                        &repo_root,
                        base,
                        files.iter().filter(|f| wanted.contains(&f.path)),
                    );
                    if bus
                        .send(Event::Refreshed(GitSnapshot { files, hashes }))
                        .is_err()
                    {
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
        let base = git::diff::detect_base(&repo);
        let req = spawn_git_worker(repo, base, tx);
        req.send(Vec::new()).unwrap();
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Event::Refreshed(_)) => {}
            Ok(_) => panic!("expected Event::Refreshed"),
            Err(e) => panic!("git worker produced nothing: {e}"),
        }
    }
}
