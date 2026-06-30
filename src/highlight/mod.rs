//! Source-code syntax highlighting for diff bodies, via `syntect` with the
//! curated syntax + theme set that the `bat` project maintains (the `two-face`
//! crate).
//!
//! Strategy: highlight **line-by-line, per chunk, in source order** — the same
//! approach `delta` and git's `diff-highlight` use. A diff has no whole-file
//! context (and in pager mode no file on disk at all), so we cannot build a
//! full parse; we feed each chunk's lines to a stateful [`HighlightLines`] and
//! let scope state accumulate within the chunk. Because a chunk interleaves the
//! pre-image (`-`/context) and post-image (`+`/context), we run **two** parser
//! states per chunk so a deleted line can't corrupt the added-side state.
//!
//! Known limitation: a multi-line construct (block comment, multi-line string,
//! here-doc, JSX) that *opens before* a chunk can mis-highlight near the chunk's
//! top, since we start each chunk from the syntax's default state. This is the
//! same, accepted trade-off `delta` carries.
//!
//! Highlights are computed once per displayed file (resolving `syntect`'s RGB
//! styles to `ratatui` colours up front) and stored parallel to the diff's
//! chunks/lines, so the per-frame render path stays cheap. Byte ranges index
//! into the diff's backing `Arc<str>` — no per-token string allocation.

use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier};

use two_face::re_exports::syntect;

use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

use crate::model::diff::{Chunk, FileDiff, LineKind};

/// Default theme when none is configured (or the configured one is unknown). A
/// dark base16 theme that sits well against hunkr's existing dark chrome.
pub const DEFAULT_THEME: &str = "base16-ocean.dark";

/// How the add/del line tint is derived: blend the *theme's own background*
/// toward these reference hues by [`TINT_ALPHA`]. Tying the tint to the theme
/// background means it always harmonizes with the selected theme — a near-black
/// background yields a dark green/red, a blue-grey one a blue-green, and a light
/// theme yields a soft pastel (the Bootstrap-alert effect). The references and
/// alpha are tuned so a typical dark editor background lands on the muted
/// green/red we settled on.
const TINT_ALPHA: f32 = 0.30;
const ADD_REF: (u8, u8, u8) = (66, 154, 80);
const DEL_REF: (u8, u8, u8) = (199, 87, 93);
/// Fallback background when a theme declares none, so tint derivation still works.
const FALLBACK_BG: syntect::highlighting::Color = syntect::highlighting::Color {
    r: 13,
    g: 17,
    b: 23,
    a: 255,
};

/// A resolved, render-ready style for one token: a foreground colour plus text
/// attributes. Background (the add/del tint, current-chunk wash) is applied by
/// the renderer, not here.
#[derive(Debug, Clone, Copy)]
pub struct SynStyle {
    pub fg: Color,
    pub modifier: Modifier,
}

/// One highlighted diff line: a run of `(byte-range-into-fd.text, style)` tokens
/// in display order. An empty `spans` means "no highlighting" — the renderer
/// falls back to the flat diff colour.
#[derive(Debug, Clone, Default)]
pub struct StyledLine {
    pub spans: Vec<(Range<usize>, SynStyle)>,
}

/// Highlights for one chunk, parallel to [`Chunk::lines`] by index.
#[derive(Debug, Clone, Default)]
pub struct ChunkHighlight {
    pub lines: Vec<StyledLine>,
}

/// Highlights for a whole file, parallel to [`FileDiff::chunks`] by index.
/// `add_bg`/`del_bg` are the theme-derived line tints (see [`TINT_ALPHA`]) the
/// renderer paints behind added/removed rows.
#[derive(Debug, Clone)]
pub struct FileHighlight {
    pub chunks: Vec<ChunkHighlight>,
    pub add_bg: Color,
    pub del_bg: Color,
}

impl FileHighlight {
    /// The styled line for chunk `h`, line `l`, if highlighting produced one.
    pub fn line(&self, h: usize, l: usize) -> Option<&StyledLine> {
        self.chunks.get(h).and_then(|c| c.lines.get(l))
    }
}

/// The bat-curated syntax set, loaded once. Includes many languages missing
/// from syntect's defaults (TOML, TypeScript, Dockerfile, …).
fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// The merged theme set: bat's bundled themes plus any user `*.tmTheme` files in
/// the theme directory. Loaded once.
fn themes() -> &'static ThemeSet {
    static SET: OnceLock<ThemeSet> = OnceLock::new();
    SET.get_or_init(|| {
        let embedded = two_face::theme::extra();
        let mut set = ThemeSet::from(&embedded);
        if let Some(dir) = user_theme_dir() {
            // Best-effort: a missing/unreadable dir just leaves the bundled set.
            let _ = set.add_from_folder(&dir);
        }
        set
    })
}

/// `~/.config/hunkr/themes` (or under `$XDG_CONFIG_HOME`), where users can drop
/// extra `.tmTheme` files. `None` when neither env var is set.
fn user_theme_dir() -> Option<std::path::PathBuf> {
    crate::config::Config::default_path().and_then(|p| p.parent().map(|d| d.join("themes")))
}

/// Every theme name available for selection, sorted. Bundled + user themes.
pub fn theme_names() -> Vec<String> {
    let mut names: Vec<String> = themes().themes.keys().cloned().collect();
    names.sort();
    names
}

/// Resolve a theme by name, falling back to [`DEFAULT_THEME`], then to any
/// available theme. `None` only if the set is somehow empty.
fn resolve_theme(name: &str) -> Option<&'static Theme> {
    let set = themes();
    set.themes
        .get(name)
        .or_else(|| set.themes.get(DEFAULT_THEME))
        .or_else(|| set.themes.values().next())
}

/// Whether a theme name resolves to a real bundled/user theme.
pub fn theme_exists(name: &str) -> bool {
    themes().themes.contains_key(name)
}

/// Pick the syntax for a file. Tries the path's extension first; falls back to a
/// first-line heuristic (shebang / modeline — also covers extensionless files
/// like `Dockerfile` in pager mode where the file isn't on disk); then plain
/// text (which yields a single un-styled token, equivalent to no highlighting).
fn detect<'a>(set: &'a SyntaxSet, path: &Path, first_line: Option<&str>) -> &'a SyntaxReference {
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && let Some(syn) = set.find_syntax_by_extension(ext)
    {
        return syn;
    }
    // No usable extension: try the filename as an extension (Dockerfile, Makefile)
    // and then a first-line heuristic.
    if let Some(name) = path.file_name().and_then(|n| n.to_str())
        && let Some(syn) = set.find_syntax_by_extension(name)
    {
        return syn;
    }
    if let Some(line) = first_line
        && let Some(syn) = set.find_syntax_by_first_line(line)
    {
        return syn;
    }
    set.find_syntax_plain_text()
}

/// Highlight every chunk of `fd` with `theme`, resolving colours for the current
/// terminal (`truecolor` selects RGB vs nearest xterm-256). Returns a
/// [`FileHighlight`] parallel to `fd.chunks`/`lines`.
pub fn highlight_file(fd: &FileDiff, theme: &Theme, truecolor: bool) -> FileHighlight {
    let set = syntaxes();
    let first_line = first_body_line(fd);
    let syntax = detect(set, &fd.path, first_line);

    // Derive the add/del tints from the theme's own background so they harmonize
    // with the selected theme (see TINT_ALPHA). Keep them as raw RGB too, since
    // token foregrounds are contrast-checked against them.
    let bg = theme.settings.background.unwrap_or(FALLBACK_BG);
    let add_rgb = blend((bg.r, bg.g, bg.b), ADD_REF, TINT_ALPHA);
    let del_rgb = blend((bg.r, bg.g, bg.b), DEL_REF, TINT_ALPHA);

    let chunks = fd
        .chunks
        .iter()
        .map(|chunk| highlight_chunk(fd, chunk, set, syntax, theme, add_rgb, del_rgb, truecolor))
        .collect();
    FileHighlight {
        chunks,
        add_bg: to_color(add_rgb, truecolor),
        del_bg: to_color(del_rgb, truecolor),
    }
}

/// Blend `(r,g,b)` toward `target` by `a` (0..=1).
fn blend(c: (u8, u8, u8), target: (u8, u8, u8), a: f32) -> (u8, u8, u8) {
    let mix = |x: u8, t: u8| {
        (x as f32 * (1.0 - a) + t as f32 * a)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (mix(c.0, target.0), mix(c.1, target.1), mix(c.2, target.2))
}

/// Resolve an RGB triple to a terminal colour: exact on truecolor, nearest
/// xterm-256 cube/ramp cell otherwise.
fn to_color((r, g, b): (u8, u8, u8), truecolor: bool) -> Color {
    if truecolor {
        Color::Rgb(r, g, b)
    } else {
        Color::Indexed(rgb_to_ansi256(r, g, b))
    }
}

/// The first non-empty body line of the diff, used as the first-line syntax hint
/// for extensionless files. Cheap: scans until the first content byte.
fn first_body_line(fd: &FileDiff) -> Option<&str> {
    fd.chunks
        .iter()
        .flat_map(|c| &c.lines)
        .map(|l| fd.slice(&l.text))
        .find(|s| !s.trim().is_empty())
}

#[allow(clippy::too_many_arguments)]
fn highlight_chunk(
    fd: &FileDiff,
    chunk: &Chunk,
    set: &SyntaxSet,
    syntax: &SyntaxReference,
    theme: &Theme,
    add_rgb: (u8, u8, u8),
    del_rgb: (u8, u8, u8),
    truecolor: bool,
) -> ChunkHighlight {
    // Two independent parser states: the pre-image (context + deletions) and the
    // post-image (context + additions). Context feeds both so each state stays
    // coherent across a hunk that touches a multi-line construct.
    let mut old = HighlightLines::new(syntax, theme);
    let mut new = HighlightLines::new(syntax, theme);

    let lines = chunk
        .lines
        .iter()
        .map(|dl| {
            let text = fd.slice(&dl.text);
            let start = dl.text.start;
            match dl.kind {
                LineKind::Context => {
                    // Keep both states advancing; display the new-side result
                    // (identical text, so either is fine). Context has no tint, so
                    // its foregrounds aren't contrast-adjusted.
                    let _ = highlight_one(&mut old, text, start, set, None, truecolor);
                    highlight_one(&mut new, text, start, set, None, truecolor)
                }
                LineKind::Add => {
                    highlight_one(&mut new, text, start, set, Some(add_rgb), truecolor)
                }
                LineKind::Del => {
                    highlight_one(&mut old, text, start, set, Some(del_rgb), truecolor)
                }
                // The "\ No newline at end of file" marker is not code.
                LineKind::NoNewline => StyledLine::default(),
            }
        })
        .collect();
    ChunkHighlight { lines }
}

/// Highlight a single line of code, mapping the resulting style runs back to byte
/// ranges in the diff's backing text. `base` is the line's start offset in
/// `fd.text`. When `tint_bg` is set (an added/removed line), each token's colour
/// is contrast-checked against that tint and nudged toward legibility if needed,
/// so a theme's dark token colours stay readable on the green/red wash. A
/// highlighter error degrades to an un-styled line (flat fallback).
fn highlight_one(
    hl: &mut HighlightLines,
    text: &str,
    base: usize,
    set: &SyntaxSet,
    tint_bg: Option<(u8, u8, u8)>,
    truecolor: bool,
) -> StyledLine {
    // syntect's line-stateful API expects a newline-terminated line.
    let owned = format!("{text}\n");
    let Ok(regions) = hl.highlight_line(&owned, set) else {
        return StyledLine::default();
    };

    let text_len = text.len();
    let mut spans = Vec::with_capacity(regions.len());
    let mut pos = 0usize; // byte offset within `owned`, which mirrors `text`
    for (style, piece) in regions {
        let start = pos;
        pos += piece.len();
        // Clip the trailing '\n' (and anything past the real text) we appended.
        let end = pos.min(text_len);
        if start >= end {
            continue;
        }
        spans.push((
            (base + start)..(base + end),
            SynStyle {
                fg: resolve_fg(style.foreground, tint_bg, truecolor),
                modifier: font_modifier(style.font_style),
            },
        ));
    }
    StyledLine { spans }
}

/// Resolve a syntect token foreground to a terminal colour. On a tinted line,
/// raise the colour's contrast against the tint just enough to stay legible
/// before resolving — leaving its hue otherwise intact.
fn resolve_fg(
    c: syntect::highlighting::Color,
    tint_bg: Option<(u8, u8, u8)>,
    truecolor: bool,
) -> Color {
    if c.a == 0 {
        // Theme default foreground → terminal default; we can't reason about its
        // actual colour, so leave contrast handling to the terminal.
        return Color::Reset;
    }
    let mut rgb = (c.r, c.g, c.b);
    if let Some(bg) = tint_bg {
        rgb = ensure_contrast(rgb, bg, MIN_CONTRAST);
    }
    to_color(rgb, truecolor)
}

/// Map a syntect `FontStyle` bitset to ratatui text attributes.
fn font_modifier(fs: FontStyle) -> Modifier {
    let mut m = Modifier::empty();
    if fs.contains(FontStyle::BOLD) {
        m |= Modifier::BOLD;
    }
    if fs.contains(FontStyle::ITALIC) {
        m |= Modifier::ITALIC;
    }
    if fs.contains(FontStyle::UNDERLINE) {
        m |= Modifier::UNDERLINED;
    }
    m
}

/// Minimum WCAG contrast ratio a token foreground must have against a line tint
/// to be considered legible. 3.0 (rather than the 4.5 AA text threshold) keeps
/// syntax hues recognizable while still rescuing genuinely unreadable colours.
const MIN_CONTRAST: f32 = 3.0;

/// Relative luminance (WCAG) of an sRGB colour, 0.0 (black) … 1.0 (white).
fn luminance((r, g, b): (u8, u8, u8)) -> f32 {
    let lin = |c: u8| {
        let s = c as f32 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// WCAG contrast ratio between two colours, 1.0 … 21.0.
fn contrast_ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// If `fg` doesn't meet `min` contrast against `bg`, blend it toward white (on a
/// dark tint) or black (on a light tint) just far enough to reach `min`, keeping
/// as much of the original hue as possible. Returns `fg` unchanged when it
/// already passes.
fn ensure_contrast(fg: (u8, u8, u8), bg: (u8, u8, u8), min: f32) -> (u8, u8, u8) {
    if contrast_ratio(fg, bg) >= min {
        return fg;
    }
    let target = if luminance(bg) < 0.5 {
        (255, 255, 255)
    } else {
        (0, 0, 0)
    };
    // Walk toward the target in small steps; take the first that clears `min`.
    let mut candidate = fg;
    for step in 1..=10 {
        candidate = blend(fg, target, step as f32 / 10.0);
        if contrast_ratio(candidate, bg) >= min {
            break;
        }
    }
    candidate
}

/// Quantise an RGB colour to the nearest xterm-256 palette index, choosing the
/// closer of the 6×6×6 colour cube and the 24-step grayscale ramp.
fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    // Grayscale ramp (indices 232..=255) when the channels are near-equal.
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max - min < 8 {
        if r < 8 {
            return 16; // cube black, cheaper than ramp's darkest
        }
        if r > 248 {
            return 231; // cube white
        }
        let gray = ((r as u16 - 8) * 24 / 247) as u8;
        return 232 + gray;
    }
    let q = |v: u8| -> u8 {
        // 6-level cube steps: 0,95,135,175,215,255.
        if v < 48 {
            0
        } else if v < 114 {
            1
        } else {
            ((v as u16 - 35) / 40) as u8
        }
    };
    16 + 36 * q(r) + 6 * q(g) + q(b)
}

/// Look up a theme by name for highlighting; callers resolve once per file.
pub fn theme(name: &str) -> Option<&'static Theme> {
    resolve_theme(name)
}

/// Whether the terminal advertises 24-bit colour via `$COLORTERM`. When false we
/// quantise theme colours to xterm-256 so we never emit unsupported RGB escapes.
pub fn supports_truecolor() -> bool {
    std::env::var("COLORTERM")
        .map(|v| {
            let v = v.to_ascii_lowercase();
            v.contains("truecolor") || v.contains("24bit")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::git::diff::parse_unified;

    fn fd(raw: &str, path: &str) -> FileDiff {
        parse_unified(Arc::from(raw), PathBuf::from(path))
    }

    #[test]
    fn default_theme_resolves() {
        assert!(theme(DEFAULT_THEME).is_some());
        // Unknown name falls back rather than panicking.
        assert!(theme("no-such-theme").is_some());
    }

    #[test]
    fn bundled_themes_present() {
        let names = theme_names();
        assert!(names.iter().any(|n| n == DEFAULT_THEME));
        // A few of bat's curated set.
        assert!(names.iter().any(|n| n == "Dracula"));
        assert!(names.iter().any(|n| n == "Nord"));
        assert!(theme_exists("Dracula"));
        assert!(!theme_exists("definitely-not-a-theme"));
    }

    #[test]
    fn detects_rust_by_extension() {
        let set = syntaxes();
        let syn = detect(set, Path::new("src/main.rs"), None);
        assert_eq!(syn.name.to_lowercase(), "rust");
    }

    #[test]
    fn unknown_extension_falls_back_to_plain_text() {
        let set = syntaxes();
        let syn = detect(set, Path::new("mystery.zzz"), None);
        assert_eq!(syn.name, set.find_syntax_plain_text().name);
    }

    #[test]
    fn highlights_rust_chunk_into_token_spans() {
        // A real edit: the added line should split into multiple coloured tokens
        // (keyword/ident/punctuation), and every span must map back to a byte
        // range inside the line's own text.
        let raw = concat!(
            "@@ -1,2 +1,2 @@\n",
            " fn main() {\n",
            "-    let x = 1;\n",
            "+    let y = compute();\n",
        );
        let d = fd(raw, "src/main.rs");
        let theme = theme(DEFAULT_THEME).unwrap();
        let hl = highlight_file(&d, theme, true);

        let added = hl.line(0, 2).expect("highlight for the added line");
        assert!(
            added.spans.len() > 1,
            "expected multiple token spans, got {}",
            added.spans.len()
        );
        // Spans stay within the added line's byte range and are non-empty.
        let line = &d.chunks[0].lines[2];
        for (range, _) in &added.spans {
            assert!(range.start >= line.text.start && range.end <= line.text.end);
            assert!(range.start < range.end);
        }
        // The concatenation of span slices reconstructs the line text exactly.
        let joined: String = added
            .spans
            .iter()
            .map(|(r, _)| &d.text[r.clone()])
            .collect();
        assert_eq!(joined, d.slice(&line.text));
    }

    #[test]
    fn no_newline_marker_is_unstyled() {
        let raw = "@@ -1,1 +1,1 @@\n-a\n\\ No newline at end of file\n+b\n";
        let d = fd(raw, "x.rs");
        let theme = theme(DEFAULT_THEME).unwrap();
        let hl = highlight_file(&d, theme, true);
        // The marker line (index 1) produces no styled spans.
        assert!(hl.line(0, 1).unwrap().spans.is_empty());
    }

    #[test]
    fn quantises_rgb_without_truecolor() {
        // Pure red maps into the colour cube; near-gray into the ramp.
        assert_eq!(to_color((255, 0, 0), false), Color::Indexed(196));
        let gray = to_color((128, 128, 128), false);
        assert!(matches!(gray, Color::Indexed(232..=255)));
    }

    #[test]
    fn low_contrast_token_is_lifted_off_the_tint() {
        // A dark-blue token on the dark-green add tint is unreadable as-is; it
        // must be nudged to clear the minimum contrast ratio.
        let tint = (29, 58, 40); // option-B add tint over a near-black theme
        let dark_blue = (40, 40, 120);
        assert!(
            contrast_ratio(dark_blue, tint) < MIN_CONTRAST,
            "test premise: dark blue should start below threshold"
        );
        let fixed = ensure_contrast(dark_blue, tint, MIN_CONTRAST);
        assert!(
            contrast_ratio(fixed, tint) >= MIN_CONTRAST,
            "adjusted colour must clear the contrast threshold"
        );
        // A colour that already passes is left untouched.
        let bright = (220, 220, 90);
        assert_eq!(ensure_contrast(bright, tint, MIN_CONTRAST), bright);
    }
}
