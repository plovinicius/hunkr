//! Application state — the single source of truth.
//!
//! Per the architecture, `App` is owned and mutated only on the UI thread, in
//! response to events. There are no locks and no shared mutable state. For the
//! Phase-A slice the diff is loaded synchronously on selection (lazy, per file);
//! M3 will move git work onto a background worker thread that feeds events.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::cache::DiffCache;
use crate::git::{self, diff::DiffBase};
use crate::glyphs::Glyphs;
use crate::model::review::{self, ReviewStatus};
use crate::model::{
    diff::{FileDiff, Chunk, LineKind},
    file::{ChangeKind, ChangedFile},
    snapshot::GitSnapshot,
    tree::FileTree,
};
use crate::persist::ReviewStore;
use crate::render::viewport;

/// Max parsed diffs kept hydrated in the LRU cache.
const DIFF_CACHE_CAP: usize = 128;

/// Rows the diff scrolls per mouse-wheel notch.
const MOUSE_SCROLL_LINES: isize = 3;

/// Default width of the file-tree sidebar, in columns.
const DEFAULT_TREE_WIDTH: u16 = 44;
/// Lower bound on the sidebar width — narrow enough to be useful, wide enough
/// to still fit a status glyph + a short filename.
pub const MIN_TREE_WIDTH: u16 = 16;
/// Lower bound on the diff panel width; the sidebar may not grow past
/// `body_width - MIN_DIFF_WIDTH`.
pub const MIN_DIFF_WIDTH: u16 = 20;
/// Columns the sidebar grows/shrinks per `>` / `<` keypress.
const TREE_RESIZE_STEP: u16 = 2;

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

/// Top-level input mode. Determines how keys are interpreted and which
/// overlays are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Editing the file filter query.
    Filter,
    /// Help overlay is open.
    Help,
}

/// A request to open a file in `$EDITOR`, consumed by the run loop (which owns
/// the terminal and can suspend/restore it around the editor).
#[derive(Debug, Clone)]
pub struct EditorRequest {
    pub path: PathBuf,
    pub line: u32,
}

/// How the diff panel lays out a file's changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Stacked unified diff (default).
    Unified,
    /// Old version on the left, new version on the right.
    SideBySide,
}

/// One rendered row of the unified diff: a chunk header or a body line
/// `(chunk, line)`. Built once on hydration; the panel slices a window out.
#[derive(Clone, Copy)]
pub enum RowRef {
    Header(usize),
    Line(usize, usize),
}

/// One rendered row of the side-by-side diff. `Pair` holds the old-side and
/// new-side line references `(chunk, line)`; either may be absent when one side
/// has no corresponding line (a pure add or delete).
#[derive(Clone, Copy)]
pub enum SideRow {
    Header(usize),
    Pair {
        left: Option<(usize, usize)>,
        right: Option<(usize, usize)>,
    },
}

pub struct App {
    pub repo_root: PathBuf,
    pub base: DiffBase,

    pub files: Vec<ChangedFile>,
    pub tree: FileTree,
    /// Index into `tree.visible`.
    pub tree_cursor: usize,

    /// Hydrated diff for the currently selected file (if any). `Arc` so the
    /// cache and the app can share one parsed copy cheaply.
    pub diff: Option<Arc<FileDiff>>,
    /// Flattened render rows for `diff` (unified view).
    pub diff_rows: Vec<RowRef>,
    /// Row index where each chunk's header sits in `diff_rows`.
    pub chunk_starts: Vec<usize>,
    /// Flattened render rows for the side-by-side view.
    pub side_rows: Vec<SideRow>,
    /// Row index where each chunk's header sits in `side_rows`.
    pub side_chunk_starts: Vec<usize>,
    /// Which file index `diff` belongs to, to avoid redundant reloads.
    pub diff_file: Option<usize>,
    /// LRU cache of parsed diffs, keyed by file signature.
    cache: DiffCache,

    pub scroll: usize,
    pub current_chunk: usize,

    /// Persisted reviewed-state, keyed by path → diff hash at review time.
    pub review: ReviewStore,
    /// Current diff hashes for reviewed files (to detect "changed after
    /// review"). Maintained on mark and on each refresh.
    pub hashes: HashMap<PathBuf, u64>,

    pub focus: Focus,
    pub mode: Mode,
    pub view: ViewMode,
    /// Glyph set used for rendering (Unicode by default, ASCII with --ascii).
    pub glyphs: Glyphs,
    /// Active file-filter query (empty = no filter).
    pub filter: String,
    pub should_quit: bool,
    pub dirty: bool,

    /// Diff viewport height in rows, updated by the renderer each frame.
    pub diff_height: usize,
    /// Left column (x origin) of the diff panel, updated by the renderer each
    /// frame, so mouse-wheel events can be routed to the panel under the cursor.
    pub diff_x: u16,
    /// Width of the file-tree sidebar in columns. Adjustable with `<` / `>` and
    /// by dragging the divider with the mouse; the renderer clamps the visible
    /// layout to fit the current terminal width.
    pub tree_width: u16,
    /// Full body width (terminal width) recorded by the renderer each frame, so
    /// resize handlers can clamp the sidebar against the current terminal size.
    pub body_width: u16,
    /// True while the user is holding the left mouse button after grabbing the
    /// tree/diff divider — subsequent drag events resize the sidebar.
    dragging_divider: bool,

    pub status_msg: Option<String>,
    pub error: Option<String>,

    /// Set when the user asks to open the current file in `$EDITOR`; the run
    /// loop takes and fulfils it.
    pub pending_editor: Option<EditorRequest>,
}

impl App {
    pub fn new(repo_root: PathBuf) -> anyhow::Result<Self> {
        let base = git::diff::detect_base(&repo_root);
        let files = git::status::changed_files(&repo_root)?;
        let git_dir = git::repo::git_dir(&repo_root)?;
        let review = ReviewStore::load(&git_dir);
        // Compute current hashes for the (typically few) reviewed files up front
        // so their ✓ status is correct on the very first paint.
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
            chunk_starts: Vec::new(),
            side_rows: Vec::new(),
            side_chunk_starts: Vec::new(),
            diff_file: None,
            cache: DiffCache::new(DIFF_CACHE_CAP),
            scroll: 0,
            current_chunk: 0,
            review,
            hashes: HashMap::new(),
            focus: Focus::Tree,
            mode: Mode::Normal,
            view: ViewMode::Unified,
            glyphs: Glyphs::unicode(),
            filter: String::new(),
            should_quit: false,
            dirty: true,
            diff_height: 0,
            diff_x: 0,
            tree_width: DEFAULT_TREE_WIDTH,
            body_width: 0,
            dragging_divider: false,
            status_msg: None,
            error: None,
            pending_editor: None,
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

    /// Toggle the reviewed state for the file under the cursor. A reviewed file
    /// becomes unreviewed; otherwise the current diff hash is recorded as the
    /// reviewed hash (which also re-affirms a file that changed since last
    /// review).
    fn toggle_reviewed(&mut self) {
        let Some(fi) = self.current_file_index() else {
            return;
        };
        let path = self.files[fi].path.clone();
        let result = if self.review_status(fi) == ReviewStatus::Reviewed {
            self.review.unmark(&path).map(|_| {
                self.hashes.remove(&path);
            })
        } else {
            let Some(hash) = self.selected_hash() else {
                self.error = Some("no diff to mark reviewed".into());
                return;
            };
            self.review.mark(path.clone(), hash, unix_now()).map(|_| {
                self.hashes.insert(path, hash);
            })
        };
        if let Err(e) = result {
            self.error = Some(format!("could not save review state: {e}"));
        }
        self.dirty = true;
    }

    // ── AI reference ───────────────────────────────────────────────────────

    /// Copy an AI-ready reference for the current chunk to the clipboard.
    fn copy_reference(&mut self) {
        let text = match &self.diff {
            Some(fd) if !fd.chunks.is_empty() => {
                let h = self.current_chunk.min(fd.chunks.len() - 1);
                crate::reference::build_chunk_reference(fd, h)
            }
            _ => {
                self.error = Some("no chunk to copy".into());
                self.dirty = true;
                return;
            }
        };
        match crate::reference::copy(&text) {
            Ok(method) => self.status_msg = Some(format!("copied reference via {method}")),
            Err(e) => self.error = Some(format!("copy failed: {e}")),
        }
        self.dirty = true;
    }

    // ── open in editor ─────────────────────────────────────────────────────

    /// Request that the run loop open the selected file in `$EDITOR` at the
    /// line currently at the top of the diff viewport.
    fn open_in_editor(&mut self) {
        let Some(fi) = self.current_file_index() else {
            return;
        };
        if matches!(self.files[fi].kind, ChangeKind::Deleted) {
            self.error = Some("file was deleted; nothing to open".into());
            self.dirty = true;
            return;
        }
        let line = self.current_line().unwrap_or(1);
        self.pending_editor = Some(EditorRequest {
            path: self.files[fi].path.clone(),
            line,
        });
    }

    /// Take a pending editor request, if any (called by the run loop).
    pub fn take_editor_request(&mut self) -> Option<EditorRequest> {
        self.pending_editor.take()
    }

    /// The new-file line number at the top of the diff viewport (falling back to
    /// the old-file number for deletions), used as the editor's target line.
    fn current_line(&self) -> Option<u32> {
        let fd = self.diff.as_ref()?;
        match self.view {
            ViewMode::Unified => match self.diff_rows.get(self.scroll)? {
                RowRef::Header(h) => first_line_no(&fd.chunks[*h]),
                RowRef::Line(h, l) => {
                    let dl = &fd.chunks[*h].lines[*l];
                    dl.new_no.or(dl.old_no)
                }
            },
            ViewMode::SideBySide => match self.side_rows.get(self.scroll)? {
                SideRow::Header(h) => first_line_no(&fd.chunks[*h]),
                SideRow::Pair { left, right } => right
                    .and_then(|(h, l)| fd.chunks[h].lines[l].new_no)
                    .or_else(|| left.and_then(|(h, l)| fd.chunks[h].lines[l].old_no)),
            },
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
    /// unchanged we keep the exact scroll position and current chunk.
    pub fn reconcile(&mut self, snapshot: GitSnapshot) {
        let prev_path = self
            .current_file_index()
            .map(|i| self.files[i].path.clone());
        let prev_text = self.diff.as_ref().map(|d| d.text.clone());
        let prev_scroll = self.scroll;
        let prev_chunk = self.current_chunk;

        // Adopt the worker's freshly computed hashes for reviewed files; this is
        // what flips a reviewed file back to unreviewed once it changes on disk.
        self.hashes = snapshot.hashes;
        self.apply_files(snapshot.files, prev_path.as_deref());

        // Force a fresh reload (bypassing the cache) of the selected file's diff.
        self.diff_file = None;
        self.load_diff(false);

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
            self.current_chunk = prev_chunk.min(self.chunk_starts.len().saturating_sub(1));
        }
        self.dirty = true;
    }

    /// Rebuild the file list + tree and restore the cursor to `keep_path` (or
    /// the first file). Pure: no git, no diff hydration — separated so it can be
    /// unit-tested without a repository.
    fn apply_files(&mut self, files: Vec<ChangedFile>, keep_path: Option<&Path>) {
        self.files = files;
        self.tree = FileTree::build(&self.files);
        self.recompute_view();
        self.tree_cursor = keep_path
            .and_then(|p| self.cursor_for_path(p))
            .or_else(|| self.first_file_cursor())
            .unwrap_or(0);
    }

    // ── filtering ──────────────────────────────────────────────────────────

    pub fn is_filtering(&self) -> bool {
        !self.filter.is_empty()
    }

    /// Recompute the tree's visible list. With an active filter, the tree
    /// collapses to a flat list of files whose path matches (case-insensitive);
    /// otherwise it's the normal hierarchical view.
    fn recompute_view(&mut self) {
        if self.filter.is_empty() {
            self.tree.recompute_visible();
        } else {
            let q = self.filter.to_lowercase();
            self.tree.visible = self
                .tree
                .nodes
                .iter()
                .enumerate()
                .filter_map(|(idx, node)| {
                    let fi = node.file?;
                    self.files[fi]
                        .path
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&q)
                        .then_some(idx)
                })
                .collect();
        }
        if self.tree_cursor >= self.tree.visible.len() {
            self.tree_cursor = self.tree.visible.len().saturating_sub(1);
        }
    }

    /// Load (or reuse) the diff for the file under the cursor, consulting the
    /// LRU cache.
    pub fn ensure_diff_loaded(&mut self) {
        self.load_diff(true);
    }

    /// Load the selected file's diff. `use_cache` is false on hot-reload so a
    /// fresh-on-disk change can't be masked by a stale (mtime, size) signature.
    fn load_diff(&mut self, use_cache: bool) {
        let Some(fi) = self.current_file_index() else {
            self.clear_diff();
            self.diff_file = None;
            return;
        };
        if self.diff_file == Some(fi) {
            return;
        }
        let path = self.files[fi].path.clone();
        let sig = crate::cache::file_signature(&self.repo_root, &path);

        if use_cache
            && let Some(sig) = sig
            && let Some(diff) = self.cache.get(&path, sig)
        {
            self.adopt_diff(fi, diff);
            return;
        }

        match git::diff::fetch_file_diff(&self.repo_root, self.base, &self.files[fi]) {
            Ok(fd) => {
                let diff = Arc::new(fd);
                if let Some(sig) = sig {
                    self.cache.put(path, sig, diff.clone());
                }
                self.adopt_diff(fi, diff);
            }
            Err(e) => {
                self.error = Some(format!("diff failed: {e}"));
                self.clear_diff();
                self.diff_file = Some(fi);
            }
        }
    }

    /// Install a (freshly loaded or cached) diff as the current one.
    fn adopt_diff(&mut self, fi: usize, diff: Arc<FileDiff>) {
        self.files[fi].additions = diff.additions();
        self.files[fi].deletions = diff.deletions();
        self.rebuild_rows(&diff);
        self.diff = Some(diff);
        self.diff_file = Some(fi);
        self.scroll = 0;
        self.current_chunk = 0;
        self.error = None;
    }

    fn clear_diff(&mut self) {
        self.diff = None;
        self.diff_rows.clear();
        self.chunk_starts.clear();
        self.side_rows.clear();
        self.side_chunk_starts.clear();
    }

    /// Inject an already-parsed diff (rendering tests use this to exercise the
    /// diff panel without shelling out to git).
    #[cfg(test)]
    pub(crate) fn set_diff_for_test(&mut self, fd: FileDiff) {
        let diff = Arc::new(fd);
        self.rebuild_rows(&diff);
        self.diff = Some(diff);
        self.diff_file = Some(0);
        self.focus = Focus::Diff;
    }

    fn rebuild_rows(&mut self, fd: &FileDiff) {
        // Unified rows: header followed by each body line.
        let mut rows = Vec::new();
        let mut starts = Vec::with_capacity(fd.chunks.len());
        for (h, chunk) in fd.chunks.iter().enumerate() {
            starts.push(rows.len());
            rows.push(RowRef::Header(h));
            for l in 0..chunk.lines.len() {
                rows.push(RowRef::Line(h, l));
            }
        }
        self.diff_rows = rows;
        self.chunk_starts = starts;

        let (side_rows, side_starts) = build_side_rows(fd);
        self.side_rows = side_rows;
        self.side_chunk_starts = side_starts;
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

    /// Number of render rows in the active view.
    fn active_row_count(&self) -> usize {
        match self.view {
            ViewMode::Unified => self.diff_rows.len(),
            ViewMode::SideBySide => self.side_rows.len(),
        }
    }

    /// Per-chunk header row indices for the active view.
    fn active_chunk_starts(&self) -> &[usize] {
        match self.view {
            ViewMode::Unified => &self.chunk_starts,
            ViewMode::SideBySide => &self.side_chunk_starts,
        }
    }

    /// Toggle unified ↔ side-by-side, keeping the current chunk in view.
    fn toggle_view(&mut self) {
        self.view = match self.view {
            ViewMode::Unified => ViewMode::SideBySide,
            ViewMode::SideBySide => ViewMode::Unified,
        };
        self.scroll_to_current_chunk();
        self.dirty = true;
    }

    fn max_scroll(&self) -> usize {
        self.active_row_count()
            .saturating_sub(self.diff_height.max(1))
    }

    fn scroll_by(&mut self, delta: isize) {
        let target = if delta < 0 {
            self.scroll.saturating_sub((-delta) as usize)
        } else {
            self.scroll + delta as usize
        };
        self.scroll =
            viewport::clamp_offset(target, self.diff_height.max(1), self.active_row_count());
        self.sync_chunk_from_scroll();
        self.dirty = true;
    }

    fn next_chunk(&mut self) {
        let n = self.active_chunk_starts().len();
        if n == 0 {
            return;
        }
        self.current_chunk = (self.current_chunk + 1).min(n - 1);
        self.scroll_to_current_chunk();
    }

    fn prev_chunk(&mut self) {
        if self.active_chunk_starts().is_empty() {
            return;
        }
        self.current_chunk = self.current_chunk.saturating_sub(1);
        self.scroll_to_current_chunk();
    }

    fn scroll_to_current_chunk(&mut self) {
        let row = self.active_chunk_starts().get(self.current_chunk).copied();
        if let Some(row) = row {
            self.scroll =
                viewport::clamp_offset(row, self.diff_height.max(1), self.active_row_count());
        }
        self.dirty = true;
    }

    /// Keep `current_chunk` in sync after free scrolling: the active chunk is the
    /// last one whose header is at or above the top of the viewport.
    fn sync_chunk_from_scroll(&mut self) {
        let scroll = self.scroll;
        let mut h = 0;
        for (i, &start) in self.active_chunk_starts().iter().enumerate() {
            if start <= scroll {
                h = i;
            } else {
                break;
            }
        }
        self.current_chunk = h;
    }

    // ── sidebar resize ─────────────────────────────────────────────────────

    /// Upper bound on the sidebar width given the current terminal size; falls
    /// back to the lower bound until the renderer has reported a body width.
    fn max_tree_width(&self) -> u16 {
        self.body_width
            .saturating_sub(MIN_DIFF_WIDTH)
            .max(MIN_TREE_WIDTH)
    }

    /// Set the sidebar width, clamped to `[MIN_TREE_WIDTH, max_tree_width()]`.
    fn set_tree_width(&mut self, w: u16) {
        let new = w.clamp(MIN_TREE_WIDTH, self.max_tree_width());
        if new != self.tree_width {
            self.tree_width = new;
            self.dirty = true;
        }
    }

    fn widen_tree(&mut self) {
        self.set_tree_width(self.tree_width.saturating_add(TREE_RESIZE_STEP));
    }

    fn narrow_tree(&mut self) {
        self.set_tree_width(self.tree_width.saturating_sub(TREE_RESIZE_STEP));
    }

    /// The two-column hit zone for the tree/diff divider. Either the tree's
    /// right border or the diff's left border counts as a grab.
    fn is_divider_column(&self, column: u16) -> bool {
        let left = self.tree_width.saturating_sub(1);
        column == left || column == self.tree_width
    }

    // ── input ────────────────────────────────────────────────────────────

    /// Route mouse events: a wheel notch scrolls the panel under the cursor,
    /// and a left-click on the tree/diff divider starts a drag-to-resize.
    pub fn on_mouse(&mut self, ev: MouseEvent) {
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.is_divider_column(ev.column) {
                    self.dragging_divider = true;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging_divider => {
                // Move the divider to the cursor: column X means the tree's
                // right border sits at X, so tree_width = X + 1.
                self.set_tree_width(ev.column.saturating_add(1));
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.dragging_divider = false;
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let down = matches!(ev.kind, MouseEventKind::ScrollDown);
                if ev.column >= self.diff_x {
                    self.scroll_by(if down {
                        MOUSE_SCROLL_LINES
                    } else {
                        -MOUSE_SCROLL_LINES
                    });
                } else if down {
                    self.cursor_down();
                } else {
                    self.cursor_up();
                }
            }
            _ => {}
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        // Ignore key-release events (Windows / kitty protocol emit them).
        if key.kind == KeyEventKind::Release {
            return;
        }
        match self.mode {
            Mode::Help => self.on_key_help(key),
            Mode::Filter => self.on_key_filter(key),
            Mode::Normal => self.on_key_normal(key),
        }
    }

    /// In the help overlay, any key closes it.
    fn on_key_help(&mut self, _key: KeyEvent) {
        self.mode = Mode::Normal;
        self.dirty = true;
    }

    /// Editing the filter query.
    fn on_key_filter(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.recompute_view();
                self.tree_cursor = 0;
                self.ensure_diff_loaded();
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => self.mode = Mode::Normal, // keep the filter applied
            KeyCode::Backspace => {
                self.filter.pop();
                self.refilter();
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.refilter();
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Re-apply the filter after an edit: rebuild the view, jump to the first
    /// match, and load its diff.
    fn refilter(&mut self) {
        self.recompute_view();
        self.tree_cursor = 0;
        self.ensure_diff_loaded();
    }

    fn on_key_normal(&mut self, key: KeyEvent) {
        self.status_msg = None;
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => self.should_quit = true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.should_quit = true,
            (KeyCode::Char('?'), _) => {
                self.mode = Mode::Help;
                self.dirty = true;
            }
            (KeyCode::Char('/'), _) => {
                self.mode = Mode::Filter;
                self.dirty = true;
            }
            (KeyCode::Esc, _) if self.is_filtering() => {
                self.filter.clear();
                self.recompute_view();
                self.tree_cursor = 0;
                self.ensure_diff_loaded();
                self.dirty = true;
            }
            (KeyCode::Tab, _) => {
                self.focus = match self.focus {
                    Focus::Tree => Focus::Diff,
                    Focus::Diff => Focus::Tree,
                };
                self.dirty = true;
            }
            // Shift+arrows page the diff, aliasing PageDown/PageUp. Matched
            // ahead of the plain-arrow arms below, whose `_` modifier would
            // otherwise swallow the Shift variant.
            (KeyCode::Down, KeyModifiers::SHIFT) | (KeyCode::PageDown, _) => {
                self.scroll_by(self.diff_height.max(1) as isize)
            }
            (KeyCode::Up, KeyModifiers::SHIFT) | (KeyCode::PageUp, _) => {
                self.scroll_by(-(self.diff_height.max(1) as isize))
            }
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => match self.focus {
                Focus::Tree => self.cursor_down(),
                Focus::Diff => self.scroll_by(1),
            },
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => match self.focus {
                Focus::Tree => self.cursor_up(),
                Focus::Diff => self.scroll_by(-1),
            },
            (KeyCode::Char('n'), _) => self.next_chunk(),
            (KeyCode::Char('p'), _) => self.prev_chunk(),
            (KeyCode::Char('s'), _) => self.toggle_view(),
            (KeyCode::Char('r'), _) => self.toggle_reviewed(),
            (KeyCode::Char('y'), _) => self.copy_reference(),
            (KeyCode::Char('e'), _) => self.open_in_editor(),
            (KeyCode::Char(']'), _) => self.next_file(),
            (KeyCode::Char('['), _) => self.prev_file(),
            (KeyCode::Char('<'), _) => self.narrow_tree(),
            (KeyCode::Char('>'), _) => self.widen_tree(),
            (KeyCode::Char('g'), _) => {
                self.scroll = 0;
                self.sync_chunk_from_scroll();
                self.dirty = true;
            }
            (KeyCode::Char('G'), _) => {
                self.scroll = self.max_scroll();
                self.sync_chunk_from_scroll();
                self.dirty = true;
            }
            (KeyCode::Enter, _) => self.activate(),
            _ => {}
        }
    }
}

/// The first meaningful line number of a chunk: the first line carrying a
/// new-file number, else the first old-file number, else `None`.
fn first_line_no(chunk: &Chunk) -> Option<u32> {
    chunk.lines
        .iter()
        .find_map(|l| l.new_no)
        .or_else(|| chunk.lines.iter().find_map(|l| l.old_no))
}

/// Transform a parsed diff into side-by-side rows. Within each chunk, context
/// lines appear on both sides; a run of deletions is paired row-for-row with
/// the run of additions that follows it (extra lines on either side get an
/// empty cell on the other). Returns the rows and each chunk header's row index.
fn build_side_rows(fd: &FileDiff) -> (Vec<SideRow>, Vec<usize>) {
    let mut rows = Vec::new();
    let mut starts = Vec::with_capacity(fd.chunks.len());

    for (h, chunk) in fd.chunks.iter().enumerate() {
        starts.push(rows.len());
        rows.push(SideRow::Header(h));

        let lines = &chunk.lines;
        let mut i = 0;
        while i < lines.len() {
            match lines[i].kind {
                LineKind::Context | LineKind::NoNewline => {
                    rows.push(SideRow::Pair {
                        left: Some((h, i)),
                        right: Some((h, i)),
                    });
                    i += 1;
                }
                LineKind::Del | LineKind::Add => {
                    let del_start = i;
                    while i < lines.len() && lines[i].kind == LineKind::Del {
                        i += 1;
                    }
                    let dels = del_start..i;
                    let add_start = i;
                    while i < lines.len() && lines[i].kind == LineKind::Add {
                        i += 1;
                    }
                    let adds = add_start..i;

                    let pairs = dels.len().max(adds.len());
                    for k in 0..pairs {
                        rows.push(SideRow::Pair {
                            left: dels.clone().nth(k).map(|l| (h, l)),
                            right: adds.clone().nth(k).map(|l| (h, l)),
                        });
                    }
                }
            }
        }
    }

    (rows, starts)
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

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn filter_narrows_to_matching_files_and_esc_clears() {
        let mut a = app(vec![
            file("src/foo.rs"),
            file("src/bar.rs"),
            file("README.md"),
        ]);

        a.on_key(key('/')); // enter filter mode
        assert_eq!(a.mode, Mode::Filter);
        a.on_key(key('b')); // query "b" → only src/bar.rs matches
        a.on_key(key('a'));
        a.on_key(key('r'));

        let visible_files: Vec<_> = a
            .tree
            .visible
            .iter()
            .filter_map(|&n| a.tree.nodes[n].file)
            .map(|fi| a.files[fi].path.clone())
            .collect();
        assert_eq!(visible_files, vec![PathBuf::from("src/bar.rs")]);

        // Esc cancels the filter and restores the full hierarchical view.
        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::Normal);
        assert!(!a.is_filtering());
        assert!(a.tree.visible.len() > 1);
    }

    #[test]
    fn side_rows_pair_deletions_with_additions() {
        use crate::git::diff::parse_unified;
        use std::sync::Arc;

        let raw = concat!(
            "@@ -1,4 +1,4 @@\n",
            " ctx\n",
            "-old1\n",
            "-old2\n",
            "+new1\n",
            "+new2\n",
            "+new3\n",
        );
        let fd = parse_unified(Arc::from(raw), PathBuf::from("x"));
        let (rows, starts) = build_side_rows(&fd);

        assert_eq!(starts, vec![0]);
        assert!(matches!(rows[0], SideRow::Header(0)));
        // ctx pair + max(2 deletions, 3 additions) = 4 pairs.
        let pairs = rows
            .iter()
            .filter(|r| matches!(r, SideRow::Pair { .. }))
            .count();
        assert_eq!(pairs, 4);
        // The 2 deletions pair with the first 2 additions; the 3rd addition has
        // an empty left cell.
        match rows[4] {
            SideRow::Pair { left, right } => {
                assert!(left.is_none(), "expected empty old side for extra addition");
                assert!(right.is_some());
            }
            _ => panic!("expected a Pair row"),
        }
    }

    #[test]
    fn e_requests_editor_at_top_visible_line() {
        use crate::git::diff::parse_unified;
        use std::sync::Arc;

        let mut a = app(vec![file("src/foo.rs")]);
        a.tree_cursor = a.cursor_for_path(Path::new("src/foo.rs")).unwrap();
        let raw = concat!("@@ -10,2 +20,2 @@\n", " ctx\n", "-old\n", "+new\n");
        a.set_diff_for_test(parse_unified(Arc::from(raw), PathBuf::from("src/foo.rs")));

        a.on_key(key('e'));
        let req = a.take_editor_request().expect("expected an editor request");
        assert_eq!(req.path, PathBuf::from("src/foo.rs"));
        // Top of the viewport is the chunk header → first new-file line is 20.
        assert_eq!(req.line, 20);
    }

    #[test]
    fn e_refuses_to_open_a_deleted_file() {
        let mut a = app(vec![ChangedFile::new(
            PathBuf::from("gone.rs"),
            ChangeKind::Deleted,
        )]);
        a.tree_cursor = a.cursor_for_path(Path::new("gone.rs")).unwrap();

        a.on_key(key('e'));
        assert!(a.take_editor_request().is_none());
        assert!(a.error.is_some());
    }

    #[test]
    fn question_mark_toggles_help_and_any_key_closes() {
        let mut a = app(vec![file("a.rs")]);
        a.on_key(key('?'));
        assert_eq!(a.mode, Mode::Help);
        // In help mode, a normally-quit key just closes the overlay.
        a.on_key(key('q'));
        assert_eq!(a.mode, Mode::Normal);
        assert!(!a.should_quit);
    }

    /// A single-file app whose diff has plenty of rows to scroll through.
    fn scrollable_diff_app() -> App {
        use crate::git::diff::parse_unified;
        use std::sync::Arc;

        let mut a = app(vec![file("src/foo.rs")]);
        a.tree_cursor = a.cursor_for_path(Path::new("src/foo.rs")).unwrap();
        let mut raw = String::from("@@ -1,30 +1,30 @@\n");
        for i in 0..30 {
            raw.push_str(&format!(" line{i}\n"));
        }
        a.set_diff_for_test(parse_unified(
            Arc::from(raw.as_str()),
            PathBuf::from("src/foo.rs"),
        ));
        a
    }

    #[test]
    fn shift_arrows_page_the_diff() {
        let mut a = scrollable_diff_app();
        a.diff_height = 5;

        a.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(
            a.scroll, 5,
            "Shift+Down should page down by the viewport height"
        );

        a.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(a.scroll, 0, "Shift+Up should page back to the top");
    }

    fn wheel(kind: MouseEventKind, column: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row: 1,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn mouse_wheel_over_diff_scrolls_it() {
        let mut a = scrollable_diff_app();
        a.diff_height = 5;
        a.diff_x = 44;

        a.on_mouse(wheel(MouseEventKind::ScrollDown, 60));
        assert_eq!(a.scroll, MOUSE_SCROLL_LINES as usize);

        a.on_mouse(wheel(MouseEventKind::ScrollUp, 60));
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn mouse_wheel_over_tree_leaves_diff_unscrolled() {
        let mut a = scrollable_diff_app();
        a.diff_height = 5;
        a.diff_x = 44;

        // A wheel notch left of the diff's edge targets the tree, not the diff.
        a.on_mouse(wheel(MouseEventKind::ScrollUp, 5));
        assert_eq!(a.scroll, 0, "tree-side wheel must not scroll the diff");
    }

    fn mouse(kind: MouseEventKind, column: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row: 1,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn angle_keys_resize_the_sidebar() {
        let mut a = app(vec![file("a.rs")]);
        a.body_width = 120;
        let start = a.tree_width;

        a.on_key(KeyEvent::new(KeyCode::Char('>'), KeyModifiers::NONE));
        assert_eq!(a.tree_width, start + TREE_RESIZE_STEP, "> should widen");

        a.on_key(KeyEvent::new(KeyCode::Char('<'), KeyModifiers::NONE));
        assert_eq!(a.tree_width, start, "< should narrow back");
    }

    #[test]
    fn sidebar_resize_clamps_to_bounds() {
        let mut a = app(vec![file("a.rs")]);
        a.body_width = 80;

        // Spam < well past the floor; width should pin to MIN_TREE_WIDTH.
        for _ in 0..200 {
            a.on_key(KeyEvent::new(KeyCode::Char('<'), KeyModifiers::NONE));
        }
        assert_eq!(a.tree_width, MIN_TREE_WIDTH);

        // Spam > past the cap; width should leave MIN_DIFF_WIDTH for the diff.
        for _ in 0..200 {
            a.on_key(KeyEvent::new(KeyCode::Char('>'), KeyModifiers::NONE));
        }
        assert_eq!(a.tree_width, a.body_width - MIN_DIFF_WIDTH);
    }

    #[test]
    fn mouse_drag_on_divider_resizes_sidebar() {
        let mut a = app(vec![file("a.rs")]);
        a.body_width = 120;
        let start = a.tree_width;

        // Press without grabbing the divider: nothing should arm.
        a.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 5));
        a.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 70));
        assert_eq!(a.tree_width, start, "drag without grabbing must not resize");

        // Grab the divider (tree's right border = tree_width - 1) and drag right.
        a.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), start - 1));
        a.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 70));
        assert_eq!(a.tree_width, 71, "divider should follow the cursor");

        // Releasing the button disarms the drag.
        a.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 70));
        a.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 30));
        assert_eq!(a.tree_width, 71, "drag after release must not resize");
    }
}
