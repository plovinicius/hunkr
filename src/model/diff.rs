//! Structured, byte-range-backed representation of a single file's diff.
//!
//! Memory strategy: the entire `git diff` output for the file is stored **once**
//! as an `Arc<str>`. Every [`DiffLine`] and hunk header is just a `Range<usize>`
//! into that backing string — no per-line allocation. Rendering slices the
//! backing text on demand for the visible window only (see `ui::diff_panel`).

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Del,
    /// The `\ No newline at end of file` marker line.
    NoNewline,
}

/// A single rendered line of a hunk. `text` excludes the leading `+`/`-`/` `
/// marker (we draw our own gutter), except for [`LineKind::NoNewline`].
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: Range<usize>,
}

/// A `@@ ... @@` hunk and its lines.
#[derive(Debug, Clone)]
pub struct Hunk {
    /// Range of the `@@ -a,b +c,d @@ ...` header line in the backing text.
    pub header: Range<usize>,
    pub lines: Vec<DiffLine>,
}

/// A fully hydrated, parsed diff for one file.
#[derive(Debug, Clone)]
pub struct FileDiff {
    pub path: PathBuf,
    /// The complete `git diff` output, owned once.
    pub text: Arc<str>,
    pub hunks: Vec<Hunk>,
    pub is_binary: bool,
}

impl FileDiff {
    /// Slice the backing text for a stored range.
    pub fn slice(&self, r: &Range<usize>) -> &str {
        &self.text[r.clone()]
    }

    pub fn additions(&self) -> u32 {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == LineKind::Add)
            .count() as u32
    }

    pub fn deletions(&self) -> u32 {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == LineKind::Del)
            .count() as u32
    }
}
