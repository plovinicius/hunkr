//! Application state — the single source of truth.
//!
//! Per the architecture, `App` is owned and mutated only on the UI thread, in
//! response to events. There are no locks and no shared mutable state. For the
//! Phase-A slice the diff is loaded synchronously on selection (lazy, per file);
//! M3 will move git work onto a background worker thread that feeds events.

use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::git::{self, diff::DiffBase};
use crate::model::{diff::FileDiff, file::ChangedFile, tree::FileTree};
use crate::render::viewport;

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
        let mut app = Self::with_files(repo_root, base, files);
        app.select_first_file();
        app.ensure_diff_loaded();
        Ok(app)
    }

    /// Construct an app from an already-resolved file list, without touching
    /// git. Used by [`App::new`] and by rendering tests (which inject synthetic
    /// files so the UI can be exercised without a repository).
    pub(crate) fn with_files(repo_root: PathBuf, base: DiffBase, files: Vec<ChangedFile>) -> Self {
        let tree = FileTree::build(&files);
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
            focus: Focus::Tree,
            should_quit: false,
            dirty: true,
            diff_height: 0,
            status_msg: None,
            error: None,
        }
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
        for (i, &node) in self.tree.visible.iter().enumerate() {
            if self.tree.nodes[node].file.is_some() {
                self.tree_cursor = i;
                return;
            }
        }
        self.tree_cursor = 0;
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
