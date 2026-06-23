# hunkr — Configuration

hunkr reads an optional **TOML** config file. Everything in it is optional; anything you
leave out keeps the built-in default. A missing file is fine, and a malformed file never
stops hunkr from starting — it falls back to the defaults and shows a **persistent toast in
the top-right corner** (titled `⚠ Config error`). hunkr keeps running on the defaults and
keeps showing that toast until you fix the file: press `C`, correct it, save, and the
hot-reload clears it the moment the config parses cleanly.

## Where it lives

In order of precedence:

1. `--config <PATH>` on the command line.
2. `$XDG_CONFIG_HOME/hunkr/config.toml` (if `$XDG_CONFIG_HOME` is set).
3. `~/.config/hunkr/config.toml` (the default on macOS and Linux).

The config is **user-owned only** — hunkr deliberately does *not* read a config committed
inside the repository you're reviewing. (That keeps the `[hide]` regex patterns trusted
input and avoids surprises from untrusted repos.)

## Editing it

Press **`C`** (the `edit_config` action) inside hunkr. It opens the config in your
`$VISUAL`/`$EDITOR`, creating a commented template on first use, and **hot-reloads** the
moment you save and exit — no restart needed. (The diff view you're currently in is left
as-is on reload; only the keymap and hide rules re-apply.)

The template's `[keys]` block lists every action with its **current default**, generated from
the binary so it's always accurate — commented out, because those defaults are already in
effect (the file only needs the lines you actually change). The defaults are also baked into
hunkr regardless of the file, so deleting or commenting a line never disables a shortcut. To
see the live bindings at any time (including your own rebinds), press `?` for the help overlay.

## Options

### `view` — startup diff layout

```toml
view = "unified"        # or "side-by-side"
```

The layout hunkr opens in. You can still toggle at runtime with `s` (`toggle_view`).

### `[hide]` — auto-hide files

Files matched here are dropped from the review automatically, on top of the interactive `h`
hides. Press `H` to reveal the hidden list.

```toml
[hide]
names    = ["Cargo.lock", "package-lock.json"]  # exact name or repo-relative path
patterns = ["^dist/", "\\.min\\.(js|css)$"]      # regex over the repo-relative path
```

- `names` matches either the full repo-relative path or just the final file name.
- `patterns` are regular expressions matched against the repo-relative path.

> A file hidden by a rule shows up in the `H` view but can't be un-hidden interactively while
> a rule still matches it — remove or narrow the rule instead.

### `[keys]` — keybindings

Rebind any action. A value is one chord (`"s"`) or a list (`["q", "ctrl+c"]`). Listing an
action **replaces** its default chords; actions you don't list are untouched.

```toml
[keys]
toggle_view = "v"            # move side-by-side toggle to v
quit        = ["q", "ctrl+c"]
edit_config = "C"
```

**Chord syntax**: a single character (`"j"`, `"<"`, `"?"`), or a named key — `enter`, `tab`,
`esc`, `up`, `down`, `left`, `right`, `pgup`, `pgdn`, `home`, `end`, `space`, `backspace`,
`del`, `ins`, `f1`–`f12` — optionally prefixed with modifiers `ctrl+`, `shift+`, `alt+`
(e.g. `"ctrl+c"`, `"shift+down"`). Use a capital letter for shifted letters (`"G"`, `"H"`).

The help overlay (`?`) and the status-bar hints always show your *current* bindings.

#### All actions and their defaults

| Action | Default | Does |
|---|---|---|
| `scroll_down` | `j`, `down` | scroll diff / move cursor down |
| `scroll_up` | `k`, `up` | scroll diff / move cursor up |
| `page_down` | `pgdn`, `shift+down` | page down |
| `page_up` | `pgup`, `shift+up` | page up |
| `next_chunk` | `n` | next chunk |
| `prev_chunk` | `p` | previous chunk |
| `next_file` | `]` | next file |
| `prev_file` | `[` | previous file |
| `narrow_sidebar` | `<` | narrow the sidebar |
| `widen_sidebar` | `>` | widen the sidebar |
| `toggle_view` | `s` | unified ↔ side-by-side |
| `top` | `g` | top of diff |
| `bottom` | `G` | bottom of diff |
| `switch_focus` | `tab` | switch tree / diff focus |
| `activate` | `enter` | expand-collapse folder / focus diff |
| `toggle_reviewed` | `r` | toggle reviewed |
| `toggle_hidden` | `h` | hide / un-hide the selected file |
| `toggle_hidden_view` | `H` | toggle the hidden-files view |
| `copy_reference` | `y` | copy AI reference for the chunk |
| `open_editor` | `e` | open file in `$EDITOR` |
| `edit_config` | `C` | edit this config file |
| `start_filter` | `/` | filter files |
| `help` | `?` | toggle the help overlay |
| `quit` | `q`, `ctrl+c` | quit |

## Full example

```toml
view = "side-by-side"

[hide]
names    = ["Cargo.lock"]
patterns = ["^target/", "\\.snap$"]

[keys]
toggle_view = "v"
quit        = ["q", "ctrl+c"]
```
