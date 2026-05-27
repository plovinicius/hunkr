# hunkr — Roadmap

Status legend: ✅ done · 🚧 in progress · ⬜ planned

## Phase A — runnable vertical slice

- ✅ **M0 — Scaffold + docs + terminal guard**
  - cargo project (edition 2024), dependencies, `.gitignore`
  - `docs/ARCHITECTURE.md` + `docs/ROADMAP.md` (living docs)
  - terminal guard: raw mode + alternate screen + panic-hook restore
  - two-pane + status-bar layout; `q` / `Ctrl-C` quit cleanly
  - event bus + input thread; event-driven dirty-flag repaint
- ✅ **M1 — Changed-file tree**
  - `git status --porcelain=v2 -z --untracked-files=all` parser (handles renames,
    untracked, unmerged, paths with spaces)
  - arena `FileTree` with folder grouping + collapse/expand + flattened visible list
  - `j`/`k` cursor nav, `Tab` focus toggle, `Enter` collapse/expand, status glyphs, `+/-` counts
- ✅ **M2 — Lazy virtualized stacked diff + navigation**
  - lazy per-file `git diff` fetch on selection (tracked + untracked + empty-tree base)
  - unified-diff parser → byte-range-backed `FileDiff`
  - virtualized stacked rendering (visible window only)
  - `n`/`p` hunk nav, `j`/`k` + `g`/`G` + PageUp/Down scroll, `]`/`[` file nav

## Phase B — full MVP

- ✅ **M3 — Hot reload**
  - `notify` recursive watcher with **`.git/` filtering** (no index/lock feedback loop)
    and ~150ms debounce/coalesce (`src/watch.rs`)
  - off-thread git worker (`event::spawn_git_worker`) recomputes the snapshot so the UI
    thread never blocks on git; requests coalesce
  - `App::reconcile` preserves selection by path and keeps exact scroll + current hunk
    when the selected file's diff is byte-for-byte unchanged
- ✅ **M4 — Reviewed state**
  - `r` mark / `u` unmark; status tied to a **deterministic diff hash** (seahash) so a
    file that changes again auto-surfaces as `↻ ChangedAfterReview`
  - persisted to `.git/hunkr/review.json` (versioned schema), loaded on startup
  - tree shows `✓ ● ↻` glyphs; status bar shows review counts
  - current hashes recomputed off-thread for only the (small) reviewed set on refresh
- ✅ **M5 — AI reference copy**
  - `y` builds an AI-ready prompt for the current hunk (File / Change # / Lines / fenced
    `diff` snippet / `Issue:` slot) — `src/reference.rs`
  - copies via `arboard`, falling back to an OSC 52 escape (built-in base64) for tmux/SSH;
    status bar reports which path was used
- ✅ **M6 — Polish**
  - `/` file filter (flat, case-insensitive, full paths; Esc clears) with a live
    status-bar prompt
  - `?` help overlay listing keybindings (any key closes)
  - LRU diff cache (`src/cache.rs`) keyed by `(mtime, size)` so re-selecting a file is
    instant; hot-reload bypasses the cache to avoid stale reads

**🎉 MVP complete** — the full review loop works: tree → virtualized diff → hot reload →
reviewed state → AI reference copy, with file filter and help.

## Later (not MVP)

Side-by-side diff toggle · syntax highlighting · minimap · revert/accept-reject hunk ·
inline AI comments · open-in-editor-at-line · session history · multi-repo · daemon mode ·
performance telemetry · config-file keybindings.

## Testing

Unit-tested today: status parser, unified-diff parser, tree builder, viewport math, and
full-frame UI rendering (via ratatui `TestBackend`). Run `cargo test`, `cargo clippy
--all-targets`, `cargo fmt --check`.
