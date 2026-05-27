//! Application state — the single source of truth.
//!
//! Per the architecture, `App` is owned and mutated only on the UI thread, in
//! response to events. There are no locks and no shared mutable state. For the
//! Phase-A slice the diff is loaded synchronously on selection (lazy, per file);
//! M3 will move git work onto a background worker thread that feeds events.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::git::{self, diff::DiffBase};
use crate::model::review::{self, ReviewStatus};
use crate::model::{diff::FileDiff, file::ChangedFile, snapshot::GitSnapshot, tree::FileTree};
use crate::persist::ReviewStore;
use crate::render::viewport;

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Diff,
}

/// One rendered row of the diff panel: either a hunk header or a body line.
/// Built once on hydration; the panel slices a window out of this list.
#[derive(Clone, Copy)]
pub enum RowRef {
    Header(usize),
    Line(usize, usize),
}

pub struct App {
    pub repo_root: PathBuf,
    pub base: DiffBase,

    pub files: Vec<ChangedFile>,
    pub tree: FileTree,
    /// Index into `tree.visible`.
    pub tree_cursor: usize,

    /// Hydrated diff for the currently selected file (if any).
    pub diff: Option<FileDiff>,
    /// Flattened render rows for `diff`.
    pub diff_rows: Vec<RowRef>,
    /// Row index where each hunk's header sits (parallel to `diff.hunks`).
    pub hunk_starts: Vec<usize>,
    /// Which file index `diff` belongs to, to avoid redundant reloads.
    pub diff_file: Option<usize>,

    pub scroll: usize,
    pub current_hunk: usize,

    /// Persisted reviewed-state, keyed by path → diff hash at review time.
    pub review: ReviewStore,
    /// Current diff hashes for reviewed files (to detect "changed after
    /// review"). Maintained on mark and on each refresh.
    pub hashes: HashMap<PathBuf, u64>,

    pub focus: Focus,
    pub should_quit: bool,
    pub dirty: bool,

    /// Diff viewport height in rows, updated by the renderer each frame.
    pub diff_height: usize,

    pub status_msg: Option<String>,
    pub error: Option<String>,
}

impl App {
    pub fn new(repo_root: PathBuf) -> anyhow::Result<Self> {
        let base = git::diff::detect_base(&repo_root);
        let files = git::status::changed_files(&repo_root)?;
        let git_dir = git::repo::git_dir(&repo_root)?;
        let review = ReviewStore::load(&git_dir);
        // Compute current hashes for the (typically few) reviewed files up front
        // so their ✓/↻ status is correct on the very first paint.
        let hashes = git::diff::diff_hashes_for(
            &repo_root,
            base,
            files.iter().filter(|f| review.get(&f.path).is_some()),
        );

        let mut app = Self::with_files(repo_root, base, files);
        app.review = review;
        app.hashes = hashes;
        app.select_first_file();
        app.ensure_diff_loaded();
        Ok(app)
    }

    /// Construct an app from an already-resolved file list, without touching
    /// git. Used by [`App::new`] and by rendering/reconcile tests (which inject
    /// synthetic files so the app can be exercised without a repository).
    pub(crate) fn with_files(repo_root: PathBuf, base: DiffBase, files: Vec<ChangedFile>) -> Self {
        let tree = FileTree::build(&files);
        let review = ReviewStore::empty(&repo_root.join(".git"));
        App {
            repo_root,
            base,
            files,
            tree,
            tree_cursor: 0,
            diff: None,
            diff_rows: Vec::new(),
            hunk_starts: Vec::new(),
            diff_file: None,
            scroll: 0,
            current_hunk: 0,
            review,
            hashes: HashMap::new(),
            focus: Focus::Tree,
            should_quit: false,
            dirty: true,
            diff_height: 0,
            status_msg: None,
            error: None,
        }
    }

    // ── reviewed state ─────────────────────────────────────────────────────

    /// Derived review status for a file (by index into `files`).
    pub fn review_status(&self, file_idx: usize) -> ReviewStatus {
        let path = &self.files[file_idx].path;
        review::derive(self.review.get(path), self.hashes.get(path).copied())
    }

    /// Paths that currently have a review record (asked of the git worker so it
    /// re-hashes only those on refresh).
    pub fn reviewed_paths(&self) -> Vec<PathBuf> {
        self.review.reviewed_paths()
    }

    /// Diff hash of the currently hydrated file, if any.
    fn selected_hash(&self) -> Option<u64> {
        self.diff.as_ref().map(|d| git::diff::hash_text(&d.text))
    }

    fn mark_reviewed(&mut self) {
        let Some(fi) = self.current_file_index() else {
            return;
        };
        let path = self.files[fi].path.clone();
        let Some(hash) = self.selected_hash() else {
            self.error = Some("no diff to mark reviewed".into());
            return;
        };
        match self.review.mark(path.clone(), hash, unix_now()) {
            Ok(()) => {
                self.hashes.insert(path, hash);
                self.status_msg = Some("marked reviewed".into());
            }
            Err(e) => self.error = Some(format!("could not save review state: {e}")),
        }
        self.dirty = true;
    }

    fn unmark_reviewed(&mut self) {
        let Some(fi) = self.current_file_index() else {
            return;
        };
        let path = self.files[fi].path.clone();
        match self.review.unmark(&path) {
            Ok(()) => {
                self.hashes.remove(&path);
                self.status_msg = Some("unmarked".into());
            }
            Err(e) => self.error = Some(format!("could not save review state: {e}")),
        }
        self.dirty = true;
    }

    // ── selection ────────────────────────────────────────────────────────

    fn node_at_cursor(&self) -> Option<usize> {
        self.tree.visible.get(self.tree_cursor).copied()
    }

    /// File index under the cursor, or `None` if the cursor is on a folder.
    pub fn current_file_index(&self) -> Option<usize> {
        self.tree.nodes[self.node_at_cursor()?].file
    }

    fn select_first_file(&mut self) {
        self.tree_cursor = self.first_file_cursor().unwrap_or(0);
    }

    /// Visible-list index of the first file leaf, if any.
    fn first_file_cursor(&self) -> Option<usize> {
        self.tree
            .visible
            .iter()
            .position(|&n| self.tree.nodes[n].file.is_some())
    }

    /// Visible-list index of the node holding `path`, if currently visible.
    fn cursor_for_path(&self, path: &Path) -> Option<usize> {
        self.tree.visible.iter().position(|&n| {
            self.tree.nodes[n]
                .file
                .is_some_and(|fi| self.files[fi].path == *path)
        })
    }

    // ── hot reload ─────────────────────────────────────────────────────────

    /// Apply a freshly computed snapshot, preserving as much of the user's
    /// place as possible: the selection stays on the same path (or snaps to the
    /// nearest file), and if the selected file's diff is byte-for-byte
    /// unchanged we keep the exact scroll position and current hunk.
    pub fn reconcile(&mut self, snapshot: GitSnapshot) {
        let prev_path = self
            .current_file_index()
            .map(|i| self.files[i].path.clone());
        let prev_text = self.diff.as_ref().map(|d| d.text.clone());
        let prev_scroll = self.scroll;
        let prev_hunk = self.current_hunk;

        // Adopt the worker's freshly computed hashes for reviewed files; this is
        // what flips a reviewed file to ↻ once it changes on disk.
        self.hashes = snapshot.hashes;
        self.apply_files(snapshot.files, prev_path.as_deref());

        // Force a reload of the (possibly new) selected file's diff.
        self.diff_file = None;
        self.ensure_diff_loaded();

        // Restore the view if we're on the same file and its diff is identical.
        let same_path = matches!(
            (&prev_path, self.current_file_index()),
            (Some(p), Some(i)) if *p == self.files[i].path
        );
        if same_path
            && let (Some(prev), Some(cur)) = (&prev_text, &self.diff)
            && prev.as_ref() == cur.text.as_ref()
        {
            self.scroll =
                viewport::clamp_offset(prev_scroll, self.diff_height.max(1), self.diff_rows.len());
            self.current_hunk = prev_hunk.min(self.hunk_starts.len().saturating_sub(1));
        }
        self.dirty = true;
    }

    /// Rebuild the file list + tree and restore the cursor to `keep_path` (or
    /// the first file). Pure: no git, no diff hydration — separated so it can be
    /// unit-tested without a repository.
    fn apply_files(&mut self, files: Vec<ChangedFile>, keep_path: Option<&Path>) {
        self.files = files;
        self.tree = FileTree::build(&self.files);
        self.tree_cursor = keep_path
            .and_then(|p| self.cursor_for_path(p))
            .or_else(|| self.first_file_cursor())
            .unwrap_or(0);
    }

    /// Load (or reuse) the diff for the file under the cursor.
    pub fn ensure_diff_loaded(&mut self) {
        let Some(fi) = self.current_file_index() else {
            self.diff = None;
            self.diff_rows.clear();
            self.hunk_starts.clear();
            self.diff_file = None;
            return;
        };
        if self.diff_file == Some(fi) {
            return;
        }
        match git::diff::fetch_file_diff(&self.repo_root, self.base, &self.files[fi]) {
            Ok(fd) => {
                self.files[fi].additions = fd.additions();
                self.files[fi].deletions = fd.deletions();
                self.rebuild_rows(&fd);
                self.diff = Some(fd);
                self.diff_file = Some(fi);
                self.scroll = 0;
                self.current_hunk = 0;
                self.error = None;
            }
            Err(e) => {
                self.error = Some(format!("diff failed: {e}"));
                self.diff = None;
                self.diff_rows.clear();
                self.hunk_starts.clear();
                self.diff_file = Some(fi);
            }
        }
    }

    /// Inject an already-parsed diff (rendering tests use this to exercise the
    /// diff panel without shelling out to git).
    #[cfg(test)]
    pub(crate) fn set_diff_for_test(&mut self, fd: FileDiff) {
        self.rebuild_rows(&fd);
        self.diff = Some(fd);
        self.diff_file = Some(0);
        self.focus = Focus::Diff;
    }

    fn rebuild_rows(&mut self, fd: &FileDiff) {
        let mut rows = Vec::new();
        let mut starts = Vec::with_capacity(fd.hunks.len());
        for (h, hunk) in fd.hunks.iter().enumerate() {
            starts.push(rows.len());
            rows.push(RowRef::Header(h));
            for l in 0..hunk.lines.len() {
                rows.push(RowRef::Line(h, l));
            }
        }
        self.diff_rows = rows;
        self.hunk_starts = starts;
    }

    // ── navigation ───────────────────────────────────────────────────────

    fn on_cursor_move(&mut self) {
        self.ensure_diff_loaded();
        self.dirty = true;
    }

    fn cursor_down(&mut self) {
        if self.tree_cursor + 1 < self.tree.visible.len() {
            self.tree_cursor += 1;
            self.on_cursor_move();
        }
    }

    fn cursor_up(&mut self) {
        if self.tree_cursor > 0 {
            self.tree_cursor -= 1;
            self.on_cursor_move();
        }
    }

    fn next_file(&mut self) {
        let mut i = self.tree_cursor + 1;
        while i < self.tree.visible.len() {
            if self.tree.nodes[self.tree.visible[i]].file.is_some() {
                self.tree_cursor = i;
                self.on_cursor_move();
                return;
            }
            i += 1;
        }
    }

    fn prev_file(&mut self) {
        let mut i = self.tree_cursor;
        while i > 0 {
            i -= 1;
            if self.tree.nodes[self.tree.visible[i]].file.is_some() {
                self.tree_cursor = i;
                self.on_cursor_move();
                return;
            }
        }
    }

    /// Enter: toggle a folder, or focus the diff panel on a file.
    fn activate(&mut self) {
        let Some(node) = self.node_at_cursor() else {
            return;
        };
        if self.tree.is_dir(node) {
            self.tree.toggle(node);
            if self.tree_cursor >= self.tree.visible.len() {
                self.tree_cursor = self.tree.visible.len().saturating_sub(1);
            }
            self.ensure_diff_loaded();
        } else {
            self.focus = Focus::Diff;
        }
        self.dirty = true;
    }

    fn max_scroll(&self) -> usize {
        self.diff_rows.len().saturating_sub(self.diff_height.max(1))
    }

    fn scroll_by(&mut self, delta: isize) {
        let target = if delta < 0 {
            self.scroll.saturating_sub((-delta) as usize)
        } else {
            self.scroll + delta as usize
        };
        self.scroll = viewport::clamp_offset(target, self.diff_height.max(1), self.diff_rows.len());
        self.sync_hunk_from_scroll();
        self.dirty = true;
    }

    fn next_hunk(&mut self) {
        if self.hunk_starts.is_empty() {
            return;
        }
        self.current_hunk = (self.current_hunk + 1).min(self.hunk_starts.len() - 1);
        self.scroll_to_current_hunk();
    }

    fn prev_hunk(&mut self) {
        if self.hunk_starts.is_empty() {
            return;
        }
        self.current_hunk = self.current_hunk.saturating_sub(1);
        self.scroll_to_current_hunk();
    }

    fn scroll_to_current_hunk(&mut self) {
        if let Some(&row) = self.hunk_starts.get(self.current_hunk) {
            self.scroll =
                viewport::clamp_offset(row, self.diff_height.max(1), self.diff_rows.len());
        }
        self.dirty = true;
    }

    /// Keep `current_hunk` in sync after free scrolling: the active hunk is the
    /// last one whose header is at or above the top of the viewport.
    fn sync_hunk_from_scroll(&mut self) {
        let mut h = 0;
        for (i, &start) in self.hunk_starts.iter().enumerate() {
            if start <= self.scroll {
                h = i;
            } else {
                break;
            }
        }
        self.current_hunk = h;
    }

    // ── input ────────────────────────────────────────────────────────────

    pub fn on_key(&mut self, key: KeyEvent) {
        // Ignore key-release events (Windows / kitty protocol emit them).
        if key.kind == KeyEventKind::Release {
            return;
        }
        self.status_msg = None;

        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => self.should_quit = true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.should_quit = true,
            (KeyCode::Tab, _) => {
                self.focus = match self.focus {
                    Focus::Tree => Focus::Diff,
                    Focus::Diff => Focus::Tree,
                };
                self.dirty = true;
            }
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => match self.focus {
                Focus::Tree => self.cursor_down(),
                Focus::Diff => self.scroll_by(1),
            },
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => match self.focus {
                Focus::Tree => self.cursor_up(),
                Focus::Diff => self.scroll_by(-1),
            },
            (KeyCode::Char('n'), _) => self.next_hunk(),
            (KeyCode::Char('p'), _) => self.prev_hunk(),
            (KeyCode::Char('r'), _) => self.mark_reviewed(),
            (KeyCode::Char('u'), _) => self.unmark_reviewed(),
            (KeyCode::Char(']'), _) => self.next_file(),
            (KeyCode::Char('['), _) => self.prev_file(),
            (KeyCode::Char('g'), _) => {
                self.scroll = 0;
                self.sync_hunk_from_scroll();
                self.dirty = true;
            }
            (KeyCode::Char('G'), _) => {
                self.scroll = self.max_scroll();
                self.sync_hunk_from_scroll();
                self.dirty = true;
            }
            (KeyCode::PageDown, _) => self.scroll_by(self.diff_height.max(1) as isize),
            (KeyCode::PageUp, _) => self.scroll_by(-(self.diff_height.max(1) as isize)),
            (KeyCode::Enter, _) => self.activate(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::file::ChangeKind;

    fn file(p: &str) -> ChangedFile {
        ChangedFile::new(PathBuf::from(p), ChangeKind::Modified)
    }

    fn app(files: Vec<ChangedFile>) -> App {
        App::with_files(PathBuf::from("/repo"), DiffBase::Head, files)
    }

    #[test]
    fn apply_files_keeps_cursor_on_same_path() {
        let mut a = app(vec![file("a.rs"), file("src/b.rs")]);
        a.tree_cursor = a.cursor_for_path(Path::new("src/b.rs")).unwrap();

        // A new file appears; the cursor should still sit on src/b.rs.
        a.apply_files(
            vec![file("a.rs"), file("src/b.rs"), file("src/c.rs")],
            Some(Path::new("src/b.rs")),
        );

        let cur = a.current_file_index().unwrap();
        assert_eq!(a.files[cur].path, PathBuf::from("src/b.rs"));
    }

    #[test]
    fn apply_files_snaps_to_first_when_path_gone() {
        let mut a = app(vec![file("a.rs"), file("b.rs")]);
        a.tree_cursor = a.cursor_for_path(Path::new("b.rs")).unwrap();

        // The selected file disappeared; fall back to the first file.
        a.apply_files(vec![file("a.rs")], Some(Path::new("b.rs")));

        let cur = a.current_file_index().unwrap();
        assert_eq!(a.files[cur].path, PathBuf::from("a.rs"));
    }
}
