# hunkr

A fast, terminal-first **git diff reviewer built for AI coding workflows**. When an agent
edits your code, `hunkr` lets you review the changes file-by-file and hunk-by-hunk, mark
what you've checked, and copy an AI-ready reference back to the agent — without leaving the
terminal or opening an IDE.

It is **not** a git client: no commit, push, PR, or merge. Just review.

## The loop

1. The AI edits files.
2. `hunkr` auto-refreshes the diff (filesystem watch + off-thread git).
3. You review file-by-file / hunk-by-hunk.
4. Mark files reviewed (`r`). Reviewed state is tied to the file's diff hash, so if the
   file changes again it automatically flips to "changed after review" (`↻`).
5. Press `y` to copy an AI-ready reference for the current hunk and paste it back to the
   agent.
6. Repeat — never leaving the terminal.

## Install & run

```sh
cargo run                  # review the repo in the current directory
cargo run -- PATH          # review the repo containing PATH
cargo run -- --unicode     # use Unicode icons (✓ ●) instead of ASCII
```

Icons are ASCII by default (`[x]` reviewed, `[ ]` unreviewed, `[!]` changed-after-review,
`>`/`v` folders) so they render in any terminal/font; pass `--unicode` for the prettier
symbols (`✓`, `●`) if your font supports them. Glyphs known to render double-width in many
fonts (`↻`, `⚠`) are deliberately avoided so columns stay aligned.

Requires the system `git` CLI. Diff scope is everything that differs from `HEAD`
(staged + unstaged + untracked), or the empty tree when the repo has no commits yet.

## Keys

| Key       | Action                                   |
|-----------|------------------------------------------|
| `j` / `k` | move cursor / scroll diff                |
| `n` / `p` | next / previous hunk                     |
| `]` / `[` | next / previous file                     |
| `s`       | toggle unified / side-by-side diff       |
| `g` / `G` | top / bottom of diff                     |
| `Tab`     | switch tree / diff focus                 |
| `Enter`   | expand-collapse folder / focus diff      |
| `r` / `u` | mark / unmark reviewed                   |
| `y`       | copy AI reference for the hunk           |
| `e`       | open the file in `$EDITOR` at the line   |
| `/`       | filter files (`Esc` clears)              |
| `?`       | help overlay                             |
| `q`       | quit                                     |

Reviewed state persists in `.git/hunkr/review.json` (per repo/worktree, never tracked).
Clipboard copy uses the system clipboard with an OSC 52 fallback for tmux/SSH. `e` opens
`$VISUAL`/`$EDITOR` (falling back to `vi`), jumping to the line for editors that accept
`+LINE` (vi/vim/nvim/nano/emacs/kak); the diff refreshes automatically when you save.

## Docs

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — design, data model, rendering, hot
  reload, caching.
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — milestones and status.
