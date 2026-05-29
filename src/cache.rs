//! A small LRU cache of parsed diffs, keyed by a cheap file signature.
//!
//! Re-selecting a file you've already viewed reuses its parsed [`FileDiff`]
//! (an `Arc` clone) instead of shelling out to git and re-parsing. The cache is
//! bounded so memory stays flat even across a long session in a huge repo.
//!
//! The signature is `(mtime, size)` of the working-tree file. It's only trusted
//! for plain navigation; hot-reload deliberately bypasses the cache (see
//! `App::ensure_diff_loaded`) so a fresh-on-disk change can never be masked by a
//! coarse-resolution mtime.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::model::diff::FileDiff;

pub type Signature = (SystemTime, u64);

/// `(mtime, size)` of `rel` under `repo_root`, or `None` if it can't be stat'd
/// (e.g. a deleted file), in which case the diff is simply not cached.
pub fn file_signature(repo_root: &Path, rel: &Path) -> Option<Signature> {
    let meta = std::fs::metadata(repo_root.join(rel)).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

struct Entry {
    sig: Signature,
    diff: Arc<FileDiff>,
}

pub struct DiffCache {
    entries: HashMap<PathBuf, Entry>,
    /// LRU order, least-recently-used at the front.
    order: VecDeque<PathBuf>,
    cap: usize,
}

impl DiffCache {
    pub fn new(cap: usize) -> Self {
        DiffCache {
            entries: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    /// Return the cached diff for `path` if present and its signature matches.
    pub fn get(&mut self, path: &Path, sig: Signature) -> Option<Arc<FileDiff>> {
        let hit = self
            .entries
            .get(path)
            .filter(|e| e.sig == sig)?
            .diff
            .clone();
        self.touch(path);
        Some(hit)
    }

    /// Insert/replace the diff for `path`, evicting the LRU entry if over cap.
    pub fn put(&mut self, path: PathBuf, sig: Signature, diff: Arc<FileDiff>) {
        if self.entries.contains_key(&path) {
            self.touch(&path);
        } else {
            self.order.push_back(path.clone());
            while self.order.len() > self.cap {
                if let Some(evict) = self.order.pop_front() {
                    self.entries.remove(&evict);
                }
            }
        }
        self.entries.insert(path, Entry { sig, diff });
    }

    fn touch(&mut self, path: &Path) {
        if let Some(pos) = self.order.iter().position(|p| p == path)
            && let Some(p) = self.order.remove(pos)
        {
            self.order.push_back(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn diff() -> Arc<FileDiff> {
        Arc::new(FileDiff {
            path: PathBuf::from("x"),
            text: Arc::from(""),
            chunks: Vec::new(),
            is_binary: false,
        })
    }

    fn sig(n: u64) -> Signature {
        (SystemTime::UNIX_EPOCH, n)
    }

    #[test]
    fn hit_only_on_matching_signature() {
        let mut c = DiffCache::new(4);
        let p = PathBuf::from("a");
        c.put(p.clone(), sig(1), diff());
        assert!(c.get(&p, sig(1)).is_some());
        // Different signature (file changed) → miss.
        assert!(c.get(&p, sig(2)).is_none());
    }

    #[test]
    fn evicts_least_recently_used() {
        let mut c = DiffCache::new(2);
        let (a, b, d) = (PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("d"));
        c.put(a.clone(), sig(1), diff());
        c.put(b.clone(), sig(1), diff());
        // Touch `a` so `b` becomes the LRU.
        assert!(c.get(&a, sig(1)).is_some());
        c.put(d.clone(), sig(1), diff());
        assert!(c.get(&b, sig(1)).is_none(), "b should have been evicted");
        assert!(c.get(&a, sig(1)).is_some());
        assert!(c.get(&d, sig(1)).is_some());
    }
}
