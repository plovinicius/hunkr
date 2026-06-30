# hunkr

A fast, terminal-first **git diff reviewer built for AI coding workflows**. When an agent
edits your code, `hunkr` lets you review the changes file-by-file and chunk-by-chunk, mark
what you've checked, and copy an AI-ready reference back to the agent — without leaving the
terminal or opening an IDE.

It is **not** a git client: no commit, push, PR, or merge. Just review.

![hunkr reviewing a diff](docs/preview.gif)

## The loop

1. The AI edits files.
2. `hunkr` auto-refreshes the diff (filesystem watch + off-thread git).
3. You review file-by-file / chunk-by-chunk.
4. Mark each chunk reviewed as you go (`r`) — it collapses out of the way and the cursor
   jumps to the next unreviewed chunk; `R` marks the whole file. Reviewed state is tied to
   the chunk's content hash, so if a chunk changes again it (and only it) falls back to
   unreviewed. The sidebar shows per-file progress (`2/4`).
5. Press `y` to copy an AI-ready reference for the current chunk and paste it back to the
   agent.
6. Repeat — never leaving the terminal.

## Install

```sh
cargo install --git https://github.com/plovinicius/hunkr   # install the `hunkr` binary
```

Or build from a clone:

```sh
git clone https://github.com/plovinicius/hunkr
cd hunkr
cargo install --path .
```

## Run

```sh
hunkr                      # review the repo in the current directory
hunkr PATH                 # review the repo containing PATH
hunkr --ascii              # force ASCII icons ([x] [ ] v >) instead of Unicode
```

During development you can use `cargo run -- …` in place of the installed binary.

Icons are Unicode by default (`✓` reviewed, `◐` partly reviewed, `●` unreviewed, `▸`/`▾`
folders); pass `--ascii` for a plain-text set (`[x]`, `[~]`, `[ ]`, `>`/`v`) if your
terminal/font mis-renders
them. Glyphs known to render double-width in many fonts (`↻`, `⚠`) are deliberately
avoided so columns stay aligned.

Requires the system `git` CLI. Diff scope is everything that differs from `HEAD`
(staged + unstaged + untracked), or the empty tree when the repo has no commits yet.

## Pager mode

Pipe any diff into hunkr and it opens as a **read-only viewer** — handy for reviewing a
branch comparison, a commit, or a `.patch` that isn't your working tree:

```sh
git diff main..feature | hunkr     # review a branch comparison
git show <sha>         | hunkr     # review a single commit
git log -p             | hunkr     # review a range of commits
```

You can also wire it in as git's diff pager so `git diff`/`git show` open in hunkr directly:

```sh
git config --global pager.diff hunkr
git config --global pager.show hunkr
```

hunkr auto-detects this: whenever stdin isn't a terminal it parses the piped diff instead of
scanning a working tree. In pager mode, navigation, fold/expand, unified/side-by-side (`s`),
copy-AI-reference (`y`), and help (`?`) all work; hot-reload, reviewed-state, hide, and
editor-open are disabled (they have no meaning for a static diff) — the status bar shows
`pager (read-only)`. ANSI colour from git is stripped automatically, and merge/combined
(`diff --cc`) sections are skipped. Don't redirect output (`git diff | hunkr > file`) — the
viewer needs a terminal on stdout.

## Keys

| Key       | Action                                   |
|-----------|------------------------------------------|
| `j` / `k` / arrows | move cursor / scroll diff       |
| `Shift`+arrows / `PgUp` / `PgDn` | page up / down   |
| mouse wheel | scroll diff (or move cursor over the tree) |
| `n` / `p` | next / previous chunk                     |
| `]` / `[` | next / previous file                     |
| `s`       | toggle unified / side-by-side diff       |
| `g` / `G` | top / bottom of diff                     |
| `Tab`     | switch tree / diff focus                 |
| `Enter`   | expand-collapse folder / focus diff      |
| `r`       | toggle reviewed for the current chunk    |
| `R`       | toggle reviewed for the whole file       |
| `o`       | reveal every collapsed (reviewed) chunk  |
| `h`       | hide / un-hide the selected file         |
| `H`       | toggle the hidden-files view             |
| `y`       | copy AI reference for the chunk           |
| `e`       | open the file in `$EDITOR` at the line   |
| `C`       | edit the config file (hot-reloads)       |
| `/`       | filter files (`Esc` clears)              |
| `?`       | help overlay                             |
| `q`       | quit                                     |

Every key above is rebindable — see [Configuration](#configuration).

`h` hides the currently-selected file from the review — it leaves the "Changed files" list
and stops counting toward the `✓`/`●` totals. Hiding is permanent: it persists across
sessions until you un-hide. Press `H` to flip the sidebar to a hidden-only view (titled
"Hidden files") where the same `h` un-hides; the status bar reports the hidden count and
flags the hidden view.

Reviewed state persists in `.git/hunkr/review.json`, and the hidden set in
`.git/hunkr/hidden.json` (both per repo/worktree, never tracked).
Clipboard copy uses the system clipboard with an OSC 52 fallback for tmux/SSH, and a brief
top-right `✓ Copied` toast confirms it (auto-dismissing after a moment). `e` opens
`$VISUAL`/`$EDITOR` (falling back to `vi`), jumping to the line for editors that accept
`+LINE` (vi/vim/nvim/nano/emacs/kak); the diff refreshes automatically when you save.

## Configuration

hunkr reads an optional TOML config at `~/.config/hunkr/config.toml` (or
`$XDG_CONFIG_HOME/hunkr/config.toml`; override with `--config <PATH>`). It's layered —
built-in defaults overlaid by your file — and fault-tolerant: a malformed config falls back
to defaults with a status-bar warning instead of failing to start. You can set the startup
diff layout (`view`), remap any key (`[keys]`), and auto-hide files by exact name or regex
(`[hide]`). Press `C` to open it in your editor (a commented template is created on first
use) and it hot-reloads when you save.

```toml
view = "side-by-side"

[hide]
names    = ["Cargo.lock"]
patterns = ["^target/"]

[keys]
toggle_view = "v"
```

See [`docs/CONFIG.md`](docs/CONFIG.md) for the full reference.

## Docs

- [`docs/CONFIG.md`](docs/CONFIG.md) — config file reference (view, keys, hide rules).
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — design, data model, rendering, hot
  reload, caching.
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — milestones and status.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Unless you explicitly state
otherwise, any contribution intentionally submitted for inclusion in this
project by you shall be dual licensed as above, without any additional terms or
conditions.
