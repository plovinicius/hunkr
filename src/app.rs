//! Application state — the single source of truth.
//!
//! Per the architecture, `App` is owned and mutated only on the UI thread, in
//! response to events. There are no locks and no shared mutable state. For the
//! Phase-A slice the diff is loaded synchronously on selection (lazy, per file);
//! M3 will move git work onto a background worker thread that feeds events.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};

use crate::cache::DiffCache;
use crate::config::{self, Action, Config};
use crate::git::{self, diff::DiffBase};
use crate::glyphs::Glyphs;
use crate::highlight::FileHighlight;
use crate::model::review::{self, FileHashes, ReviewStatus};
use crate::model::{
    diff::{Chunk, FileDiff, LineKind},
    file::{ChangeKind, ChangedFile},
    snapshot::GitSnapshot,
    tree::FileTree,
};
use crate::persist::{HiddenStore, ReviewStore};
use crate::render::viewport;

/// Max parsed diffs kept hydrated in the LRU cache.
const DIFF_CACHE_CAP: usize = 128;

/// Max computed highlights cached for instant (flicker-free) revisits. Cleared
/// wholesale when exceeded — a coarse but cheap bound.
const HIGHLIGHT_CACHE_CAP: usize = 256;

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

/// How long a transient toast (e.g. "copied for AI") stays up before it
/// auto-dismisses. Kept short — it's a flash acknowledgement, not a message to
/// read. The run loop's 100ms input poll bounds the dismissal resolution.
const TOAST_TTL: Duration = Duration::from_millis(900);

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
    /// Theme picker overlay is open (fuzzy-search + live preview).
    ThemePicker,
}

/// State for the theme picker overlay: a fuzzy-search input over the available
/// theme names with live preview as the cursor moves.
pub struct ThemePicker {
    /// Current search query.
    pub query: String,
    /// Theme names matching `query`, best match first.
    pub matches: Vec<String>,
    /// Index into `matches` of the highlighted row.
    pub cursor: usize,
    /// The theme that was active when the picker opened, restored on cancel.
    pub original: String,
}

/// A request to open a file in `$EDITOR`, consumed by the run loop (which owns
/// the terminal and can suspend/restore it around the editor).
#[derive(Debug, Clone)]
pub struct EditorRequest {
    pub path: PathBuf,
    pub line: u32,
}

/// A transient top-right toast that auto-dismisses once `expires_at` passes
/// (see [`App::expire_toast`]). Used for quick acknowledgements like "copied".
pub struct Toast {
    pub text: String,
    pub expires_at: Instant,
}

/// How the diff panel lays out a file's changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Stacked unified diff (default).
    Unified,
    /// Old version on the left, new version on the right.
    SideBySide,
}

/// How a chunk marked reviewed is presented in the diff panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewedDisplay {
    /// Fold the chunk to its header line (default) — reviewed work disappears.
    Collapse,
    /// Keep the chunk's lines visible but dimmed.
    Dim,
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
    /// Syntax highlighting for the currently displayed `diff`, parallel to its
    /// chunks/lines. Recomputed on file change and on theme change (live preview
    /// in the theme picker). `None` when highlighting is off, the diff is too
    /// large, or no diff is loaded.
    pub highlight: Option<Arc<FileHighlight>>,
    /// The theme name currently used for highlighting. Mirrors `config.theme.theme`
    /// except while the theme picker is previewing a candidate.
    pub active_theme: String,
    /// Open theme picker overlay, if any (`mode == ThemePicker` mirrors this).
    pub theme_picker: Option<ThemePicker>,
    /// Whether the terminal supports 24-bit colour (drives RGB vs xterm-256).
    pub truecolor: bool,
    /// Sender to the foreground highlight worker (the file in view). `None` in
    /// tests (and until the run loop wires it up), where highlighting is instead
    /// computed inline.
    highlight_tx: Option<crossbeam_channel::Sender<crate::event::HighlightRequest>>,
    /// Sender to the prefetch pool that warms not-yet-opened files in the
    /// background. `None` until the run loop wires it up.
    prefetch_tx: Option<crossbeam_channel::Sender<crate::event::HighlightRequest>>,
    /// How many prefetch jobs may be in flight at once (the pool's thread count).
    prefetch_parallelism: usize,
    /// Paths with a prefetch job currently in flight (so we don't redispatch the
    /// same file before its result lands).
    prefetch_inflight: std::collections::HashSet<PathBuf>,
    /// Paths permanently skipped by prefetch this session (binary / oversized).
    /// Cleared when the file list changes, in case a file's nature changed.
    prefetch_skip: std::collections::HashSet<PathBuf>,
    /// Monotonic tag for highlight requests, so a result for a since-superseded
    /// file/theme selection is dropped instead of flashing in.
    highlight_gen: u64,
    /// Computed highlights keyed by path, tagged with the theme and diff-content
    /// hash they were built for. Lets a revisited file render coloured instantly
    /// (no flat→coloured flicker) as long as its diff and theme are unchanged.
    highlight_cache: HashMap<PathBuf, (String, u64, Arc<FileHighlight>)>,
    /// LRU cache of parsed diffs, keyed by file signature.
    cache: DiffCache,

    /// True for the normal live-repo session (git worker + filesystem watch +
    /// persisted reviewed state). False in **pager mode**: the diff was piped in
    /// on stdin and the app is a read-only viewer — no git calls, no hot reload,
    /// no persistence, no editor.
    pub live: bool,
    /// Pre-parsed diffs keyed by path, populated only in pager mode. When set,
    /// [`Self::load_diff`] serves from here instead of shelling out to git.
    preloaded: HashMap<PathBuf, Arc<FileDiff>>,

    pub scroll: usize,
    pub current_chunk: usize,
    /// Per-chunk fold state for the hydrated diff (UI-only, not persisted).
    /// `true` hides a chunk's body rows, showing only its header. Rebuilt on
    /// every hydration; reviewed chunks start collapsed in collapse mode.
    pub chunk_collapsed: Vec<bool>,
    /// Content hash of each chunk in the hydrated diff, parallel to
    /// `diff.chunks`. Used to mark/look-up per-chunk reviewed state.
    pub chunk_hashes: Vec<u64>,

    /// Persisted reviewed-state, keyed by path → reviewed chunk/diff hashes.
    pub review: ReviewStore,
    /// Current diff/chunk hashes for reviewed files (to detect "changed after
    /// review"). Maintained on mark and on each refresh.
    pub hashes: HashMap<PathBuf, FileHashes>,

    /// Persisted set of files hidden from the review. Hidden files are dropped
    /// from the sidebar and excluded from the review counts.
    pub hidden: HiddenStore,
    /// When true, the sidebar inverts to show *only* hidden files (so they can
    /// be un-hidden); otherwise hidden files are dropped from the normal list.
    pub hidden_view: bool,

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

    /// The resolved configuration (defaults overlaid with the user's file).
    pub config: Config,
    /// Where the user config lives on disk, used by the "edit config" action.
    pub config_path: PathBuf,
    /// Set when the user asks to edit the config; the run loop opens it in
    /// `$EDITOR` (creating a template first if absent) and then hot-reloads.
    pub pending_config_edit: bool,
    /// A *persistent* config problem (parse error, unknown action, bad regex):
    /// hunkr keeps running on the built-in defaults and shows this until a clean
    /// reload clears it. Unlike `status_msg`, it survives keypresses.
    pub config_error: Option<String>,

    /// A *transient* top-right toast (e.g. "copied for AI") that auto-dismisses
    /// after [`TOAST_TTL`]; the run loop calls [`Self::expire_toast`] to clear it.
    pub toast: Option<Toast>,
}

impl App {
    pub fn new(repo_root: PathBuf, config: Config, config_path: PathBuf) -> anyhow::Result<Self> {
        let base = git::diff::detect_base(&repo_root);
        let mut files = git::status::changed_files(&repo_root)?;
        // Populate +/- counts up front so the tree shows them before any file's
        // diff is hydrated.
        git::numstat::fill_line_counts(&repo_root, base, &mut files);
        let git_dir = git::repo::git_dir(&repo_root)?;
        let review = ReviewStore::load(&git_dir);
        // Compute current hashes for the (typically few) reviewed files up front
        // so their ✓ status is correct on the very first paint.
        let hashes = git::diff::diff_hashes_for(
            &repo_root,
            base,
            files.iter().filter(|f| review.get(&f.path).is_some()),
        );

        let hidden = HiddenStore::load(&git_dir);

        let mut app = Self::with_files(repo_root, base, files);
        app.review = review;
        app.hashes = hashes;
        app.hidden = hidden;
        app.view = config.view;
        app.config = config;
        app.config_path = config_path;
        app.active_theme = app.config.theme.theme.clone();
        // Apply the persisted hidden set + config rules before picking the first
        // file, so the selection lands on a *shown* file rather than a hidden one.
        app.recompute_view();
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
        let hidden = HiddenStore::empty(&repo_root.join(".git"));
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
            highlight: None,
            active_theme: crate::highlight::DEFAULT_THEME.to_string(),
            theme_picker: None,
            truecolor: crate::highlight::supports_truecolor(),
            highlight_tx: None,
            prefetch_tx: None,
            prefetch_parallelism: 0,
            prefetch_inflight: std::collections::HashSet::new(),
            prefetch_skip: std::collections::HashSet::new(),
            highlight_gen: 0,
            highlight_cache: HashMap::new(),
            cache: DiffCache::new(DIFF_CACHE_CAP),
            live: true,
            preloaded: HashMap::new(),
            scroll: 0,
            current_chunk: 0,
            chunk_collapsed: Vec::new(),
            chunk_hashes: Vec::new(),
            review,
            hashes: HashMap::new(),
            hidden,
            hidden_view: false,
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
            config: Config::default(),
            config_path: Config::default_path().unwrap_or_default(),
            pending_config_edit: false,
            config_error: None,
            toast: None,
        }
    }

    /// Construct a read-only **pager-mode** app from an already-parsed diff
    /// (e.g. piped in from `git diff`/`git show`). The file list and every
    /// file's diff are supplied up front; the app never calls git, never
    /// watches the filesystem, and never persists reviewed state. `repo_root` is
    /// best-effort (the cwd) and unused on this path.
    pub fn from_diff(
        repo_root: PathBuf,
        files: Vec<ChangedFile>,
        diffs: HashMap<PathBuf, Arc<FileDiff>>,
        config: Config,
        config_path: PathBuf,
    ) -> Self {
        let mut app = Self::with_files(repo_root, DiffBase::Head, files);
        app.live = false;
        app.preloaded = diffs;
        app.view = config.view;
        app.config = config;
        app.config_path = config_path;
        app.active_theme = app.config.theme.theme.clone();
        app.recompute_view();
        app.select_first_file();
        app.ensure_diff_loaded();
        app
    }

    // ── reviewed state ─────────────────────────────────────────────────────

    /// Derived review status for a file (by index into `files`).
    pub fn review_status(&self, file_idx: usize) -> ReviewStatus {
        let path = &self.files[file_idx].path;
        review::derive_file(self.review.get(path), self.hashes.get(path))
    }

    /// `(reviewed, total)` chunk counts for a file, for the sidebar `n/m` badge.
    /// Zero/zero when the file has no record or no known hashes yet.
    pub fn reviewed_chunk_count(&self, file_idx: usize) -> (usize, usize) {
        let path = &self.files[file_idx].path;
        let hashes = self.hashes.get(path);
        let total = hashes.map(|h| h.chunks.len()).unwrap_or(0);
        let reviewed = review::reviewed_chunk_count(self.review.get(path), hashes);
        (reviewed, total)
    }

    /// Whether chunk `h` of the currently hydrated diff is marked reviewed.
    pub fn chunk_reviewed(&self, h: usize) -> bool {
        let Some(d) = self.diff.as_ref() else {
            return false;
        };
        let Some(&hash) = self.chunk_hashes.get(h) else {
            return false;
        };
        review::chunk_reviewed(self.review.get(&d.path), hash)
    }

    /// Paths that currently have a review record (asked of the git worker so it
    /// re-hashes only those on refresh).
    pub fn reviewed_paths(&self) -> Vec<PathBuf> {
        self.review.reviewed_paths()
    }

    /// The whole-diff + per-chunk hashes of the currently hydrated file.
    fn current_file_hashes(&self) -> Option<FileHashes> {
        let d = self.diff.as_ref()?;
        Some(FileHashes {
            whole: git::diff::hash_text(&d.text),
            chunks: self.chunk_hashes.clone(),
        })
    }

    /// Keep the hydrated file's entry in `hashes` current (so the sidebar `n/m`
    /// and ✓ update immediately after a mark, without a worker round-trip).
    fn refresh_current_hashes(&mut self) {
        let Some(d) = self.diff.as_ref() else {
            return;
        };
        let path = d.path.clone();
        let whole = git::diff::hash_text(&d.text);
        let chunks = self.chunk_hashes.clone();
        if self.review.get(&path).is_some() {
            self.hashes.insert(path, FileHashes { whole, chunks });
        } else {
            self.hashes.remove(&path);
        }
    }

    /// Toggle reviewed state for the *current chunk* (the `r` action). Marking a
    /// chunk collapses it (in collapse mode) and advances to the next unreviewed
    /// chunk; un-marking re-opens it. Falls back to the whole-file toggle for
    /// binary/no-chunk files.
    fn toggle_chunk_reviewed(&mut self) {
        let Some(d) = self.diff.clone() else {
            return;
        };
        if self.chunk_hashes.is_empty() {
            self.toggle_reviewed();
            return;
        }
        let h = self.current_chunk.min(self.chunk_hashes.len() - 1);
        let chunk_hash = self.chunk_hashes[h];
        let path = d.path.clone();
        let whole = git::diff::hash_text(&d.text);
        let now_reviewed = !review::chunk_reviewed(self.review.get(&path), chunk_hash);

        let result = if now_reviewed {
            self.review
                .mark_chunk(path.clone(), whole, chunk_hash, unix_now())
        } else {
            self.review.unmark_chunk(&path, chunk_hash)
        };
        // A failed *persist* still leaves the in-memory state updated, so reflect
        // it in the UI either way and only surface the save error.
        if let Err(e) = result {
            self.error = Some(format!("could not save review state: {e}"));
        }
        self.refresh_current_hashes();

        if self.config.reviewed_chunks == ReviewedDisplay::Collapse {
            if let Some(c) = self.chunk_collapsed.get_mut(h) {
                *c = now_reviewed;
            }
            self.rebuild_rows(&d);
        }
        if now_reviewed {
            self.advance_to_next_unreviewed(h);
        }
        self.dirty = true;
    }

    /// Move the chunk cursor to the first unreviewed chunk after `from`; if there
    /// is none, stay put.
    fn advance_to_next_unreviewed(&mut self, from: usize) {
        let Some(d) = self.diff.clone() else {
            return;
        };
        let n = self.chunk_hashes.len();
        for idx in (from + 1)..n {
            let hash = self.chunk_hashes[idx];
            if !review::chunk_reviewed(self.review.get(&d.path), hash) {
                self.current_chunk = idx;
                self.scroll_to_current_chunk();
                return;
            }
        }
    }

    /// Toggle reviewed state for the whole file under the cursor (the `R`
    /// action): marks/clears every chunk (and the whole-diff hash for binary
    /// files), collapsing or revealing all chunks to match.
    fn toggle_reviewed(&mut self) {
        let Some(fi) = self.current_file_index() else {
            return;
        };
        let path = self.files[fi].path.clone();
        let on_current = self.diff_file == Some(fi);
        let collapse = self.config.reviewed_chunks == ReviewedDisplay::Collapse;

        let result = if self.review_status(fi) == ReviewStatus::Reviewed {
            let r = self.review.unmark(&path);
            self.hashes.remove(&path);
            if on_current {
                self.set_all_collapsed(false);
            }
            r
        } else {
            let Some(fh) = self.current_file_hashes() else {
                self.error = Some("no diff to mark reviewed".into());
                return;
            };
            let r = self
                .review
                .mark(path.clone(), fh.whole, fh.chunks.clone(), unix_now());
            self.hashes.insert(path, fh);
            if on_current && collapse {
                self.set_all_collapsed(true);
            }
            r
        };
        if let Err(e) = result {
            self.error = Some(format!("could not save review state: {e}"));
        }
        self.dirty = true;
    }

    /// Reveal every collapsed chunk in the current file without changing reviewed
    /// state (the `o` action). Reviewed chunks keep their ✓ marker.
    fn expand_all_chunks(&mut self) {
        if self.chunk_collapsed.iter().any(|c| *c) {
            self.set_all_collapsed(false);
            self.dirty = true;
        }
    }

    /// Set every chunk's collapse flag and rebuild the render rows.
    fn set_all_collapsed(&mut self, collapsed: bool) {
        for c in self.chunk_collapsed.iter_mut() {
            *c = collapsed;
        }
        if let Some(d) = self.diff.clone() {
            self.rebuild_rows(&d);
        }
    }

    // ── hidden state ─────────────────────────────────────────────────────────

    /// Number of currently-changed files that are hidden.
    pub fn hidden_count(&self) -> usize {
        (0..self.files.len())
            .filter(|&i| self.is_file_hidden(i))
            .count()
    }

    /// Number of currently-changed files that are *not* hidden (the normal
    /// "Changed files" total).
    pub fn shown_count(&self) -> usize {
        self.files.len() - self.hidden_count()
    }

    /// Whether file `fi` is hidden — either interactively (the persisted
    /// [`HiddenStore`]) or by a config auto-hide rule. Used by the renderer to
    /// exclude it from the review counts and by the sidebar view split.
    pub fn is_file_hidden(&self, fi: usize) -> bool {
        let path = &self.files[fi].path;
        self.hidden.is_hidden(path) || self.config.hide.matches(path)
    }

    /// Toggle the selected file's hidden state and persist. This is the single
    /// `h` action: in the normal view it hides the file; in the hidden view the
    /// selected file is already hidden, so the same toggle un-hides it. Either
    /// way the file leaves the current list, so the selection snaps to the next
    /// file.
    fn toggle_hidden(&mut self) {
        let Some(fi) = self.current_file_index() else {
            return;
        };
        let path = self.files[fi].path.clone();
        let result = if self.hidden.is_hidden(&path) {
            self.hidden.unhide(&path)
        } else {
            self.hidden.hide(path)
        };
        if let Err(e) = result {
            self.error = Some(format!("could not save hidden state: {e}"));
        }
        // The file just left the current view; rebuild it and land on the next
        // file (keeping the same row index naturally selects the follower).
        self.recompute_view();
        self.snap_cursor_to_file();
        self.ensure_diff_loaded();
        self.dirty = true;
    }

    /// Toggle between the normal sidebar and the hidden-only view. Both render
    /// through the same code path (see [`Self::recompute_view`]); this only
    /// flips which set is shown and re-anchors the cursor on the first file.
    fn toggle_hidden_view(&mut self) {
        self.hidden_view = !self.hidden_view;
        self.recompute_view();
        self.tree_cursor = self.first_file_cursor().unwrap_or(0);
        self.ensure_diff_loaded();
        self.dirty = true;
    }

    /// Keep the cursor on a file leaf after the visible list changes: clamp into
    /// range, then scan forward (the natural "next file") and finally backward.
    fn snap_cursor_to_file(&mut self) {
        if self.tree.visible.is_empty() {
            self.tree_cursor = 0;
            return;
        }
        if self.tree_cursor >= self.tree.visible.len() {
            self.tree_cursor = self.tree.visible.len() - 1;
        }
        if self.current_file_index().is_some() {
            return;
        }
        let is_file = |c: usize| self.tree.nodes[self.tree.visible[c]].file.is_some();
        if let Some(i) = (self.tree_cursor..self.tree.visible.len()).find(|&c| is_file(c)) {
            self.tree_cursor = i;
        } else if let Some(i) = (0..self.tree_cursor).rev().find(|&c| is_file(c)) {
            self.tree_cursor = i;
        }
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
            Ok(method) => self.show_toast(format!("AI reference · {method}")),
            Err(e) => self.error = Some(format!("copy failed: {e}")),
        }
        self.dirty = true;
    }

    /// Flash a transient top-right toast for [`TOAST_TTL`].
    fn show_toast(&mut self, text: String) {
        self.toast = Some(Toast {
            text,
            expires_at: Instant::now() + TOAST_TTL,
        });
        self.dirty = true;
    }

    /// Clear the toast once its lifetime has elapsed. Called from the run loop,
    /// which wakes at least every input-poll interval, so the toast dismisses on
    /// its own without any user action.
    pub fn expire_toast(&mut self) {
        if self
            .toast
            .as_ref()
            .is_some_and(|t| Instant::now() >= t.expires_at)
        {
            self.toast = None;
            self.dirty = true;
        }
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
        // A file's nature may have changed (e.g. binary↔text); re-evaluate skips
        // on the next prefetch pass.
        self.prefetch_skip.clear();
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

    /// Recompute the tree's visible list. One `keep` predicate gates both the
    /// hidden/normal view split and the text filter, so there's a single source
    /// of truth for "what's in the sidebar right now". With an active filter the
    /// tree collapses to a flat list of matching files (full paths shown);
    /// otherwise it's the normal hierarchical view with empty folders pruned.
    fn recompute_view(&mut self) {
        // Borrow individual fields (not `self`) so the closure stays disjoint
        // from the `&mut self.tree` call below. A file is "hidden" if it's in
        // the interactive store *or* matches a config auto-hide rule.
        let hidden = &self.hidden;
        let hide_rules = &self.config.hide;
        let files = &self.files;
        let hidden_view = self.hidden_view;
        let keep = |fi: usize| {
            let path = &files[fi].path;
            (hidden.is_hidden(path) || hide_rules.matches(path)) == hidden_view
        };

        if self.filter.is_empty() {
            self.tree.recompute_visible_with(keep);
        } else {
            let q = self.filter.to_lowercase();
            self.tree.visible = self
                .tree
                .nodes
                .iter()
                .enumerate()
                .filter_map(|(idx, node)| {
                    let fi = node.file?;
                    (keep(fi) && files[fi].path.to_string_lossy().to_lowercase().contains(&q))
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

        // Pager mode: the diff was parsed up front from stdin. Serve it directly
        // (no cache, no git). A missing entry just clears the panel.
        if !self.live {
            match self.preloaded.get(&path).cloned() {
                Some(diff) => self.adopt_diff(fi, diff),
                None => {
                    self.clear_diff();
                    self.diff_file = Some(fi);
                }
            }
            return;
        }

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

        // Per-chunk content hashes, then the collapse state: reviewed chunks
        // start collapsed in collapse mode.
        self.chunk_hashes = diff
            .chunks
            .iter()
            .map(|c| git::diff::hash_chunk(&diff, c))
            .collect();
        let path = self.files[fi].path.clone();
        let collapse = self.config.reviewed_chunks == ReviewedDisplay::Collapse;
        let record = self.review.get(&path);
        self.chunk_collapsed = self
            .chunk_hashes
            .iter()
            .map(|h| collapse && review::chunk_reviewed(record, *h))
            .collect();
        // Keep this file's hashes current so its ✓/`n/m` is right immediately.
        if record.is_some() {
            let fh = FileHashes {
                whole: git::diff::hash_text(&diff.text),
                chunks: self.chunk_hashes.clone(),
            };
            self.hashes.insert(path, fh);
        } else {
            self.hashes.remove(&path);
        }

        self.rebuild_rows(&diff);
        self.diff = Some(diff);
        self.diff_file = Some(fi);
        self.scroll = 0;
        self.current_chunk = 0;
        self.error = None;
        self.recompute_highlight();
    }

    fn clear_diff(&mut self) {
        self.diff = None;
        self.highlight = None;
        self.diff_rows.clear();
        self.chunk_starts.clear();
        self.side_rows.clear();
        self.side_chunk_starts.clear();
        self.chunk_collapsed.clear();
        self.chunk_hashes.clear();
    }

    /// Give the app handles to the foreground highlight worker and the prefetch
    /// pool, and kick off the first highlight. Called once by the run loop after
    /// the workers are spawned; `parallelism` is the prefetch pool's thread count.
    pub fn set_highlight_senders(
        &mut self,
        highlight_tx: crossbeam_channel::Sender<crate::event::HighlightRequest>,
        prefetch_tx: crossbeam_channel::Sender<crate::event::HighlightRequest>,
        parallelism: usize,
    ) {
        self.highlight_tx = Some(highlight_tx);
        self.prefetch_tx = Some(prefetch_tx);
        self.prefetch_parallelism = parallelism.max(1);
        self.recompute_highlight();
    }

    /// (Re)compute syntax highlighting for the current diff with the active
    /// theme. When the background worker is wired, this dispatches the work
    /// off-thread (clearing the stale highlight so the panel shows a flat diff
    /// until the result lands); without it (tests) the work runs inline. Sets
    /// `highlight` to `None` when highlighting is disabled, there's no textual
    /// diff, or the diff exceeds the configured size guard.
    pub(crate) fn recompute_highlight(&mut self) {
        if !self.should_highlight() {
            self.highlight = None;
            return;
        }
        let Some(diff) = self.diff.clone() else {
            self.highlight = None;
            return;
        };
        let diff_hash = git::diff::hash_text(&diff.text);

        // Cache hit: render coloured immediately, no async round-trip (and so no
        // flat→coloured flicker when flipping back to a file).
        if let Some((theme, hash, hl)) = self.highlight_cache.get(&diff.path)
            && *theme == self.active_theme
            && *hash == diff_hash
        {
            self.highlight = Some(hl.clone());
            return;
        }

        self.highlight_gen = self.highlight_gen.wrapping_add(1);
        match &self.highlight_tx {
            Some(tx) => {
                // Off-thread: drop the old highlight (flat render meanwhile) and
                // request a fresh one; the result arrives via Event::Highlighted.
                self.highlight = None;
                let _ = tx.send(crate::event::HighlightRequest {
                    path: diff.path.clone(),
                    diff,
                    theme: self.active_theme.clone(),
                    truecolor: self.truecolor,
                    diff_hash,
                    generation: self.highlight_gen,
                });
            }
            None => {
                let hl = crate::highlight::theme(&self.active_theme).map(|theme| {
                    Arc::new(crate::highlight::highlight_file(
                        &diff,
                        theme,
                        self.truecolor,
                    ))
                });
                if let Some(hl) = &hl {
                    self.cache_highlight(
                        diff.path.clone(),
                        self.active_theme.clone(),
                        diff_hash,
                        hl.clone(),
                    );
                }
                self.highlight = hl;
            }
        }
    }

    /// Store a computed highlight for later reuse, bounding the cache so a long
    /// session over many files/themes can't grow it without limit.
    fn cache_highlight(&mut self, path: PathBuf, theme: String, hash: u64, hl: Arc<FileHighlight>) {
        if self.highlight_cache.len() >= HIGHLIGHT_CACHE_CAP
            && !self.highlight_cache.contains_key(&path)
        {
            self.highlight_cache.clear();
        }
        self.highlight_cache.insert(path, (theme, hash, hl));
    }

    /// Whether the current diff should be highlighted at all (feature on, textual,
    /// within the size guard).
    fn should_highlight(&self) -> bool {
        if !self.config.theme.syntax {
            return false;
        }
        let Some(fd) = self.diff.as_ref() else {
            return false;
        };
        if fd.is_binary {
            return false;
        }
        let lines: usize = fd.chunks.iter().map(|c| c.lines.len()).sum();
        lines <= self.config.theme.max_lines
    }

    /// Install a highlight result from the worker. It's cached for reuse
    /// regardless (so a prefetched neighbour is reused on open), but only
    /// displayed if it still matches the current file and the latest request
    /// generation (else it's stale — e.g. the user has already moved on).
    pub fn apply_highlight(
        &mut self,
        path: &Path,
        theme: String,
        diff_hash: u64,
        generation: u64,
        highlight: Arc<FileHighlight>,
    ) {
        // Whichever worker produced it, this file is no longer in flight.
        self.prefetch_inflight.remove(path);
        self.cache_highlight(path.to_path_buf(), theme, diff_hash, highlight.clone());
        if generation != self.highlight_gen {
            return; // superseded (or a prefetch result, gen 0) — cached, not shown
        }
        if self.diff.as_ref().map(|d| d.path.as_path()) != Some(path) {
            return;
        }
        self.highlight = Some(highlight);
        self.dirty = true;
    }

    /// Top up the prefetch pool so the highlight cache warms in display order.
    /// Keeps up to `prefetch_parallelism` jobs in flight; called when the UI is
    /// idle so it always yields to active navigation. Each dispatched file is
    /// fetched/parsed here (cheap) and highlighted on the pool (the slow part).
    pub fn pump_prefetch(&mut self) {
        if self.prefetch_tx.is_none() || !self.config.theme.syntax {
            return;
        }
        while self.prefetch_inflight.len() < self.prefetch_parallelism {
            let Some(fi) = self.next_prefetch_file() else {
                break; // everything reachable is warm, in flight, or skipped
            };
            let path = self.files[fi].path.clone();
            let Some(diff) = self.diff_for_index(fi) else {
                self.prefetch_skip.insert(path);
                continue;
            };
            let lines: usize = diff.chunks.iter().map(|c| c.lines.len()).sum();
            if diff.is_binary || lines > self.config.theme.max_lines {
                self.prefetch_skip.insert(path);
                continue;
            }
            let req = crate::event::HighlightRequest {
                path: path.clone(),
                diff_hash: git::diff::hash_text(&diff.text),
                diff,
                theme: self.active_theme.clone(),
                truecolor: self.truecolor,
                generation: 0, // prefetch: cached on arrival, never displayed directly
            };
            self.prefetch_inflight.insert(path);
            if let Some(tx) = &self.prefetch_tx {
                let _ = tx.send(req);
            }
        }
    }

    /// The next file to prefetch, in the tree's display order: the first one that
    /// isn't already warm for the active theme, in flight, or skipped.
    fn next_prefetch_file(&self) -> Option<usize> {
        for &node_idx in &self.tree.visible {
            let Some(fi) = self.tree.nodes[node_idx].file else {
                continue;
            };
            let path = &self.files[fi].path;
            if self.prefetch_inflight.contains(path) || self.prefetch_skip.contains(path) {
                continue;
            }
            let warm = self
                .highlight_cache
                .get(path)
                .is_some_and(|(t, _, _)| *t == self.active_theme);
            if !warm {
                return Some(fi);
            }
        }
        None
    }

    /// Fetch (or reuse from cache/preload) the parsed diff for file `fi` without
    /// disturbing the current selection. Used by prefetch.
    fn diff_for_index(&mut self, fi: usize) -> Option<Arc<FileDiff>> {
        let path = self.files.get(fi)?.path.clone();
        if !self.live {
            return self.preloaded.get(&path).cloned();
        }
        let sig = crate::cache::file_signature(&self.repo_root, &path);
        if let Some(sig) = sig
            && let Some(d) = self.cache.get(&path, sig)
        {
            return Some(d);
        }
        let fd = git::diff::fetch_file_diff(&self.repo_root, self.base, &self.files[fi]).ok()?;
        let diff = Arc::new(fd);
        if let Some(sig) = sig {
            self.cache.put(path, sig, diff.clone());
        }
        Some(diff)
    }

    /// Inject an already-parsed diff (rendering tests use this to exercise the
    /// diff panel without shelling out to git). Routes through [`Self::adopt_diff`]
    /// so per-chunk hashes and collapse state are populated as in normal use.
    #[cfg(test)]
    pub(crate) fn set_diff_for_test(&mut self, fd: FileDiff) {
        self.adopt_diff(0, Arc::new(fd));
        self.focus = Focus::Diff;
    }

    fn rebuild_rows(&mut self, fd: &FileDiff) {
        // Unified rows: header followed by each body line. A collapsed chunk
        // contributes only its header row.
        let mut rows = Vec::new();
        let mut starts = Vec::with_capacity(fd.chunks.len());
        for (h, chunk) in fd.chunks.iter().enumerate() {
            starts.push(rows.len());
            rows.push(RowRef::Header(h));
            if !self.chunk_collapsed.get(h).copied().unwrap_or(false) {
                for l in 0..chunk.lines.len() {
                    rows.push(RowRef::Line(h, l));
                }
            }
        }
        self.diff_rows = rows;
        self.chunk_starts = starts;

        let (side_rows, side_starts) = build_side_rows(fd, &self.chunk_collapsed);
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
            // `toggle` rebuilds the visible list keeping every file; re-apply the
            // hidden/filter predicate so hidden files don't reappear, and let it
            // clamp the cursor.
            self.recompute_view();
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
        self.config.view = self.view;
        self.scroll_to_current_chunk();
        // Persist the choice so it sticks across runs, like the theme does. A
        // write failure is non-fatal — the toggle still applies for this session.
        if let Err(e) = self.persist_view() {
            self.show_toast(format!("view not saved: {e}"));
        }
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
            Mode::ThemePicker => self.on_key_theme_picker(key),
            Mode::Normal => self.on_key_normal(key),
        }
    }

    /// In the help overlay, any key closes it.
    fn on_key_help(&mut self, _key: KeyEvent) {
        self.mode = Mode::Normal;
        self.dirty = true;
    }

    // ── theme picker ───────────────────────────────────────────────────────

    /// Open the theme picker, seeded with every available theme and the cursor
    /// on the currently-active one.
    fn open_theme_picker(&mut self) {
        let matches = crate::highlight::theme_names();
        let cursor = matches
            .iter()
            .position(|n| *n == self.active_theme)
            .unwrap_or(0);
        self.theme_picker = Some(ThemePicker {
            query: String::new(),
            matches,
            cursor,
            original: self.active_theme.clone(),
        });
        self.mode = Mode::ThemePicker;
        self.dirty = true;
    }

    /// Key handling for the theme picker: type to fuzzy-filter, arrows (or
    /// Ctrl-n/p) to move with live preview, Enter to apply + persist, Esc to
    /// cancel and restore the previous theme.
    fn on_key_theme_picker(&mut self, key: KeyEvent) {
        let ctrl = key
            .modifiers
            .contains(ratatui::crossterm::event::KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.close_theme_picker(true),
            KeyCode::Enter => self.close_theme_picker(false),
            KeyCode::Down => self.move_theme_cursor(1),
            KeyCode::Up => self.move_theme_cursor(-1),
            KeyCode::Char('n') if ctrl => self.move_theme_cursor(1),
            KeyCode::Char('p') if ctrl => self.move_theme_cursor(-1),
            KeyCode::Backspace => {
                if let Some(p) = self.theme_picker.as_mut() {
                    p.query.pop();
                }
                self.refilter_themes();
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(p) = self.theme_picker.as_mut() {
                    p.query.push(c);
                }
                self.refilter_themes();
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Re-rank the theme list against the current query and preview the top match.
    fn refilter_themes(&mut self) {
        let Some(p) = self.theme_picker.as_mut() else {
            return;
        };
        let all = crate::highlight::theme_names();
        p.matches = fuzzy_filter(&p.query, all);
        p.cursor = 0;
        self.preview_theme_at_cursor();
    }

    /// Move the picker cursor by `delta`, clamped, then preview that theme.
    fn move_theme_cursor(&mut self, delta: isize) {
        if let Some(p) = self.theme_picker.as_mut() {
            if p.matches.is_empty() {
                return;
            }
            let last = p.matches.len() as isize - 1;
            p.cursor = (p.cursor as isize + delta).clamp(0, last) as usize;
        }
        self.preview_theme_at_cursor();
    }

    /// Apply the highlighted theme to the live diff as a preview (no persistence).
    fn preview_theme_at_cursor(&mut self) {
        let name = self
            .theme_picker
            .as_ref()
            .and_then(|p| p.matches.get(p.cursor).cloned());
        if let Some(name) = name
            && name != self.active_theme
        {
            self.active_theme = name;
            self.recompute_highlight();
        }
    }

    /// Close the picker. On `cancel`, restore the theme that was active when it
    /// opened; otherwise commit the highlighted theme and persist it to config.
    fn close_theme_picker(&mut self, cancel: bool) {
        let Some(p) = self.theme_picker.take() else {
            self.mode = Mode::Normal;
            return;
        };
        if cancel {
            if self.active_theme != p.original {
                self.active_theme = p.original;
                self.recompute_highlight();
            }
        } else {
            let chosen = p
                .matches
                .get(p.cursor)
                .cloned()
                .unwrap_or_else(|| self.active_theme.clone());
            self.active_theme = chosen.clone();
            self.config.theme.theme = chosen.clone();
            self.recompute_highlight();
            match self.persist_theme(&chosen) {
                Ok(()) => self.show_toast(format!("theme: {chosen}")),
                Err(e) => self.show_toast(format!("theme set (not saved: {e})")),
            }
        }
        self.mode = Mode::Normal;
        self.dirty = true;
    }

    /// Write the chosen theme into the user's config file's `[theme]` table.
    fn persist_theme(&self, name: &str) -> Result<(), String> {
        use toml_edit::{Item, Table, value};
        self.edit_config(|doc| {
            if !doc.contains_key("theme") {
                doc["theme"] = Item::Table(Table::new());
            }
            doc["theme"]["theme"] = value(name);
        })
    }

    /// Write the current view mode to the config's top-level `view` key.
    fn persist_view(&self) -> Result<(), String> {
        use toml_edit::value;
        let view = match self.view {
            ViewMode::Unified => "unified",
            ViewMode::SideBySide => "side-by-side",
        };
        self.edit_config(|doc| {
            doc["view"] = value(view);
        })
    }

    /// Apply `edit` to the user's config file, preserving its existing formatting
    /// and comments. Creates the file (from the template) if it's absent.
    fn edit_config(&self, edit: impl FnOnce(&mut toml_edit::DocumentMut)) -> Result<(), String> {
        use toml_edit::DocumentMut;

        let path = &self.config_path;
        let text = if path.exists() {
            std::fs::read_to_string(path).map_err(|e| e.to_string())?
        } else {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            Config::default_template()
        };
        let mut doc = text.parse::<DocumentMut>().map_err(|e| {
            e.to_string()
                .lines()
                .next()
                .unwrap_or("invalid config")
                .to_string()
        })?;
        edit(&mut doc);
        std::fs::write(path, doc.to_string()).map_err(|e| e.to_string())
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
        // Esc clears an active filter. It isn't a rebindable action (Esc is the
        // universal "cancel"), so it's handled ahead of the keymap lookup.
        if key.code == KeyCode::Esc && self.is_filtering() {
            self.filter.clear();
            self.recompute_view();
            self.tree_cursor = 0;
            self.ensure_diff_loaded();
            self.dirty = true;
            return;
        }
        let chord = config::normalize(key.code, key.modifiers);
        if let Some(action) = self.config.keys.get(chord) {
            self.dispatch(action);
        }
    }

    /// Perform a bound [`Action`]. Movement actions keep their focus-dependent
    /// behavior (e.g. `ScrollDown` moves the tree cursor when the tree is
    /// focused, and scrolls the diff otherwise).
    fn dispatch(&mut self, action: Action) {
        use Action::*;

        // Pager mode is a read-only viewer: actions that mutate reviewed/hidden
        // state, open the editor, or edit config have no meaning for a static
        // piped diff. Swallow them with a brief hint instead of acting.
        if !self.live
            && matches!(
                action,
                ToggleReviewed
                    | ToggleChunkReviewed
                    | ToggleHidden
                    | ToggleHiddenView
                    | OpenEditor
                    | EditConfig
            )
        {
            self.show_toast("read-only (pager mode)".into());
            return;
        }

        match action {
            ScrollDown => match self.focus {
                Focus::Tree => self.cursor_down(),
                Focus::Diff => self.scroll_by(1),
            },
            ScrollUp => match self.focus {
                Focus::Tree => self.cursor_up(),
                Focus::Diff => self.scroll_by(-1),
            },
            PageDown => self.scroll_by(self.diff_height.max(1) as isize),
            PageUp => self.scroll_by(-(self.diff_height.max(1) as isize)),
            NextChunk => self.next_chunk(),
            PrevChunk => self.prev_chunk(),
            NextFile => self.next_file(),
            PrevFile => self.prev_file(),
            NarrowSidebar => self.narrow_tree(),
            WidenSidebar => self.widen_tree(),
            ToggleView => self.toggle_view(),
            Top => {
                self.scroll = 0;
                self.sync_chunk_from_scroll();
                self.dirty = true;
            }
            Bottom => {
                self.scroll = self.max_scroll();
                self.sync_chunk_from_scroll();
                self.dirty = true;
            }
            SwitchFocus => {
                self.focus = match self.focus {
                    Focus::Tree => Focus::Diff,
                    Focus::Diff => Focus::Tree,
                };
                self.dirty = true;
            }
            Activate => self.activate(),
            ToggleReviewed => self.toggle_reviewed(),
            ToggleChunkReviewed => self.toggle_chunk_reviewed(),
            ExpandAllChunks => self.expand_all_chunks(),
            ToggleHidden => self.toggle_hidden(),
            ToggleHiddenView => self.toggle_hidden_view(),
            CopyReference => self.copy_reference(),
            OpenEditor => self.open_in_editor(),
            EditConfig => self.pending_config_edit = true,
            StartFilter => {
                self.mode = Mode::Filter;
                self.dirty = true;
            }
            OpenThemePicker => self.open_theme_picker(),
            Help => {
                self.mode = Mode::Help;
                self.dirty = true;
            }
            Quit => self.should_quit = true,
        }
    }

    // ── config ───────────────────────────────────────────────────────────────

    /// Take a pending "edit config" request, if any (called by the run loop,
    /// which owns the terminal and can suspend it for `$EDITOR`).
    pub fn take_config_edit_request(&mut self) -> bool {
        std::mem::take(&mut self.pending_config_edit)
    }

    /// Re-read the config from disk and re-apply it live: rebuild the keymap and
    /// hide rules, then recompute the sidebar. The current diff view is left
    /// as-is so a reload doesn't yank the user out of their layout. A clean load
    /// clears any prior config error; a broken one keeps the defaults in effect
    /// and records the problem persistently (see [`Self::config_error`]).
    pub fn reload_config(&mut self) {
        let (config, warnings) = Config::load(&self.config_path);
        self.config = config;
        self.recompute_view();
        self.snap_cursor_to_file();
        self.ensure_diff_loaded();
        if warnings.is_empty() {
            self.config_error = None;
            self.status_msg = Some("config reloaded".to_string());
        } else {
            self.config_error = Some(warnings.join("; "));
            self.status_msg = None;
        }
        self.dirty = true;
    }
}

/// The first meaningful line number of a chunk: the first line carrying a
/// new-file number, else the first old-file number, else `None`.
fn first_line_no(chunk: &Chunk) -> Option<u32> {
    chunk
        .lines
        .iter()
        .find_map(|l| l.new_no)
        .or_else(|| chunk.lines.iter().find_map(|l| l.old_no))
}

/// Transform a parsed diff into side-by-side rows. Within each chunk, context
/// lines appear on both sides; a run of deletions is paired row-for-row with
/// the run of additions that follows it (extra lines on either side get an
/// empty cell on the other). Returns the rows and each chunk header's row index.
fn build_side_rows(fd: &FileDiff, collapsed: &[bool]) -> (Vec<SideRow>, Vec<usize>) {
    let mut rows = Vec::new();
    let mut starts = Vec::with_capacity(fd.chunks.len());

    for (h, chunk) in fd.chunks.iter().enumerate() {
        starts.push(rows.len());
        rows.push(SideRow::Header(h));

        if collapsed.get(h).copied().unwrap_or(false) {
            continue;
        }

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

/// Rank `candidates` by a case-insensitive fuzzy match against `query`. An empty
/// query keeps the original (alphabetical) order; non-matching names are dropped.
fn fuzzy_filter(query: &str, candidates: Vec<String>) -> Vec<String> {
    if query.is_empty() {
        return candidates;
    }
    let q = query.to_lowercase();
    let mut scored: Vec<(i64, String)> = candidates
        .into_iter()
        .filter_map(|c| fuzzy_score(&q, &c).map(|s| (s, c)))
        .collect();
    // Higher score first; ties broken alphabetically for a stable order.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, c)| c).collect()
}

/// Subsequence fuzzy score: each char of `q` (already lowercased) must occur in
/// order within `candidate`. Contiguous runs score higher and earlier first
/// matches are preferred. `None` when `q` is not a subsequence of `candidate`.
fn fuzzy_score(q: &str, candidate: &str) -> Option<i64> {
    let mut chars = candidate
        .to_lowercase()
        .chars()
        .collect::<Vec<_>>()
        .into_iter();
    let mut score: i64 = 0;
    let mut last_matched = false;
    let mut idx: i64 = 0;
    let mut first: Option<i64> = None;
    for qc in q.chars() {
        loop {
            match chars.next() {
                Some(cc) => {
                    idx += 1;
                    if cc == qc {
                        first.get_or_insert(idx);
                        score += if last_matched { 10 } else { 1 };
                        last_matched = true;
                        break;
                    }
                    last_matched = false;
                }
                None => return None,
            }
        }
    }
    // Prefer names where the match starts earlier.
    Some(score - first.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::file::ChangeKind;
    use ratatui::crossterm::event::KeyModifiers;

    fn file(p: &str) -> ChangedFile {
        ChangedFile::new(PathBuf::from(p), ChangeKind::Modified)
    }

    fn app(files: Vec<ChangedFile>) -> App {
        let mut a = App::with_files(PathBuf::from("/repo"), DiffBase::Head, files);
        // Point config writes (view/theme persistence) at a throwaway temp path so
        // tests never touch the real user config.
        a.config_path =
            std::env::temp_dir().join(format!("hunkr-test-{}.toml", std::process::id()));
        a
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

    #[test]
    fn pager_mode_serves_preloaded_diffs_without_git() {
        // Two files piped in; App::from_diff must render them with no git access
        // (the /repo path doesn't exist), and switching files hydrates from the
        // preloaded map.
        let raw = concat!(
            "diff --git a/one.rs b/one.rs\n",
            "--- a/one.rs\n",
            "+++ b/one.rs\n",
            "@@ -1,1 +1,1 @@\n",
            "-a\n",
            "+b\n",
            "diff --git a/two.rs b/two.rs\n",
            "--- a/two.rs\n",
            "+++ b/two.rs\n",
            "@@ -1,1 +1,2 @@\n",
            " keep\n",
            "+added\n",
        );
        let parsed = crate::git::diff::split_unified(raw);
        let files: Vec<_> = parsed.iter().map(|(f, _)| f.clone()).collect();
        let diffs: HashMap<_, _> = parsed
            .iter()
            .map(|(f, d)| (f.path.clone(), d.clone()))
            .collect();

        let mut a = App::from_diff(
            PathBuf::from("/repo"),
            files,
            diffs,
            Config::default(),
            PathBuf::from("/x"),
        );
        assert!(!a.live);
        // First file is hydrated on construction.
        assert_eq!(a.diff.as_ref().unwrap().path, PathBuf::from("one.rs"));

        // Move to the second file → its preloaded diff is served.
        a.dispatch(Action::NextFile);
        assert_eq!(a.diff.as_ref().unwrap().path, PathBuf::from("two.rs"));

        // A mutating action is inert in pager mode (no panic, no review record).
        a.dispatch(Action::ToggleChunkReviewed);
        assert!(a.review.reviewed_paths().is_empty());
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

    /// Point the app's hidden store at a writable temp dir so `hide`/`unhide`
    /// actually persist (the synthetic `/repo` path isn't writable). Each test
    /// gets its own subdir to stay independent under parallel execution.
    fn writable_hidden(a: &mut App, name: &str) {
        let dir =
            std::env::temp_dir().join(format!("hunkr-app-hidden-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        a.hidden = crate::persist::HiddenStore::empty(&dir);
    }

    fn visible_paths(a: &App) -> Vec<PathBuf> {
        a.tree
            .visible
            .iter()
            .filter_map(|&n| a.tree.nodes[n].file)
            .map(|fi| a.files[fi].path.clone())
            .collect()
    }

    #[test]
    fn h_hides_selected_file_and_advances_to_next() {
        let mut a = app(vec![file("a.rs"), file("b.rs"), file("c.rs")]);
        writable_hidden(&mut a, "advance");
        a.tree_cursor = a.first_file_cursor().unwrap();
        assert_eq!(
            a.current_file_index().map(|i| a.files[i].path.clone()),
            Some(PathBuf::from("a.rs"))
        );

        a.on_key(key('h'));

        assert_eq!(a.hidden_count(), 1);
        assert!(a.hidden.is_hidden(Path::new("a.rs")));
        // a.rs is gone from the sidebar; the selection advanced to the next file.
        assert!(!visible_paths(&a).contains(&PathBuf::from("a.rs")));
        assert_eq!(
            a.current_file_index().map(|i| a.files[i].path.clone()),
            Some(PathBuf::from("b.rs")),
            "selection should move to the next file after hiding"
        );
    }

    #[test]
    fn capital_h_shows_hidden_view_and_h_there_unhides() {
        let mut a = app(vec![file("a.rs"), file("b.rs")]);
        writable_hidden(&mut a, "view");
        a.tree_cursor = a.first_file_cursor().unwrap();
        a.on_key(key('h')); // hide a.rs
        assert_eq!(a.hidden_count(), 1);

        a.on_key(key('H')); // enter the hidden-only view
        assert!(a.hidden_view);
        assert_eq!(
            visible_paths(&a),
            vec![PathBuf::from("a.rs")],
            "hidden view should show only the hidden file"
        );

        // The same `h` action un-hides here (the selected file is already hidden).
        a.tree_cursor = a.first_file_cursor().unwrap();
        a.on_key(key('h'));
        assert_eq!(a.hidden_count(), 0);
        assert!(!a.hidden.is_hidden(Path::new("a.rs")));
        assert!(
            a.tree.visible.is_empty(),
            "nothing left to show once the last hidden file is restored"
        );
    }

    #[test]
    fn hidden_files_stay_hidden_when_toggling_a_folder() {
        // Regression: folder collapse/expand rebuilds the visible list, and must
        // not resurrect a hidden file.
        let mut a = app(vec![file("src/a.rs"), file("src/b.rs"), file("README.md")]);
        writable_hidden(&mut a, "folder_toggle");
        a.tree_cursor = a.cursor_for_path(Path::new("src/a.rs")).unwrap();
        a.on_key(key('h')); // hide src/a.rs
        assert!(a.hidden.is_hidden(Path::new("src/a.rs")));
        assert!(!visible_paths(&a).contains(&PathBuf::from("src/a.rs")));

        // Collapse then expand the src/ folder via Enter.
        let src = a
            .tree
            .visible
            .iter()
            .position(|&n| a.tree.nodes[n].name == "src" && a.tree.nodes[n].file.is_none())
            .unwrap();
        a.tree_cursor = src;
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // collapse
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // expand

        assert!(
            !visible_paths(&a).contains(&PathBuf::from("src/a.rs")),
            "hidden file must not reappear after toggling its folder"
        );
        assert!(visible_paths(&a).contains(&PathBuf::from("src/b.rs")));
    }

    #[test]
    fn rebound_key_dispatches_through_the_keymap() {
        // End-to-end check of the keymap dispatch: a user rebind routes the new
        // chord to the action and frees the old default.
        let dir = std::env::temp_dir().join(format!("hunkr-app-rebind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("config.toml");
        std::fs::write(&cfg_path, "[keys]\ntoggle_view = \"v\"\nquit = \"x\"\n").unwrap();
        let (config, warns) = Config::load(&cfg_path);
        assert!(warns.is_empty(), "unexpected warnings: {warns:?}");

        let mut a = app(vec![file("a.rs")]);
        a.config = config;
        assert_eq!(a.view, ViewMode::Unified);

        a.on_key(key('v')); // rebound toggle_view
        assert_eq!(
            a.view,
            ViewMode::SideBySide,
            "rebound key should toggle the view"
        );

        a.on_key(key('s')); // the old default is no longer bound
        assert_eq!(
            a.view,
            ViewMode::SideBySide,
            "the freed default must not still toggle"
        );

        a.on_key(key('x')); // rebound quit
        assert!(a.should_quit, "rebound quit key should quit");
    }

    #[test]
    fn toast_shows_then_auto_expires() {
        let mut a = app(vec![file("a.rs")]);
        a.show_toast("copied".into());
        assert!(a.toast.is_some());

        // Not yet expired → expire_toast leaves it in place.
        a.expire_toast();
        assert!(
            a.toast.is_some(),
            "a live toast must not be dismissed early"
        );

        // Force the deadline into the past; the next tick clears it and repaints.
        a.dirty = false;
        if let Some(t) = &mut a.toast {
            t.expires_at = Instant::now() - Duration::from_millis(1);
        }
        a.expire_toast();
        assert!(a.toast.is_none(), "an elapsed toast must auto-dismiss");
        assert!(a.dirty, "dismissing a toast must request a repaint");
    }

    #[test]
    fn config_error_persists_across_keypress_and_clears_on_clean_reload() {
        let dir = std::env::temp_dir().join(format!("hunkr-app-cfgerr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("config.toml");

        let mut a = app(vec![file("a.rs")]);
        a.config_path = cfg_path.clone();

        // A malformed config records a persistent error and keeps the defaults.
        std::fs::write(&cfg_path, "this is = = broken\n").unwrap();
        a.reload_config();
        assert!(
            a.config_error.is_some(),
            "malformed config should set an error"
        );

        // The error survives a keypress (unlike the transient status message).
        a.on_key(key('j'));
        assert!(
            a.config_error.is_some(),
            "config error must persist across keypresses until fixed"
        );

        // Fixing the file and reloading clears the error.
        std::fs::write(&cfg_path, "view = \"unified\"\n").unwrap();
        a.reload_config();
        assert!(
            a.config_error.is_none(),
            "a clean reload should clear the error"
        );
    }

    #[test]
    fn config_hide_rule_drops_file_from_sidebar() {
        // A config auto-hide rule combines with interactive hides: a matching
        // file is dropped from the normal view and surfaced in the hidden view.
        let dir = std::env::temp_dir().join(format!("hunkr-app-cfgrule-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("config.toml");
        std::fs::write(&cfg_path, "[hide]\npatterns = [\"\\\\.lock$\"]\n").unwrap();
        let (config, warns) = Config::load(&cfg_path);
        assert!(warns.is_empty(), "unexpected warnings: {warns:?}");

        let mut a = app(vec![file("a.rs"), file("b.lock"), file("c.rs")]);
        a.config = config;
        a.recompute_view();

        assert!(a.is_file_hidden(1), "b.lock should match the config rule");
        assert!(!visible_paths(&a).contains(&PathBuf::from("b.lock")));
        assert_eq!(a.hidden_count(), 1);

        // The hidden view reveals the rule-hidden file.
        a.on_key(key('H'));
        assert_eq!(visible_paths(&a), vec![PathBuf::from("b.lock")]);
    }

    #[test]
    fn hidden_files_stay_hidden_across_a_refresh() {
        let mut a = app(vec![file("a.rs"), file("b.rs")]);
        writable_hidden(&mut a, "refresh");
        a.tree_cursor = a.first_file_cursor().unwrap();
        a.on_key(key('h')); // hide a.rs
        assert!(a.hidden.is_hidden(Path::new("a.rs")));

        // A hot-reload brings the same file list back.
        a.reconcile(GitSnapshot {
            files: vec![file("a.rs"), file("b.rs")],
            hashes: HashMap::new(),
        });

        assert!(
            a.hidden.is_hidden(Path::new("a.rs")),
            "hide must survive a refresh"
        );
        assert!(
            !visible_paths(&a).contains(&PathBuf::from("a.rs")),
            "hidden file must stay out of the sidebar after a refresh"
        );
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
        let (rows, starts) = build_side_rows(&fd, &[]);

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

    /// A single-file app whose diff has three independent chunks.
    fn multi_chunk_app() -> App {
        use crate::git::diff::parse_unified;
        use std::sync::Arc;

        let mut a = app(vec![file("src/foo.rs")]);
        a.tree_cursor = a.cursor_for_path(Path::new("src/foo.rs")).unwrap();
        let raw = "@@ -1,2 +1,2 @@\n a\n-b\n+B\n\
                   @@ -10,2 +10,2 @@\n c\n-d\n+D\n\
                   @@ -20,2 +20,2 @@\n e\n-f\n+F\n";
        a.set_diff_for_test(parse_unified(Arc::from(raw), PathBuf::from("src/foo.rs")));
        a
    }

    #[test]
    fn r_marks_current_chunk_collapses_it_and_advances() {
        let mut a = multi_chunk_app();
        assert_eq!(a.current_chunk, 0);

        a.on_key(key('r'));

        assert!(a.chunk_reviewed(0), "chunk 0 should be reviewed");
        assert!(a.chunk_collapsed[0], "reviewed chunk collapses by default");
        assert_eq!(a.current_chunk, 1, "cursor advances to the next chunk");
        assert_eq!(
            a.review_status(0),
            ReviewStatus::Unreviewed,
            "file isn't done until every chunk is reviewed"
        );
        assert_eq!(a.reviewed_chunk_count(0), (1, 3));
    }

    #[test]
    fn reviewing_every_chunk_completes_the_file() {
        let mut a = multi_chunk_app();
        a.on_key(key('r'));
        a.on_key(key('r'));
        a.on_key(key('r'));

        assert_eq!(a.review_status(0), ReviewStatus::Reviewed);
        assert_eq!(a.reviewed_chunk_count(0), (3, 3));
    }

    #[test]
    fn capital_r_toggles_the_whole_file() {
        let mut a = multi_chunk_app();

        a.on_key(key('R'));
        assert_eq!(a.review_status(0), ReviewStatus::Reviewed);
        assert!(a.chunk_collapsed.iter().all(|c| *c), "all chunks collapse");

        a.on_key(key('R'));
        assert_eq!(a.review_status(0), ReviewStatus::Unreviewed);
        assert!(
            a.chunk_collapsed.iter().all(|c| !*c),
            "un-marking reveals every chunk"
        );
    }

    #[test]
    fn o_expands_all_chunks_without_unreviewing() {
        let mut a = multi_chunk_app();
        a.on_key(key('R')); // mark whole file → everything collapses
        assert!(a.chunk_collapsed.iter().all(|c| *c));

        a.on_key(key('o'));

        assert!(
            a.chunk_collapsed.iter().all(|c| !*c),
            "expand-all reveals every chunk"
        );
        assert_eq!(
            a.review_status(0),
            ReviewStatus::Reviewed,
            "expanding must not change reviewed state"
        );
        assert!(a.chunk_reviewed(0));
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

    #[test]
    fn fuzzy_filter_ranks_subsequence_matches() {
        let themes = vec![
            "Dracula".to_string(),
            "base16-ocean.dark".to_string(),
            "Nord".to_string(),
            "gruvbox-dark".to_string(),
        ];
        // Empty query keeps everything in order.
        assert_eq!(fuzzy_filter("", themes.clone()).len(), 4);
        // "drac" matches only Dracula.
        let m = fuzzy_filter("drac", themes.clone());
        assert_eq!(m, vec!["Dracula".to_string()]);
        // "dark" matches both dark themes; non-matches are dropped.
        let m = fuzzy_filter("dark", themes.clone());
        assert_eq!(m.len(), 2);
        assert!(m.iter().all(|t| t.contains("dark")));
        // A query that matches nothing yields an empty list.
        assert!(fuzzy_filter("zzzz", themes).is_empty());
    }

    #[test]
    fn theme_picker_previews_and_cancel_restores() {
        let mut a = app(vec![file("foo.rs")]);
        a.active_theme = crate::highlight::DEFAULT_THEME.to_string();
        let original = a.active_theme.clone();

        a.on_key(KeyEvent::new(KeyCode::Char('T'), KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::ThemePicker);
        assert!(a.theme_picker.is_some());

        // Type to filter to Dracula and confirm it previews live.
        for c in "drac".chars() {
            a.on_key(key(c));
        }
        assert_eq!(a.active_theme, "Dracula", "cursor theme previews live");

        // Esc cancels and restores the theme that was active on open.
        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::Normal);
        assert!(a.theme_picker.is_none());
        assert_eq!(a.active_theme, original, "cancel restores the prior theme");
    }

    #[test]
    fn async_highlight_dispatches_and_applies_matching_result() {
        use crate::git::diff::parse_unified;
        use crate::highlight::FileHighlight;
        use ratatui::style::Color;

        let mut a = app(vec![file("foo.rs")]);
        let raw = "@@ -1,1 +1,1 @@\n-a\n+let x = 1;\n";
        a.set_diff_for_test(parse_unified(Arc::from(raw), PathBuf::from("foo.rs")));

        // Wire worker channels and select a theme not yet cached, so the next
        // recompute dispatches a request rather than serving a cache hit.
        let (tx, rx) = crossbeam_channel::unbounded();
        let (pf_tx, _pf_rx) = crossbeam_channel::unbounded();
        a.set_highlight_senders(tx, pf_tx, 2);
        a.active_theme = "Nord".to_string();
        a.recompute_highlight();

        // The request was dispatched and the highlight cleared so the panel
        // renders flat until the worker replies.
        assert!(a.highlight.is_none(), "highlight cleared while pending");
        let req = rx.try_recv().expect("a highlight request was sent");
        assert_eq!(req.path, PathBuf::from("foo.rs"));
        assert_eq!(req.theme, "Nord");

        let dummy = Arc::new(FileHighlight {
            chunks: Vec::new(),
            add_bg: Color::Reset,
            del_bg: Color::Reset,
        });

        // A stale generation is ignored (but still cached for later reuse).
        a.apply_highlight(
            &req.path,
            req.theme.clone(),
            req.diff_hash,
            req.generation.wrapping_sub(1),
            dummy.clone(),
        );
        assert!(
            a.highlight.is_none(),
            "stale-generation result must be dropped"
        );

        // A result for a different file is ignored.
        a.apply_highlight(
            Path::new("other.rs"),
            req.theme.clone(),
            req.diff_hash,
            req.generation,
            dummy.clone(),
        );
        assert!(a.highlight.is_none(), "wrong-path result must be dropped");

        // The matching result is installed.
        a.apply_highlight(
            &req.path,
            req.theme.clone(),
            req.diff_hash,
            req.generation,
            dummy,
        );
        assert!(a.highlight.is_some(), "matching result must be applied");

        // Revisiting the same file+theme now hits the cache: coloured instantly,
        // no new request dispatched.
        a.recompute_highlight();
        assert!(a.highlight.is_some(), "cache hit should keep it coloured");
        assert!(
            rx.try_recv().is_err(),
            "a cache hit must not dispatch another request"
        );
    }

    #[test]
    fn prefetch_warms_all_files_in_parallel() {
        use crate::highlight::FileHighlight;
        use ratatui::style::Color;
        use std::collections::HashSet;
        // Pager mode preloads every file's diff, so prefetch needs no git. With
        // three files and the first already warm (highlighted on load), prefetch
        // should warm the other two — up to `parallelism` (2) in flight at once.
        let raw = concat!(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,1 @@\n-a\n+let x = 1;\n",
            "diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1,1 +1,1 @@\n-b\n+let y = 2;\n",
            "diff --git a/c.rs b/c.rs\n--- a/c.rs\n+++ b/c.rs\n@@ -1,1 +1,1 @@\n-c\n+let z = 3;\n",
        );
        let parsed = crate::git::diff::split_unified(raw);
        let files: Vec<_> = parsed.iter().map(|(f, _)| f.clone()).collect();
        let diffs: HashMap<_, _> = parsed
            .iter()
            .map(|(f, d)| (f.path.clone(), d.clone()))
            .collect();
        let mut a = App::from_diff(
            PathBuf::from("/repo"),
            files,
            diffs,
            Config::default(),
            PathBuf::from("/x"),
        );

        let (fg_tx, _fg_rx) = crossbeam_channel::unbounded();
        let (pf_tx, pf_rx) = crossbeam_channel::unbounded();
        a.set_highlight_senders(fg_tx, pf_tx, 2);

        let dummy = || {
            Arc::new(FileHighlight {
                chunks: Vec::new(),
                add_bg: Color::Reset,
                del_bg: Color::Reset,
            })
        };

        // Pump fills the pool up to parallelism (2) — the two not-yet-warm files.
        a.pump_prefetch();
        let r1 = pf_rx.try_recv().expect("first prefetch dispatched");
        let r2 = pf_rx.try_recv().expect("second prefetch dispatched");
        assert!(
            pf_rx.try_recv().is_err(),
            "only `parallelism` jobs in flight"
        );
        let dispatched: HashSet<_> = [r1.path.clone(), r2.path.clone()].into_iter().collect();
        assert_eq!(
            dispatched,
            HashSet::from([PathBuf::from("b.rs"), PathBuf::from("c.rs")]),
        );
        assert_eq!(
            r1.generation, 0,
            "prefetch must not claim a display generation"
        );

        // One result lands → a slot frees → the next pump has nothing new left
        // (the third file is the one still in flight), and once both are cached
        // prefetch is idle.
        a.apply_highlight(&r1.path, r1.theme, r1.diff_hash, 0, dummy());
        a.pump_prefetch();
        assert!(
            pf_rx.try_recv().is_err(),
            "nothing new while the last is in flight"
        );
        a.apply_highlight(&r2.path, r2.theme, r2.diff_hash, 0, dummy());
        a.pump_prefetch();
        assert!(
            pf_rx.try_recv().is_err(),
            "all files warm: prefetch is idle"
        );
    }

    #[test]
    fn toggling_view_persists_to_config() {
        let dir = std::env::temp_dir().join(format!("hunkr-view-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml");

        let mut a = app(vec![file("foo.rs")]);
        a.config_path = cfg.clone();
        assert_eq!(a.view, ViewMode::Unified);

        // Toggle to side-by-side via the bound key.
        a.on_key(key('s'));
        assert_eq!(a.view, ViewMode::SideBySide);

        // The choice is written and reloads as side-by-side.
        let (reloaded, warns) = Config::load(&cfg);
        assert!(
            warns.is_empty(),
            "persisted config must reload cleanly: {warns:?}"
        );
        assert_eq!(reloaded.view, ViewMode::SideBySide);

        // Toggling back persists the new value too.
        a.on_key(key('s'));
        let (reloaded, _) = Config::load(&cfg);
        assert_eq!(reloaded.view, ViewMode::Unified);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn theme_picker_commit_applies_and_persists() {
        let dir = std::env::temp_dir().join(format!("hunkr-theme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml");

        let mut a = app(vec![file("foo.rs")]);
        a.config_path = cfg.clone();

        a.on_key(KeyEvent::new(KeyCode::Char('T'), KeyModifiers::NONE));
        for c in "nord".chars() {
            a.on_key(key(c));
        }
        // Enter commits the highlighted theme.
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(a.active_theme, "Nord");
        assert_eq!(a.config.theme.theme, "Nord");

        // It was written to the config file's [theme] table.
        let written = std::fs::read_to_string(&cfg).unwrap();
        let (reloaded, warns) = Config::load(&cfg);
        assert!(
            warns.is_empty(),
            "persisted config must reload cleanly: {warns:?}"
        );
        assert_eq!(reloaded.theme.theme, "Nord", "config on disk:\n{written}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
