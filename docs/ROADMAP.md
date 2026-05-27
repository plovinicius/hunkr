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
- ⬜ **M4 — Reviewed state**: `r`/`u`, diff-hash tie-in, `ChangedAfterReview ↻`, persisted to
  `.git/hunkr/review.json`.
- ⬜ **M5 — AI reference copy**: `y` → prompt builder → `arboard` + OSC 52 fallback + status flash.
- ⬜ **M6 — Polish**: `/` file filter, `?` help overlay, status-bar counts, diff cache +
  bounded hydration, richer error flashes.

## Later (not MVP)

Side-by-side diff toggle · syntax highlighting · minimap · revert/accept-reject hunk ·
inline AI comments · open-in-editor-at-line · session history · multi-repo · daemon mode ·
performance telemetry · config-file keybindings.

## Testing

Unit-tested today: status parser, unified-diff parser, tree builder, viewport math, and
full-frame UI rendering (via ratatui `TestBackend`). Run `cargo test`, `cargo clippy
--all-targets`, `cargo fmt --check`.
