//! The lightweight, always-resident description of a single changed file.
//!
//! This is what populates the file tree. It deliberately holds *no* diff text —
//! diffs are hydrated lazily on selection (see [`crate::git::diff`]). Only the
//! metadata needed to draw a tree row lives here.

use std::path::PathBuf;

/// How a file changed relative to the diff base (HEAD, or the empty tree when
/// the repo has no commits yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed { from: PathBuf },
    Untracked,
    TypeChange,
}

impl ChangeKind {
    /// Single-character status glyph shown in the tree (mirrors git's letters).
    pub fn glyph(&self) -> char {
        match self {
            ChangeKind::Added => 'A',
            ChangeKind::Modified => 'M',
            ChangeKind::Deleted => 'D',
            ChangeKind::Renamed { .. } => 'R',
            ChangeKind::Untracked => '?',
            ChangeKind::TypeChange => 'T',
        }
    }

    /// Untracked files have no blob in the base, so they need a `--no-index`
    /// diff rather than `git diff <base>`.
    pub fn is_untracked(&self) -> bool {
        matches!(self, ChangeKind::Untracked)
    }
}

/// One entry in the changed-file list. Cheap to clone and keep around.
#[derive(Debug, Clone)]
pub struct ChangedFile {
    /// Repo-relative path (new path for renames).
    pub path: PathBuf,
    pub kind: ChangeKind,
    /// Populated once the file's diff is hydrated; 0 until then.
    pub additions: u32,
    pub deletions: u32,
}

impl ChangedFile {
    pub fn new(path: PathBuf, kind: ChangeKind) -> Self {
        Self {
            path,
            kind,
            additions: 0,
            deletions: 0,
        }
    }
}
