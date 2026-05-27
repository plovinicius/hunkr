//! Persistence of reviewed-state to `.git/hunkr/review.json`.
//!
//! Living inside the git directory means it's never tracked and is naturally
//! scoped per clone/worktree. The on-disk form is versioned (`schema`) for
//! forward compatibility. The file is tiny, so we just rewrite it on each
//! change.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::review::ReviewRecord;

const SCHEMA: u32 = 1;

/// On-disk shape. Keys are repo-relative path strings (JSON object keys must be
/// strings); the in-memory store uses `PathBuf`.
#[derive(Serialize, Deserialize, Default)]
struct Persisted {
    schema: u32,
    files: HashMap<String, ReviewRecord>,
}

/// In-memory reviewed-state, backed by a JSON file under the git directory.
pub struct ReviewStore {
    file: PathBuf,
    records: HashMap<PathBuf, ReviewRecord>,
}

impl ReviewStore {
    fn file_for(git_dir: &Path) -> PathBuf {
        git_dir.join("hunkr").join("review.json")
    }

    /// An empty store anchored at `git_dir` without reading from disk.
    pub fn empty(git_dir: &Path) -> Self {
        ReviewStore {
            file: Self::file_for(git_dir),
            records: HashMap::new(),
        }
    }

    /// Load the store for `git_dir` (e.g. `/repo/.git`). A missing or unreadable
    /// file yields an empty store rather than an error — review state is
    /// best-effort and must never block startup.
    pub fn load(git_dir: &Path) -> Self {
        let file = Self::file_for(git_dir);
        let records = fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Persisted>(&bytes).ok())
            .map(|p| {
                p.files
                    .into_iter()
                    .map(|(k, v)| (PathBuf::from(k), v))
                    .collect()
            })
            .unwrap_or_default();
        ReviewStore { file, records }
    }

    pub fn get(&self, path: &Path) -> Option<&ReviewRecord> {
        self.records.get(path)
    }

    /// Paths with a review record (used to ask the worker which files to re-hash).
    pub fn reviewed_paths(&self) -> Vec<PathBuf> {
        self.records.keys().cloned().collect()
    }

    /// Mark `path` reviewed at diff hash `hash`, and persist.
    pub fn mark(&mut self, path: PathBuf, hash: u64, now: i64) -> Result<()> {
        self.records.insert(
            path,
            ReviewRecord {
                reviewed_hash: hash,
                reviewed_at: now,
            },
        );
        self.save()
    }

    /// Remove any review record for `path`, and persist.
    pub fn unmark(&mut self, path: &Path) -> Result<()> {
        self.records.remove(path);
        self.save()
    }

    fn save(&self) -> Result<()> {
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("could not create {}", dir.display()))?;
        }
        let persisted = Persisted {
            schema: SCHEMA,
            files: self
                .records
                .iter()
                .map(|(k, v)| (k.to_string_lossy().into_owned(), v.clone()))
                .collect(),
        };
        let json = serde_json::to_vec_pretty(&persisted)?;
        fs::write(&self.file, json)
            .with_context(|| format!("could not write {}", self.file.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("hunkr-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        let mut store = ReviewStore::load(&dir);
        assert!(store.reviewed_paths().is_empty());
        store.mark(PathBuf::from("src/foo.rs"), 42, 1000).unwrap();

        // Reloading from the same git dir sees the persisted record.
        let reloaded = ReviewStore::load(&dir);
        let rec = reloaded.get(Path::new("src/foo.rs")).unwrap();
        assert_eq!(rec.reviewed_hash, 42);
        assert_eq!(rec.reviewed_at, 1000);

        // Unmark clears it on disk too.
        let mut store = reloaded;
        store.unmark(Path::new("src/foo.rs")).unwrap();
        assert!(
            ReviewStore::load(&dir)
                .get(Path::new("src/foo.rs"))
                .is_none()
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
