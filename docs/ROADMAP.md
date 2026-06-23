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
  - `n`/`p` chunk nav, `j`/`k` + `g`/`G` + PageUp/Down scroll, `]`/`[` file nav

## Phase B — full MVP

- ✅ **M3 — Hot reload**
  - `notify` recursive watcher with **`.git/` filtering** (no index/lock feedback loop)
    and ~150ms debounce/coalesce (`src/watch.rs`)
  - off-thread git worker (`event::spawn_git_worker`) recomputes the snapshot so the UI
    thread never blocks on git; requests coalesce
  - `App::reconcile` preserves selection by path and keeps exact scroll + current chunk
    when the selected file's diff is byte-for-byte unchanged
- ✅ **M4 — Reviewed state**
  - `r` toggles reviewed; status tied to a **deterministic diff hash** (seahash) so a
    file that changes again automatically falls back to unreviewed
  - persisted to `.git/hunkr/review.json` (versioned schema), loaded on startup
  - tree shows `✓ ●` glyphs; status bar shows review counts
  - current hashes recomputed off-thread for only the (small) reviewed set on refresh
- ✅ **M5 — AI reference copy**
  - `y` builds an AI-ready prompt for the current chunk (File / Change # / Lines / fenced
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

## Post-MVP

- ✅ **Side-by-side diff** (`s` toggles unified ↔ side-by-side): paired old/new columns,
  virtualized like the unified view; `src/ui/diff_panel.rs` + `build_side_rows` in `app.rs`.
- ✅ **Open in `$EDITOR`** (`e`): suspends the TUI and opens the selected file in
  `$VISUAL`/`$EDITOR` at the top-of-viewport line (`+LINE` for editors that support it),
  then restores and redraws. Input moved onto the UI thread so the editor gets the terminal
  exclusively (`open_editor` in `main.rs`).
- ✅ **Hide files** (`h` hide / un-hide, `H` hidden-only view): permanently drops a file from
  the review, persisted to `.git/hunkr/hidden.json` (`HiddenStore` in `src/persist.rs`).
  Hidden files leave the sidebar and the `✓`/`●` totals; one predicate drives both views and
  composes with the filter; survives hot-reload.
- ✅ **Configuration** (TOML at `~/.config/hunkr/config.toml`, `--config` to override):
  layered defaults + user overrides, fault-tolerant load (malformed → defaults + status-bar
  warning). Configurable `view` (startup layout), full `[keys]` remap (an `Action` enum +
  chord→action `KeyMap`, replacing the hardcoded dispatch; help/hints render from the live
  keymap), and `[hide]` auto-hide rules (exact names + regex) that combine with interactive
  hides. `C` opens the config in `$EDITOR` (seeding a commented template) and hot-reloads on
  return. `src/config.rs`; see `docs/CONFIG.md` and ARCHITECTURE → Configuration.

## Later (not MVP)

Syntax highlighting · minimap · revert/accept-reject chunk ·
inline AI comments · session history · multi-repo · daemon mode ·
performance telemetry · per-repo (committed) config · theme/color config.

## Testing

Unit-tested today: status parser, unified-diff parser, tree builder, viewport math, and
full-frame UI rendering (via ratatui `TestBackend`). Run `cargo test`, `cargo clippy
--all-targets`, `cargo fmt --check`.
