//! Background event bus. Producer threads — the filesystem watcher (`Fs`) and
//! the git worker (`Refreshed` / `Error`) — feed a channel that the UI thread
//! drains. Terminal input is read directly on the UI thread (see
//! `main::run`) rather than on a thread, so that shelling out to `$EDITOR` can
//! take exclusive control of the terminal without a second reader competing for
//! keystrokes.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Sender, unbounded};

use crate::git::{self, diff::DiffBase};
use crate::highlight::FileHighlight;
use crate::model::diff::FileDiff;
use crate::model::snapshot::GitSnapshot;

pub enum Event {
    /// A debounced burst of filesystem changes occurred — trigger a refresh.
    Fs,
    /// A freshly computed snapshot of the repo's changed files.
    Refreshed(GitSnapshot),
    /// Syntax highlighting for a file, computed off the UI thread. `generation`
    /// lets the UI drop a result that a newer file/theme selection has superseded
    /// for *display*; `theme`/`diff_hash` key it in the highlight cache (so a
    /// prefetched neighbour is reused without recompute when it's later opened).
    Highlighted {
        path: PathBuf,
        theme: String,
        diff_hash: u64,
        generation: u64,
        highlight: Arc<FileHighlight>,
    },
    /// A background error to surface in the status bar.
    Error(String),
}

/// A request to highlight one file's diff off the UI thread.
pub struct HighlightRequest {
    pub path: PathBuf,
    pub diff: Arc<FileDiff>,
    pub theme: String,
    pub truecolor: bool,
    pub diff_hash: u64,
    /// Generation of the originating selection. Foreground requests carry the
    /// latest selection's generation (and are displayed only if it still
    /// matches); prefetch requests use 0 (cached on arrival, never displayed).
    pub generation: u64,
}

/// Spawn the foreground highlight worker: the file the user is currently looking
/// at. Highlighting a large diff with syntect takes tens to hundreds of
/// milliseconds, which would stutter navigation if done on the UI thread, so it
/// runs here and posts the result back. Requests coalesce to the most recent, so
/// rapid navigation only ever computes the latest selection. This worker is
/// dedicated to the foreground, so background prefetch can never delay it.
pub fn spawn_highlight_worker(bus: Sender<Event>) -> Sender<HighlightRequest> {
    let (req_tx, req_rx) = unbounded::<HighlightRequest>();
    thread::spawn(move || {
        while let Ok(mut req) = req_rx.recv() {
            while let Ok(next) = req_rx.try_recv() {
                req = next; // coalesce: only the latest selection matters
            }
            if emit(&bus, req).is_err() {
                break;
            }
        }
    });
    req_tx
}

/// Spawn the prefetch pool: `threads` workers that warm the highlight cache for
/// files the user hasn't opened yet, in the order the UI hands them out. They
/// share one queue (so the next free worker takes the next file) and run
/// independently of the foreground worker, so warming the whole changeset never
/// blocks the file in view. Each result is cached but not displayed directly.
pub fn spawn_prefetch_pool(bus: Sender<Event>, threads: usize) -> Sender<HighlightRequest> {
    let (req_tx, req_rx) = unbounded::<HighlightRequest>();
    for _ in 0..threads.max(1) {
        let rx = req_rx.clone();
        let bus = bus.clone();
        thread::spawn(move || {
            while let Ok(req) = rx.recv() {
                if emit(&bus, req).is_err() {
                    break;
                }
            }
        });
    }
    req_tx
}

/// Highlight one request and post the result. Returns `Err` if the bus is closed.
fn emit(bus: &Sender<Event>, req: HighlightRequest) -> Result<(), ()> {
    let Some(theme) = crate::highlight::theme(&req.theme) else {
        return Ok(());
    };
    let hl = crate::highlight::highlight_file(&req.diff, theme, req.truecolor);
    bus.send(Event::Highlighted {
        path: req.path,
        theme: req.theme,
        diff_hash: req.diff_hash,
        generation: req.generation,
        highlight: Arc::new(hl),
    })
    .map_err(|_| ())
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
                Ok(mut files) => {
                    // Refresh +/- counts so the tree stays current as files change.
                    git::numstat::fill_line_counts(&repo_root, base, &mut files);
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
