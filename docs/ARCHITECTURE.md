# hunkr — Architecture

`hunkr` is a terminal-first git diff reviewer built for the AI coding loop:

> AI edits files → the diff auto-refreshes → you review file-by-file / chunk-by-chunk →
> mark files reviewed → reviewed state clears automatically when a file changes again →
> copy an AI-friendly reference for a chunk → repeat — without leaving the terminal.

It is **not** a git client: no commit/push/PR/merge/host integration. Just review.

This document is a living design reference. Sections marked _(planned: Mx)_ describe
designs not yet implemented; see [`ROADMAP.md`](./ROADMAP.md) for status.

---

## Design principles

1. **Single source of truth.** One owned `App` struct (`src/app.rs`) holds all state and
   is mutated **only on the UI thread** via event handlers. No `Arc<Mutex<_>>`, no shared
   mutable state, no locks.
2. **Message-passing concurrency.** Background producers (an fs watcher + a git worker)
   never touch `App` — they push `Event`s onto one channel (`src/event.rs`). Terminal input
   is read on the UI thread itself. This keeps the UI thread free so the app stays
   responsive under huge repos and diff storms.
3. **Dirty-flagged repaint.** `terminal.draw()` runs only when `app.dirty` is set — no frame
   timer. The loop polls input with a short timeout and otherwise idles cheaply; ratatui's
   double-buffered backend diffs cells and writes only what changed.
4. **Virtualized rendering.** The diff panel materializes only the visible window of rows
   each frame, so a 200k-line diff costs the same per frame as a 30-line one.
5. **Lazy everything.** The file list is cheap and eager; per-file diffs are fetched and
   parsed only on selection; only the visible rows are rendered.
6. **git CLI is the source of truth.** No git2/libgit2 — we shell out to the system `git`
   so diff semantics (rename detection, EOL, attributes, textconv) match git exactly.

---

## Module map (`src/`)

```
main.rs          entry: CLI parse → repo discover → App::new → terminal guard → run loop
cli.rs           clap args (optional repo path)
event.rs         background Event enum + git-worker thread feeding the channel
terminal.rs      raw mode + alternate screen + mouse capture + panic-hook restore
app.rs           App state (single source of truth) + update/navigation/input
git/
  command.rs     spawn `git` (capture / capture_diff / succeeds)
  repo.rs        repo discovery (rev-parse --show-toplevel)
  status.rs      parse `status --porcelain=v2 -z` → Vec<ChangedFile>
  diff.rs        DiffBase + fetch_file_diff + parse_unified → FileDiff
model/
  file.rs        ChangedFile, ChangeKind
  diff.rs        FileDiff, Chunk, DiffLine (byte-range backed)
  tree.rs        arena FileTree (folder grouping, collapse/expand, flattened visible list)
render/
  viewport.rs    virtualization math (clamp_offset / visible_range)
ui/
  mod.rs         frame composition (tree | diff body + status bar) + render tests
  tree_panel.rs  left panel: changed-file tree
  diff_panel.rs  right panel: virtualized stacked diff
  statusbar.rs   bottom bar: context + key hints
```

---

## Data model

Memory strategy: each file's full `git diff` output is stored **once** as `Arc<str>`;
every `DiffLine` and chunk header is a `Range<usize>` into that backing string — no
per-line allocation. Only the **selected** file's diff is hydrated; the rest stay as
lightweight `ChangedFile` metadata. The tree is an **arena** (`Vec<TreeNode>` referenced
by index — no `Rc`/pointers).

- `ChangedFile { path, kind: ChangeKind, additions, deletions }` — one tree row.
- `FileDiff { path, text: Arc<str>, chunks, is_binary }`.
- `Chunk { header: Range, lines: Vec<DiffLine> }`.
- `DiffLine { kind, old_no, new_no, text: Range }` (`text` excludes the `+`/`-`/` ` marker).
- `FileTree { nodes: Vec<TreeNode>, root, visible: Vec<usize> }`; `visible` is the
  flattened, in-order list of on-screen node indices given collapsed state.
- `App` owns `files`, `tree`, `tree_cursor`, the hydrated `diff` + its flattened
  `diff_rows` / `chunk_starts`, `scroll`, `current_chunk`, `focus`, and dirty/quit flags.

`RowRef` (`Header(chunk)` | `Line(chunk, line)`) is the flattened render-row index built
once on hydration; the diff panel slices a window out of it.

---

## Git interaction & diff parsing

- **Diff scope:** everything that differs from the base — staged + unstaged + all
  untracked, surfaced once per path. Base is `HEAD`, or git's empty-tree object
  (`4b825d…`) when the repo has no commits yet (`DiffBase` in `git/diff.rs`).
- **File list (eager, cheap):** `git -c core.quotepath=false status --porcelain=v2 -z
  --untracked-files=all`. `-z` (NUL-delimited) is robust to spaces/unicode; v2 carries
  rename info. Rename records consume a trailing original-path field.
- **Per-file diff (lazy, on selection):** tracked → `git diff --no-color --no-ext-diff -M
  <base> -- <path>`; untracked → `git diff --no-index -- /dev/null <path>` (exit code 1 is
  expected and tolerated).
- **Parser:** a line-by-line state machine; preamble before the first `@@` is skipped,
  chunk bodies retained, line numbers tracked, binary diffs flagged. Cost is O(diff size),
  bounded by *what changed* — a huge file with a small edit parses instantly.

---

## Rendering & virtualization

The diff panel computes `viewport::visible_range(scroll, height, total)` and renders only
those `diff_rows` into `Line`s each frame. Headers render bold cyan — except the chunk the
`n`/`p` cursor is on, which gets a marker, yellow text, and a full-width background fill so
the current change is obvious; lines render with an `old new ± ` gutter, green/red/gray by
kind, with tab expansion (`unicode-width`).
Panel heights are written back into `App` during render so scroll clamping and PageUp/Down
(and Shift+arrows, which alias them) know the page size; the diff panel's left column
(`diff_x`) is recorded too so mouse-wheel events route to the panel under the cursor.
The sidebar width (`tree_width`) is user-adjustable with `<` / `>` keys and by
left-click-dragging the divider column; the renderer clamps it to
`[MIN_TREE_WIDTH, body_width - MIN_DIFF_WIDTH]` so neither panel can starve.

Verified by in-memory `TestBackend` render tests in `ui/mod.rs`.

---

## Event loop

The UI thread reads terminal input **directly** (polling), and drains background events
(filesystem watch + git worker) from a channel. Input is read on this thread — not a
dedicated one — so that shelling out to `$EDITOR` can hand the terminal over exclusively
without a second reader stealing keystrokes (see "Open in editor").

```
loop {
    if app.dirty { terminal.draw(ui::render); app.dirty = false; }
    if event::poll(POLL_INTERVAL)? {        // returns instantly on a keypress
        handle(event::read());              // + drain any further pending input
    }
    while let Ok(ev) = rx.try_recv() {      // non-blocking background events
        Fs        => request git refresh,
        Refreshed => reconcile preserving state,
        Error     => flash in status bar,
    }
    if let Some(req) = app.take_editor_request() { open_editor(req); }
    if app.should_quit { break; }
}
```

`POLL_INTERVAL` (~100ms) only bounds how soon background refreshes are noticed; input
latency is unaffected (poll wakes immediately on a key). Idle cost is one cheap poll per
interval.

## Open in editor

`e` records an `EditorRequest` (path + the line at the top of the diff viewport). The run
loop then suspends the TUI (`terminal::restore`), runs `$VISUAL`/`$EDITOR` (falling back to
`vi`) as a foreground child — adding `+LINE` for editors known to accept it — re-enters the
alternate screen, and forces a redraw. The subsequent file save trips the watcher, so the
diff refreshes on return.

---

## Hot reload

An fs-watch thread (`src/watch.rs`: `notify`, recursive, **filtering most of `.git/`**
to avoid the index/lock feedback loop while still listening for `HEAD`/`refs/`/`packed-refs`/
`ORIG_HEAD` so commits/checkouts/resets in another shell refresh the view, debounced
~150ms and coalesced) posts `Event::Fs`. The UI thread
forwards that to a git-worker thread (`event::spawn_git_worker`) which recomputes the
changed-file snapshot off-thread and emits `Event::Refreshed`. `App::reconcile` matches
files by path and preserves selection, scroll, and current chunk when the selected file's
diff is byte-for-byte unchanged; otherwise it re-hydrates and clamps. This off-thread
design is what makes refresh feel instant — the UI thread never blocks on `git`.

Currently the selected file's diff is still re-hydrated synchronously on the UI thread
during reconcile (one file, lazy); moving per-file diff fetch fully off-thread is a future
refinement if large single-file diffs ever stutter.

## Reviewed state

Reviewed status is tied to a **diff hash**, not a filename, and is *derived* (never
stored): a record whose hash matches the file's current diff hash → `Reviewed ✓`;
everything else (no record, stale record, or unknown hash) → `Unreviewed ●` (see
`model/review.rs`). The hash is **seahash** — deterministic across processes, so a hash
persisted one session still matches the next (the default randomly-seeded hashers would
not). `r` toggles the record — marking hashes the already-hydrated diff locally (no git
call), unmarking drops the record; on refresh the git worker recomputes hashes for only the
reviewed set, which is what makes a changed file fall back to unreviewed. Records persist
to `.git/hunkr/review.json` via `persist.rs` (versioned `schema`; git dir resolved with
`rev-parse --absolute-git-dir` for worktree correctness), loaded on startup, rewritten on
each change.

## AI reference copy

`y` on a chunk builds an AI-ready prompt (file, change #, new-file line range, the exact
raw diff snippet, and an `Issue:` slot) in `src/reference.rs` and copies it via `arboard`,
falling back to an **OSC 52** terminal escape (with a built-in base64 encoder) so it works
over tmux/SSH where there's no local display. The status bar reports which path was used.
The line range is derived from the chunk's actual line numbers; the snippet is sliced
byte-for-byte from the backing diff text.

## Caching

`src/cache.rs` is a bounded **LRU cache** of parsed `Arc<FileDiff>`, keyed by `(mtime,
size)` of the working-tree file. Re-selecting a file you've already viewed reuses the
parsed diff (an `Arc` clone) instead of shelling out to git — navigation is instant and
memory stays flat (cap `DIFF_CACHE_CAP`). Hot-reload deliberately **bypasses** the cache
(`load_diff(use_cache=false)`) so a fresh-on-disk change can never be masked by a
coarse-resolution mtime; deleted files (no stat) are simply never cached.

## Glyphs

All non-text markers (review status, folder arrows, error prefix, filter cursor, status-bar
separators) come from one `Glyphs` set in `src/glyphs.rs`. The default is **Unicode**
(`✓ ● ▸ ▾`); `--ascii` swaps in a plain-text set (`[x]`/`[ ]`, `>`/`v`) for
terminals/fonts that mis-render the symbols. Every Unicode glyph in that set is chosen to
be single-width in common fonts —
ones frequently rendered double-width (`↻`, `⚠`) are avoided, since a wide glyph swallows
the following space and misaligns the row. The status bar measures text width
(`unicode-width`) and only draws the right-aligned key hints when they fit, so they never
clobber the left-side counts on a narrow terminal.

## Diff views

`s` toggles `App::view` between `Unified` (stacked) and `SideBySide` (old left / new right).
On hydration the diff is flattened into **both** a unified row list (`diff_rows`) and a
side-by-side row list (`side_rows`, built by `build_side_rows`, which pairs each run of
deletions with the additions that follow it). Navigation/scroll operate on whichever list
is active via `active_row_count`/`active_chunk_starts`, and both render paths are virtualized
to the visible window. The side-by-side renderer truncates/pads each column to a fixed width
(`unicode-width`) so the two sides stay aligned.

## Filtering & help

`/` enters filter mode: the tree collapses to a flat, case-insensitive list of files whose
path matches (shown with full paths), with a live prompt in the status bar; Enter keeps the
filter, Esc clears it. `?` opens a centered help overlay (`ui/help.rs`) that any key
closes. Both are driven by `App::mode` (`Normal` / `Filter` / `Help`), which `on_key`
dispatches on.

---

## Crates

`ratatui` (TUI; re-exports `crossterm` for the backend/events — used via
`ratatui::crossterm` to avoid version skew), `clap` (CLI), `crossbeam-channel` (event
bus), `anyhow`/`thiserror` (errors), `unicode-width` (column math). Planned: `notify`
(watch), `serde`/`serde_json` (persistence), `arboard` (clipboard), `ahash`/`seahash`
(diff hash).

## Risks

- **`.git/` watch feedback loop** — must be filtered (highest-priority M3 correctness item).
- **git subprocess latency on huge repos** — mitigated by off-thread git + caching.
- **Terminal restoration on panic** — handled by the panic hook in `terminal.rs`.
- **Clipboard over SSH/tmux** — needs the OSC 52 fallback.
- **Renames / binary / unicode / tabs** — explicit `ChangeKind`, binary placeholder,
  `unicode-width` + tab expansion.
