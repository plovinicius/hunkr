//! Right panel: the virtualized diff, in unified or side-by-side layout.
//!
//! Only the visible window of rows is materialized into `Line`s each frame, so
//! cost is O(viewport height) no matter how large the diff is.

use std::ops::Range;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Focus, ReviewedDisplay, RowRef, SideRow, ViewMode};
use crate::glyphs::Glyphs;
use crate::highlight::{FileHighlight, StyledLine};
use crate::model::diff::{DiffLine, FileDiff, LineKind};
use crate::render::{sanitize, viewport};

/// Tab stop width used when expanding tabs for display.
const TAB_WIDTH: usize = 4;

/// Background fill behind the current chunk's header row — the brightest part of
/// the active block.
const CURRENT_CHUNK_HEADER_BG: Color = Color::Indexed(238);
/// Subtler fill behind the current chunk's body rows, so the whole chunk reads as
/// one active block without washing out the `+`/`-` colours.
const CURRENT_CHUNK_BODY_BG: Color = Color::Indexed(236);
/// Colour of the left accent bar marking the current chunk's left edge.
const CURRENT_CHUNK_BAR: Color = Color::Yellow;

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Diff;
    let mode = match app.view {
        ViewMode::Unified => "unified",
        ViewMode::SideBySide => "side-by-side",
    };

    let title = match &app.diff {
        // The path is untrusted (a repo can name a file with embedded escape
        // sequences) and the Block-title render path writes cell symbols
        // verbatim, so sanitize before it reaches the terminal.
        Some(fd) => format!(" {} [{mode}] ", sanitize(&fd.path.display().to_string())),
        None => " diff ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        })
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let dim = Style::default().fg(Color::DarkGray);

    let Some(fd) = &app.diff else {
        f.render_widget(
            Paragraph::new("Select a file to view its diff").style(dim),
            inner,
        );
        return;
    };
    if fd.is_binary {
        f.render_widget(
            Paragraph::new("Binary file - no textual diff").style(dim),
            inner,
        );
        return;
    }
    if app.diff_rows.is_empty() {
        f.render_widget(Paragraph::new("No changes in this file").style(dim), inner);
        return;
    }

    match app.view {
        ViewMode::Unified => render_unified(f, inner, app, fd),
        ViewMode::SideBySide => render_side_by_side(f, inner, app, fd),
    }
}

fn render_unified(f: &mut Frame, inner: Rect, app: &App, fd: &FileDiff) {
    let dim_reviewed = app.config.reviewed_chunks == ReviewedDisplay::Dim;
    let highlight = app.highlight.as_deref();
    let tints = highlight.map(|h| (h.add_bg, h.del_bg));
    let height = inner.height as usize;
    let window = viewport::visible_range(app.scroll, height, app.diff_rows.len());

    let mut lines = Vec::with_capacity(window.len());
    for &row in &app.diff_rows[window] {
        match row {
            RowRef::Header(h) => {
                let text = sanitize(fd.slice(&fd.chunks[h].header));
                lines.push(header_line(
                    &text,
                    h == app.current_chunk,
                    app.chunk_reviewed(h),
                    collapsed_lines(app, fd, h),
                    &app.glyphs,
                    inner.width as usize,
                ));
            }
            RowRef::Line(h, l) => {
                let dl = &fd.chunks[h].lines[l];
                let (marker, color) = line_marker(dl.kind);
                let gutter = format!(
                    "{:>5} {:>5} {marker} ",
                    fmt_no(dl.old_no),
                    fmt_no(dl.new_no)
                );
                lines.push(body_line(
                    fd,
                    dl,
                    highlight.and_then(|hl| hl.line(h, l)),
                    h == app.current_chunk,
                    dim_reviewed && app.chunk_reviewed(h),
                    tints,
                    gutter,
                    color,
                    &app.glyphs,
                    inner.width as usize,
                ));
            }
        }
    }

    f.render_widget(Paragraph::new(lines), inner);
}

/// `Some(n)` — with `n` the number of hidden body lines — when chunk `h` is
/// collapsed; `None` when it's expanded.
fn collapsed_lines(app: &App, fd: &FileDiff, h: usize) -> Option<usize> {
    if app.chunk_collapsed.get(h).copied().unwrap_or(false) {
        Some(fd.chunks[h].lines.len())
    } else {
        None
    }
}

fn render_side_by_side(f: &mut Frame, inner: Rect, app: &App, fd: &FileDiff) {
    let width = inner.width as usize;
    if width < 4 {
        return;
    }
    // Reserve one column for the current-chunk accent bar; the rest splits into
    // two equal columns separated by a single divider column.
    let content_w = width - 1;
    let left_w = (content_w - 1) / 2;
    let right_w = content_w - 1 - left_w;

    let height = inner.height as usize;
    let window = viewport::visible_range(app.scroll, height, app.side_rows.len());
    let divider = Style::default().fg(Color::DarkGray);
    let dim_reviewed = app.config.reviewed_chunks == ReviewedDisplay::Dim;
    let highlight = app.highlight.as_deref();

    let mut lines = Vec::with_capacity(window.len());
    for &row in &app.side_rows[window] {
        match row {
            SideRow::Header(h) => {
                let text = sanitize(fd.slice(&fd.chunks[h].header));
                lines.push(header_line(
                    &text,
                    h == app.current_chunk,
                    app.chunk_reviewed(h),
                    collapsed_lines(app, fd, h),
                    &app.glyphs,
                    width,
                ));
            }
            SideRow::Pair { left, right } => {
                let chunk = left.or(right).map(|(h, _)| h);
                let current = chunk == Some(app.current_chunk);
                let dimmed = dim_reviewed && chunk.is_some_and(|h| app.chunk_reviewed(h));
                let lead = if current { app.glyphs.chunk_bar } else { ' ' };
                let chrome_bg = if current {
                    Some(CURRENT_CHUNK_BODY_BG)
                } else {
                    None
                };
                let mut spans = Vec::new();
                let mut lead_style = Style::default().fg(if current {
                    CURRENT_CHUNK_BAR
                } else {
                    Color::DarkGray
                });
                if let Some(bg) = chrome_bg {
                    lead_style = lead_style.bg(bg);
                }
                spans.push(Span::styled(lead.to_string(), lead_style));
                spans.extend(side_cell(
                    fd,
                    left,
                    left_w,
                    Side::Old,
                    dimmed,
                    current,
                    highlight,
                ));
                let mut div_style = divider;
                if let Some(bg) = chrome_bg {
                    div_style = div_style.bg(bg);
                }
                spans.push(Span::styled("│", div_style));
                spans.extend(side_cell(
                    fd,
                    right,
                    right_w,
                    Side::New,
                    dimmed,
                    current,
                    highlight,
                ));
                lines.push(Line::from(spans));
            }
        }
    }

    f.render_widget(Paragraph::new(lines), inner);
}

#[derive(Clone, Copy)]
enum Side {
    Old,
    New,
}

/// Render one side of a side-by-side row as styled spans, filling exactly
/// `width` columns. An absent line yields a blank cell. The line-number gutter
/// keeps the side's diff colour (red on the old side's deletions, green on the
/// new side's additions); code content is syntax-highlighted (with a faint
/// add/del background tint) when `highlight` is present, else flat-coloured.
#[allow(clippy::too_many_arguments)]
fn side_cell(
    fd: &FileDiff,
    cell: Option<(usize, usize)>,
    width: usize,
    side: Side,
    dimmed: bool,
    current: bool,
    highlight: Option<&FileHighlight>,
) -> Vec<Span<'static>> {
    let tints = highlight.map(|h| (h.add_bg, h.del_bg));
    let wash = if current {
        Some(CURRENT_CHUNK_BODY_BG)
    } else {
        None
    };
    let Some((h, l)) = cell else {
        // Blank cell: still carry the current-chunk wash so the block reads as one.
        let mut style = Style::default();
        if let Some(bg) = wash {
            style = style.bg(bg);
        }
        return vec![Span::styled(" ".repeat(width), style)];
    };
    let dl = &fd.chunks[h].lines[l];
    let (no, marker, color) = match side {
        Side::Old => (
            dl.old_no,
            if dl.kind == LineKind::Del { '-' } else { ' ' },
            if dl.kind == LineKind::Del {
                Color::Red
            } else {
                Color::Gray
            },
        ),
        Side::New => (
            dl.new_no,
            if dl.kind == LineKind::Add { '+' } else { ' ' },
            if dl.kind == LineKind::Add {
                Color::Green
            } else {
                Color::Gray
            },
        ),
    };
    let cell_bg = line_bg(dl.kind, current, dimmed, tints);
    let gutter = format!("{:>4} {marker} ", fmt_no(no));
    let gutter_w = gutter.width();
    let gutter_fg = if dimmed { Color::DarkGray } else { color };
    let mut gutter_style = Style::default().fg(gutter_fg);
    if let Some(bg) = cell_bg {
        gutter_style = gutter_style.bg(bg);
    }

    let mut spans = vec![Span::styled(gutter, gutter_style)];
    let max_cols = width.saturating_sub(gutter_w);
    spans.extend(styled_content(
        fd,
        dl,
        highlight.and_then(|hl| hl.line(h, l)),
        color,
        dimmed,
        cell_bg,
        max_cols,
    ));
    spans
}

/// Build a styled chunk-header line. The chunk the `n`/`p` cursor is on gets the
/// accent bar, a contrasting colour, and a full-width background fill so it's
/// obvious which change is selected; others keep a blank lead column so the
/// header text doesn't shift as you navigate. The line is padded/truncated to
/// `width` so the highlight fills the row.
fn header_line<'a>(
    text: &str,
    current: bool,
    reviewed: bool,
    hidden: Option<usize>,
    g: &Glyphs,
    width: usize,
) -> Line<'a> {
    let lead = if current { g.chunk_bar } else { ' ' };
    // A ✓ prefix marks a reviewed chunk; a collapsed one also shows how many
    // body lines are folded away.
    let mark = if reviewed {
        format!("{} ", g.reviewed)
    } else {
        String::new()
    };
    let suffix = match hidden {
        Some(n) if n > 0 => format!("   {n} {}", if n == 1 { "line" } else { "lines" }),
        _ => String::new(),
    };
    let body = fit(&format!("{lead} {mark}{text}{suffix}"), width);
    let style = if current {
        Style::default()
            .fg(Color::Yellow)
            .bg(CURRENT_CHUNK_HEADER_BG)
            .add_modifier(Modifier::BOLD)
    } else if reviewed {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    };
    Line::styled(body, style)
}

/// Build a styled unified body row. Rows inside the current chunk get the left
/// accent bar and a subtle full-row background wash, so the whole chunk reads as
/// one active block. The leading bar column is present (as a blank) on every
/// row so content stays vertically aligned as the cursor moves between chunks.
///
/// When `tints` is set (syntax highlighting on), added/removed rows carry the
/// theme-derived green/red background and the code shows its syntax colours in
/// the foreground; `styled` holds the per-token colours (absent → flat `base_fg`).
#[allow(clippy::too_many_arguments)]
fn body_line(
    fd: &FileDiff,
    dl: &DiffLine,
    styled: Option<&StyledLine>,
    current: bool,
    dimmed: bool,
    tints: Option<(Color, Color)>,
    gutter: String,
    base_fg: Color,
    g: &Glyphs,
    width: usize,
) -> Line<'static> {
    let lead = if current { g.chunk_bar } else { ' ' };
    let chrome_bg = if current {
        Some(CURRENT_CHUNK_BODY_BG)
    } else {
        None
    };
    let content_bg = line_bg(dl.kind, current, dimmed, tints);

    let mut lead_style = Style::default().fg(if current {
        CURRENT_CHUNK_BAR
    } else {
        Color::DarkGray
    });
    let mut gutter_style = Style::default().fg(Color::DarkGray);
    if let Some(bg) = chrome_bg {
        lead_style = lead_style.bg(bg);
        gutter_style = gutter_style.bg(bg);
    }

    let gutter_w = gutter.width();
    let mut spans = vec![
        Span::styled(lead.to_string(), lead_style),
        Span::styled(gutter, gutter_style),
    ];
    let max_cols = width.saturating_sub(1 + gutter_w);
    spans.extend(styled_content(
        fd, dl, styled, base_fg, dimmed, content_bg, max_cols,
    ));
    Line::from(spans)
}

/// The background fill for a body row's *content*. When highlighting is on,
/// `tints` carries the theme-derived `(add, del)` colours; added/removed rows get
/// the matching tint. Otherwise only the current chunk's wash applies. A dimmed
/// (reviewed) row drops the tint so it stays visually quiet.
fn line_bg(
    kind: LineKind,
    current: bool,
    dimmed: bool,
    tints: Option<(Color, Color)>,
) -> Option<Color> {
    match tints {
        Some((add, _)) if !dimmed && kind == LineKind::Add => Some(add),
        Some((_, del)) if !dimmed && kind == LineKind::Del => Some(del),
        _ if current => Some(CURRENT_CHUNK_BODY_BG),
        _ => None,
    }
}

/// Build the content spans for a diff line: one `Span` per syntax token (or a
/// single flat span when `styled` is absent/empty), expanding tabs and
/// sanitizing control characters across a shared display-column counter so tab
/// stops line up across token boundaries. Truncates to `max_cols`; if `bg` is
/// set, pads with spaces so the background fills the row's content area.
fn styled_content(
    fd: &FileDiff,
    dl: &DiffLine,
    styled: Option<&StyledLine>,
    base_fg: Color,
    dimmed: bool,
    bg: Option<Color>,
    max_cols: usize,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut col = 0usize;

    // Token list: highlighter spans when present, else the whole line as one
    // flat-coloured token.
    let tokens: Vec<(Range<usize>, Color, Modifier)> = match styled {
        Some(sl) if !sl.spans.is_empty() => sl
            .spans
            .iter()
            .map(|(r, st)| (r.clone(), st.fg, st.modifier))
            .collect(),
        _ => vec![(dl.text.clone(), base_fg, Modifier::empty())],
    };

    for (range, fg, modifier) in tokens {
        let mut out = String::new();
        let hit_cap = push_expanded(&mut out, &fd.text[range], &mut col, max_cols);
        if !out.is_empty() {
            let mut style = Style::default().fg(if dimmed { Color::DarkGray } else { fg });
            if !dimmed {
                style = style.add_modifier(modifier);
            }
            if let Some(bg) = bg {
                style = style.bg(bg);
            }
            spans.push(Span::styled(out, style));
        }
        if hit_cap {
            break;
        }
    }

    // Pad so the tint/wash background reaches the right edge of the content area.
    if let Some(bg) = bg
        && col < max_cols
    {
        spans.push(Span::styled(
            " ".repeat(max_cols - col),
            Style::default().bg(bg),
        ));
    }
    spans
}

fn line_marker(kind: LineKind) -> (char, Color) {
    match kind {
        LineKind::Add => ('+', Color::Green),
        LineKind::Del => ('-', Color::Red),
        LineKind::Context => (' ', Color::Gray),
        LineKind::NoNewline => ('\\', Color::DarkGray),
    }
}

fn fmt_no(n: Option<u32>) -> String {
    match n {
        Some(n) => n.to_string(),
        None => String::new(),
    }
}

/// Truncate `s` to exactly `width` display columns, padding with spaces.
fn fit(s: &str, width: usize) -> String {
    let mut out = String::with_capacity(width);
    let mut used = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > width {
            break;
        }
        out.push(ch);
        used += cw;
    }
    for _ in used..width {
        out.push(' ');
    }
    out
}

/// Expand `s` into `out` for display, advancing the shared display column `col`
/// and stopping before it would exceed `max_cols`. Tabs expand to the next tab
/// stop (so columns line up across token boundaries via the shared `col`); every
/// other control character is replaced with the single-width replacement char.
///
/// Diff content is untrusted repo text — a raw `ESC`/`BEL` could be interpreted
/// by the terminal as an escape sequence — so neutralizing control bytes here
/// closes that off for the highlighted render path too.
///
/// Returns `true` if the column budget was reached (the caller should stop
/// emitting further tokens for this line).
fn push_expanded(out: &mut String, s: &str, col: &mut usize, max_cols: usize) -> bool {
    for ch in s.chars() {
        if ch == '\t' {
            let spaces = TAB_WIDTH - (*col % TAB_WIDTH);
            for _ in 0..spaces {
                if *col >= max_cols {
                    return true;
                }
                out.push(' ');
                *col += 1;
            }
        } else if ch.is_control() {
            if *col >= max_cols {
                return true;
            }
            out.push('\u{FFFD}');
            *col += 1;
        } else {
            let w = ch.width().unwrap_or(0);
            if *col + w > max_cols {
                return true;
            }
            out.push(ch);
            *col += w;
        }
    }
    false
}
