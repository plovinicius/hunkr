//! User configuration: a TOML file at `~/.config/hunkr/config.toml`.
//!
//! Configuration is layered: sensible defaults are compiled into the binary
//! ([`Config::default`]) and the user's file *overrides* only the keys it sets.
//! A missing file is normal (defaults apply silently); a malformed file never
//! aborts startup — it falls back to defaults and the problems are returned as
//! warnings for the UI to surface (a persistent top-right error toast).
//!
//! Three things are configurable:
//! - `view` — the default diff layout (`unified` or `side-by-side`).
//! - `[keys]` — a full remap of every [`Action`] (one or many chords each).
//! - `[hide]` — auto-hide rules (exact names and/or regex patterns) that
//!   combine with the interactive `h`/`H` hides from [`crate::persist`].
//!
//! Because the file is *user-owned only* (we deliberately don't read a config
//! committed inside the repo under review), the regex patterns here are trusted
//! input — there's no untrusted-ReDoS surface.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use regex::Regex;
use serde::Deserialize;

use crate::app::{ReviewedDisplay, ViewMode};

/// A normalized key combination: a key plus its (masked) modifiers. See
/// [`normalize`] for what "normalized" means.
pub type Chord = (KeyCode, KeyModifiers);

/// A rebindable command. Every keybinding maps to one of these; the dispatch in
/// [`crate::app::App`] turns an `Action` into a state change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    ScrollDown,
    ScrollUp,
    PageDown,
    PageUp,
    NextChunk,
    PrevChunk,
    NextFile,
    PrevFile,
    NarrowSidebar,
    WidenSidebar,
    ToggleView,
    Top,
    Bottom,
    SwitchFocus,
    Activate,
    ToggleReviewed,
    ToggleChunkReviewed,
    ExpandAllChunks,
    ToggleHidden,
    ToggleHiddenView,
    CopyReference,
    OpenEditor,
    EditConfig,
    StartFilter,
    Help,
    Quit,
}

impl Action {
    /// Every action, in display order — drives the generated config template
    /// and the help overlay row order.
    pub const ALL: [Action; 26] = {
        use Action::*;
        [
            ScrollDown,
            ScrollUp,
            PageDown,
            PageUp,
            NextChunk,
            PrevChunk,
            NextFile,
            PrevFile,
            NarrowSidebar,
            WidenSidebar,
            ToggleView,
            Top,
            Bottom,
            SwitchFocus,
            Activate,
            ToggleReviewed,
            ToggleChunkReviewed,
            ExpandAllChunks,
            ToggleHidden,
            ToggleHiddenView,
            CopyReference,
            OpenEditor,
            EditConfig,
            StartFilter,
            Help,
            Quit,
        ]
    };

    /// The snake_case name used as the config `[keys]` table key. The inverse
    /// of [`Action::from_name`].
    pub fn name(self) -> &'static str {
        use Action::*;
        match self {
            ScrollDown => "scroll_down",
            ScrollUp => "scroll_up",
            PageDown => "page_down",
            PageUp => "page_up",
            NextChunk => "next_chunk",
            PrevChunk => "prev_chunk",
            NextFile => "next_file",
            PrevFile => "prev_file",
            NarrowSidebar => "narrow_sidebar",
            WidenSidebar => "widen_sidebar",
            ToggleView => "toggle_view",
            Top => "top",
            Bottom => "bottom",
            SwitchFocus => "switch_focus",
            Activate => "activate",
            ToggleReviewed => "toggle_reviewed",
            ToggleChunkReviewed => "toggle_chunk_reviewed",
            ExpandAllChunks => "expand_all_chunks",
            ToggleHidden => "toggle_hidden",
            ToggleHiddenView => "toggle_hidden_view",
            CopyReference => "copy_reference",
            OpenEditor => "open_editor",
            EditConfig => "edit_config",
            StartFilter => "start_filter",
            Help => "help",
            Quit => "quit",
        }
    }

    fn from_name(s: &str) -> Option<Action> {
        use Action::*;
        Some(match s {
            "scroll_down" => ScrollDown,
            "scroll_up" => ScrollUp,
            "page_down" => PageDown,
            "page_up" => PageUp,
            "next_chunk" => NextChunk,
            "prev_chunk" => PrevChunk,
            "next_file" => NextFile,
            "prev_file" => PrevFile,
            "narrow_sidebar" => NarrowSidebar,
            "widen_sidebar" => WidenSidebar,
            "toggle_view" => ToggleView,
            "top" => Top,
            "bottom" => Bottom,
            "switch_focus" => SwitchFocus,
            "activate" => Activate,
            "toggle_reviewed" => ToggleReviewed,
            "toggle_chunk_reviewed" => ToggleChunkReviewed,
            "expand_all_chunks" => ExpandAllChunks,
            "toggle_hidden" => ToggleHidden,
            "toggle_hidden_view" => ToggleHiddenView,
            "copy_reference" => CopyReference,
            "open_editor" => OpenEditor,
            "edit_config" => EditConfig,
            "start_filter" => StartFilter,
            "help" => Help,
            "quit" => Quit,
            _ => return None,
        })
    }
}

/// The chord → action lookup table. Built from the defaults, then the user's
/// `[keys]` overrides are layered on per-action ([`KeyMap::rebind`]).
pub struct KeyMap {
    map: HashMap<Chord, Action>,
}

impl KeyMap {
    /// The built-in bindings. These mirror hunkr's original hardcoded keys.
    fn defaults() -> KeyMap {
        use Action as A;
        use KeyCode::*;
        let n = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        let ctrl = KeyModifiers::CONTROL;
        let pairs: &[(Chord, Action)] = &[
            ((Char('j'), n), A::ScrollDown),
            ((Down, n), A::ScrollDown),
            ((Char('k'), n), A::ScrollUp),
            ((Up, n), A::ScrollUp),
            ((PageDown, n), A::PageDown),
            ((Down, shift), A::PageDown),
            ((PageUp, n), A::PageUp),
            ((Up, shift), A::PageUp),
            ((Char('n'), n), A::NextChunk),
            ((Char('p'), n), A::PrevChunk),
            ((Char(']'), n), A::NextFile),
            ((Char('['), n), A::PrevFile),
            ((Char('<'), n), A::NarrowSidebar),
            ((Char('>'), n), A::WidenSidebar),
            ((Char('s'), n), A::ToggleView),
            ((Char('g'), n), A::Top),
            ((Char('G'), n), A::Bottom),
            ((Tab, n), A::SwitchFocus),
            ((Enter, n), A::Activate),
            ((Char('r'), n), A::ToggleChunkReviewed),
            ((Char('R'), n), A::ToggleReviewed),
            ((Char('o'), n), A::ExpandAllChunks),
            ((Char('h'), n), A::ToggleHidden),
            ((Char('H'), n), A::ToggleHiddenView),
            ((Char('y'), n), A::CopyReference),
            ((Char('e'), n), A::OpenEditor),
            ((Char('C'), n), A::EditConfig),
            ((Char('/'), n), A::StartFilter),
            ((Char('?'), n), A::Help),
            ((Char('q'), n), A::Quit),
            ((Char('c'), ctrl), A::Quit),
        ];
        KeyMap {
            map: pairs.iter().copied().collect(),
        }
    }

    /// Look up the action bound to a (already-normalized) chord.
    pub fn get(&self, chord: Chord) -> Option<Action> {
        self.map.get(&chord).copied()
    }

    /// Replace every chord currently bound to `action` with `chords`. Chords are
    /// inserted last-write-wins, so a chord that previously triggered a *different*
    /// action is reassigned to `action`.
    fn rebind(&mut self, action: Action, chords: Vec<Chord>) {
        self.map.retain(|_, a| *a != action);
        for ch in chords {
            self.map.insert(ch, action);
        }
    }

    /// All chords bound to `action`, formatted for display, shortest first then
    /// alphabetical (so help/status hints prefer the terse key).
    pub fn chords_for(&self, action: Action) -> Vec<String> {
        let mut v: Vec<String> = self
            .map
            .iter()
            .filter(|(_, a)| **a == action)
            .map(|(c, _)| chord_to_string(*c))
            .collect();
        v.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        v
    }

    /// The terse chord to advertise for `action` in a status hint, if bound.
    pub fn primary(&self, action: Action) -> Option<String> {
        self.chords_for(action).into_iter().next()
    }
}

impl Default for KeyMap {
    fn default() -> Self {
        KeyMap::defaults()
    }
}

/// Auto-hide rules from `[hide]`. Empty by default, so a hunkr with no config
/// behaves exactly as before.
#[derive(Default)]
pub struct HideRules {
    names: HashSet<String>,
    patterns: Vec<Regex>,
}

impl HideRules {
    /// Whether `path` (repo-relative) should be auto-hidden: an exact match on
    /// the full path or its final component, or any regex matching the path.
    pub fn matches(&self, path: &Path) -> bool {
        let full = path.to_string_lossy();
        if self.names.contains(full.as_ref()) {
            return true;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && self.names.contains(name)
        {
            return true;
        }
        self.patterns.iter().any(|re| re.is_match(&full))
    }
}

/// The fully-resolved configuration the app runs against.
pub struct Config {
    pub view: ViewMode,
    pub reviewed_chunks: ReviewedDisplay,
    pub keys: KeyMap,
    pub hide: HideRules,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            view: ViewMode::Unified,
            reviewed_chunks: ReviewedDisplay::Collapse,
            keys: KeyMap::default(),
            hide: HideRules::default(),
        }
    }
}

impl Config {
    /// The default config path: `$XDG_CONFIG_HOME/hunkr/config.toml`, else
    /// `$HOME/.config/hunkr/config.toml`. `None` only when neither is set.
    pub fn default_path() -> Option<PathBuf> {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
            return Some(PathBuf::from(xdg).join("hunkr").join("config.toml"));
        }
        let home = std::env::var_os("HOME").filter(|s| !s.is_empty())?;
        Some(
            PathBuf::from(home)
                .join(".config")
                .join("hunkr")
                .join("config.toml"),
        )
    }

    /// Load from `path`, overlaying onto the built-in defaults. Returns the
    /// resolved config plus any non-fatal warnings (unknown keys, bad regex,
    /// parse errors). A missing file yields the defaults with no warnings.
    pub fn load(path: &Path) -> (Config, Vec<String>) {
        let mut cfg = Config::default();
        let mut warnings = Vec::new();

        let Ok(text) = fs::read_to_string(path) else {
            return (cfg, warnings); // missing/unreadable → defaults, silently
        };
        let raw: RawConfig = match toml::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                warnings.push(format!("parse error: {}", first_line(&e.to_string())));
                return (cfg, warnings);
            }
        };

        if let Some(v) = raw.reviewed_chunks.as_deref() {
            match parse_reviewed_display(v) {
                Some(rd) => cfg.reviewed_chunks = rd,
                None => warnings.push(format!(
                    "unknown reviewed_chunks `{v}` (use \"collapse\" or \"dim\")"
                )),
            }
        }

        if let Some(v) = raw.view.as_deref() {
            match parse_view(v) {
                Some(vm) => cfg.view = vm,
                None => warnings.push(format!(
                    "unknown view `{v}` (use \"unified\" or \"side-by-side\")"
                )),
            }
        }

        cfg.hide.names = raw.hide.names.into_iter().collect();
        for p in raw.hide.patterns {
            match Regex::new(&p) {
                Ok(re) => cfg.hide.patterns.push(re),
                Err(e) => warnings.push(format!(
                    "bad hide pattern `{p}`: {}",
                    first_line(&e.to_string())
                )),
            }
        }

        for (name, spec) in raw.keys {
            let Some(action) = Action::from_name(&name) else {
                warnings.push(format!("unknown action `{name}`"));
                continue;
            };
            let mut chords = Vec::new();
            for c in spec.into_vec() {
                match parse_chord(&c) {
                    Ok(ch) => chords.push(ch),
                    Err(e) => warnings.push(format!("bad key `{c}` for `{name}`: {e}")),
                }
            }
            if !chords.is_empty() {
                cfg.keys.rebind(action, chords);
            }
        }

        (cfg, warnings)
    }

    /// The commented template written to disk the first time the user opens the
    /// config (when no file exists yet). The static prose is followed by a
    /// **generated** `[keys]` block listing every action with its real default
    /// chords, so the documented defaults can never drift from the code.
    pub fn default_template() -> String {
        let mut s = String::from(include_str!("config_template.toml"));
        s.push_str(&default_keys_doc());
        s
    }
}

/// Build the commented `[keys]` reference block from the live default keymap.
/// Each line is `# <action> = <chord(s)>`, ready to uncomment and edit.
fn default_keys_doc() -> String {
    let km = KeyMap::defaults();
    let width = Action::ALL
        .iter()
        .map(|a| a.name().len())
        .max()
        .unwrap_or(0);
    let mut out = String::from("# [keys]\n");
    for action in Action::ALL {
        let chords = km.chords_for(action);
        let value = match chords.as_slice() {
            [one] => format!("\"{one}\""),
            many => {
                let quoted: Vec<String> = many.iter().map(|c| format!("\"{c}\"")).collect();
                format!("[{}]", quoted.join(", "))
            }
        };
        out.push_str(&format!(
            "# {name:<width$} = {value}\n",
            name = action.name()
        ));
    }
    out
}

// ── raw (on-disk) shapes ───────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct RawConfig {
    view: Option<String>,
    reviewed_chunks: Option<String>,
    #[serde(default)]
    hide: RawHide,
    #[serde(default)]
    keys: BTreeMap<String, ChordSpec>,
}

#[derive(Deserialize, Default)]
struct RawHide {
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    patterns: Vec<String>,
}

/// A binding value: either a single chord (`"s"`) or a list (`["q", "ctrl+c"]`).
#[derive(Deserialize)]
#[serde(untagged)]
enum ChordSpec {
    One(String),
    Many(Vec<String>),
}

impl ChordSpec {
    fn into_vec(self) -> Vec<String> {
        match self {
            ChordSpec::One(s) => vec![s],
            ChordSpec::Many(v) => v,
        }
    }
}

// ── parsing helpers ─────────────────────────────────────────────────────────

fn parse_view(s: &str) -> Option<ViewMode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "unified" | "default" | "stacked" => Some(ViewMode::Unified),
        "side-by-side" | "side_by_side" | "sidebyside" | "split" => Some(ViewMode::SideBySide),
        _ => None,
    }
}

fn parse_reviewed_display(s: &str) -> Option<ReviewedDisplay> {
    match s.trim().to_ascii_lowercase().as_str() {
        "collapse" | "fold" | "hide" => Some(ReviewedDisplay::Collapse),
        "dim" | "dimmed" | "keep" => Some(ReviewedDisplay::Dim),
        _ => None,
    }
}

/// Normalize a key event (or parsed chord) into a stable lookup key. We keep
/// only Ctrl/Alt/Shift (dropping protocol-specific modifier bits), and for a
/// character key we drop Shift — the shift is already encoded in the char's
/// case (`H` vs `h`), so terminals that do or don't report it both match.
pub fn normalize(code: KeyCode, mods: KeyModifiers) -> Chord {
    let mut m = mods & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
    if matches!(code, KeyCode::Char(_)) {
        m.remove(KeyModifiers::SHIFT);
    }
    (code, m)
}

/// Parse a chord string like `"j"`, `"ctrl+c"`, `"shift+down"`, `"<"`, `"f5"`.
/// The result is normalized via [`normalize`].
pub fn parse_chord(s: &str) -> Result<Chord, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty key".into());
    }
    let parts: Vec<&str> = s.split('+').collect();
    let (mod_parts, key_part) = parts.split_at(parts.len() - 1);

    let mut mods = KeyModifiers::NONE;
    for m in mod_parts {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
            "shift" => mods |= KeyModifiers::SHIFT,
            "alt" | "option" | "meta" => mods |= KeyModifiers::ALT,
            other => return Err(format!("unknown modifier `{other}`")),
        }
    }

    let mut code = parse_key(key_part[0])?;
    // Crossterm reports Ctrl/Alt letter combos as the lowercase char; match it.
    if let KeyCode::Char(c) = code
        && mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        && c.is_ascii_alphabetic()
    {
        code = KeyCode::Char(c.to_ascii_lowercase());
    }
    Ok(normalize(code, mods))
}

fn parse_key(k: &str) -> Result<KeyCode, String> {
    let chars: Vec<char> = k.chars().collect();
    if chars.len() == 1 {
        return Ok(KeyCode::Char(chars[0]));
    }
    let lower = k.to_ascii_lowercase();
    let code = match lower.as_str() {
        "enter" | "return" | "cr" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pgup" | "pageup" => KeyCode::PageUp,
        "pgdn" | "pagedown" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        f if f.starts_with('f') && f[1..].parse::<u8>().is_ok() => {
            KeyCode::F(f[1..].parse().unwrap())
        }
        _ => return Err(format!("unknown key `{k}`")),
    };
    Ok(code)
}

/// Render a chord for display (the inverse of [`parse_chord`] for the common
/// cases). Used by the help overlay and status bar.
pub fn chord_to_string((code, mods): Chord) -> String {
    let mut s = String::new();
    if mods.contains(KeyModifiers::CONTROL) {
        s.push_str("ctrl+");
    }
    if mods.contains(KeyModifiers::ALT) {
        s.push_str("alt+");
    }
    if mods.contains(KeyModifiers::SHIFT) {
        s.push_str("shift+");
    }
    s.push_str(&key_to_string(code));
    s
}

fn key_to_string(code: KeyCode) -> String {
    match code {
        KeyCode::Char(' ') => "space".into(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::PageUp => "pgup".into(),
        KeyCode::PageDown => "pgdn".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Delete => "del".into(),
        KeyCode::Insert => "ins".into(),
        KeyCode::F(n) => format!("f{n}"),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// The first line of a multi-line error message (toml/regex errors can be long).
fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hunkr-cfg-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn action_names_round_trip() {
        // Every action's canonical name parses back to the same action. This
        // also guards the [keys] table keys documented in the template.
        for a in Action::ALL {
            assert_eq!(
                Action::from_name(a.name()),
                Some(a),
                "name round-trip for {a:?}"
            );
        }
    }

    #[test]
    fn template_documents_every_action_default() {
        // The generated template lists every action with its *real* default
        // chords, so the documented defaults can't drift from the code.
        let template = Config::default_template();
        // Collapse each line's runs of whitespace so the column padding in the
        // generated block doesn't matter to the comparison.
        let normalized: Vec<String> = template
            .lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        let km = KeyMap::default();
        for a in Action::ALL {
            let chords = km.chords_for(a);
            let value = if chords.len() == 1 {
                format!("\"{}\"", chords[0])
            } else {
                let quoted: Vec<String> = chords.iter().map(|c| format!("\"{c}\"")).collect();
                format!("[{}]", quoted.join(", "))
            };
            let line = format!("# {} = {value}", a.name());
            assert!(
                normalized.contains(&line),
                "template missing default line: `{line}`"
            );
        }
    }

    #[test]
    fn chord_round_trips() {
        for s in [
            "j",
            "G",
            "ctrl+c",
            "shift+down",
            "<",
            "?",
            "tab",
            "enter",
            "pgup",
            "f5",
        ] {
            let chord = parse_chord(s).unwrap();
            // The display form re-parses to the same normalized chord.
            let back = parse_chord(&chord_to_string(chord)).unwrap();
            assert_eq!(chord, back, "round-trip failed for {s}");
        }
    }

    #[test]
    fn shift_on_char_is_normalized_away() {
        // A bare capital and an explicit shift+lowercase should be distinct from
        // the lowercase letter, but a char never keeps the SHIFT bit.
        let (_, mods) = normalize(KeyCode::Char('H'), KeyModifiers::SHIFT);
        assert!(!mods.contains(KeyModifiers::SHIFT));
        // Shift is preserved for non-character keys (Shift+Down ≠ Down).
        let (_, mods) = normalize(KeyCode::Down, KeyModifiers::SHIFT);
        assert!(mods.contains(KeyModifiers::SHIFT));
    }

    #[test]
    fn missing_file_yields_defaults_no_warnings() {
        let (cfg, warns) = Config::load(Path::new("/nonexistent/hunkr/config.toml"));
        assert_eq!(cfg.view, ViewMode::Unified);
        assert!(warns.is_empty());
        assert_eq!(
            cfg.keys.get((KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(Action::Quit)
        );
    }

    #[test]
    fn partial_override_keeps_other_defaults() {
        let path = write_tmp(
            "partial",
            "view = \"side-by-side\"\n[keys]\ntoggle_view = \"v\"\n",
        );
        let (cfg, warns) = Config::load(&path);
        assert!(warns.is_empty(), "unexpected warnings: {warns:?}");
        assert_eq!(cfg.view, ViewMode::SideBySide);
        // toggle_view moved to `v`; the old `s` no longer triggers it.
        assert_eq!(
            cfg.keys.get(parse_chord("v").unwrap()),
            Some(Action::ToggleView)
        );
        assert_eq!(cfg.keys.get(parse_chord("s").unwrap()), None);
        // Everything else keeps its default.
        assert_eq!(cfg.keys.get(parse_chord("q").unwrap()), Some(Action::Quit));
        assert_eq!(
            cfg.keys.get(parse_chord("h").unwrap()),
            Some(Action::ToggleHidden)
        );
    }

    #[test]
    fn malformed_toml_falls_back_with_warning() {
        let path = write_tmp("bad", "this is = = not toml\n");
        let (cfg, warns) = Config::load(&path);
        assert_eq!(cfg.view, ViewMode::Unified);
        assert!(!warns.is_empty());
    }

    #[test]
    fn unknown_action_and_bad_regex_warn_but_load() {
        let path = write_tmp(
            "warns",
            "[keys]\nbogus_action = \"x\"\n[hide]\npatterns = [\"[unclosed\"]\n",
        );
        let (_cfg, warns) = Config::load(&path);
        assert!(warns.iter().any(|w| w.contains("bogus_action")));
        assert!(warns.iter().any(|w| w.contains("hide pattern")));
    }

    #[test]
    fn hide_rules_match_names_and_patterns() {
        let path = write_tmp(
            "hide",
            "[hide]\nnames = [\"Cargo.lock\", \"package-lock.json\"]\npatterns = [\"^dist/\", \"\\\\.min\\\\.js$\"]\n",
        );
        let (cfg, warns) = Config::load(&path);
        assert!(warns.is_empty(), "unexpected warnings: {warns:?}");
        assert!(cfg.hide.matches(Path::new("Cargo.lock")));
        // file-name match works for nested paths
        assert!(cfg.hide.matches(Path::new("frontend/package-lock.json")));
        assert!(cfg.hide.matches(Path::new("dist/app.js")));
        assert!(cfg.hide.matches(Path::new("a/b/foo.min.js")));
        assert!(!cfg.hide.matches(Path::new("src/main.rs")));
    }

    #[test]
    fn list_of_chords_binds_all() {
        let path = write_tmp("list", "[keys]\nquit = [\"x\", \"ctrl+d\"]\n");
        let (cfg, warns) = Config::load(&path);
        assert!(warns.is_empty(), "unexpected warnings: {warns:?}");
        assert_eq!(cfg.keys.get(parse_chord("x").unwrap()), Some(Action::Quit));
        assert_eq!(
            cfg.keys.get(parse_chord("ctrl+d").unwrap()),
            Some(Action::Quit)
        );
        // The default `q` was replaced.
        assert_eq!(cfg.keys.get(parse_chord("q").unwrap()), None);
    }

    #[test]
    fn template_parses_cleanly() {
        let path = write_tmp("template", &Config::default_template());
        let (_cfg, warns) = Config::load(&path);
        assert!(
            warns.is_empty(),
            "shipped template must load without warnings: {warns:?}"
        );
    }
}
