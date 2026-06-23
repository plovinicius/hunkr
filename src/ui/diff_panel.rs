//! Right panel: the virtualized diff, in unified or side-by-side layout.
//!
//! Only the visible window of rows is materialized into `Line`s each frame, so
//! cost is O(viewport height) no matter how large the diff is.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Focus, ReviewedDisplay, RowRef, SideRow, ViewMode};
use crate::glyphs::Glyphs;
use crate::model::diff::{FileDiff, LineKind};
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
                let content = expand_tabs(fd.slice(&dl.text));
                let (marker, color) = line_marker(dl.kind);
                let gutter = format!(
                    "{:>5} {:>5} {marker} ",
                    fmt_no(dl.old_no),
                    fmt_no(dl.new_no)
                );
                lines.push(body_line(
                    h == app.current_chunk,
                    dim_reviewed && app.chunk_reviewed(h),
                    gutter,
                    content,
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
                let (lc, ls) = side_cell(fd, left, left_w, Side::Old, dimmed);
                let (rc, rs) = side_cell(fd, right, right_w, Side::New, dimmed);
                if current {
                    let bg = CURRENT_CHUNK_BODY_BG;
                    lines.push(Line::from(vec![
                        Span::styled(
                            lead.to_string(),
                            Style::default().fg(CURRENT_CHUNK_BAR).bg(bg),
                        ),
                        Span::styled(lc, ls.bg(bg)),
                        Span::styled("│", divider.bg(bg)),
                        Span::styled(rc, rs.bg(bg)),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled(lead.to_string(), divider),
                        Span::styled(lc, ls),
                        Span::styled("│", divider),
                        Span::styled(rc, rs),
                    ]));
                }
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

/// Render one side of a side-by-side row, padded/truncated to `width` columns.
/// An absent line yields a blank cell.
fn side_cell(
    fd: &FileDiff,
    cell: Option<(usize, usize)>,
    width: usize,
    side: Side,
    dimmed: bool,
) -> (String, Style) {
    let Some((h, l)) = cell else {
        return (" ".repeat(width), Style::default());
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
    let content = expand_tabs(fd.slice(&dl.text));
    let text = fit(&format!("{:>4} {marker} {content}", fmt_no(no)), width);
    let color = if dimmed { Color::DarkGray } else { color };
    (text, Style::default().fg(color))
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
fn body_line<'a>(
    current: bool,
    dimmed: bool,
    gutter: String,
    content: String,
    color: Color,
    g: &Glyphs,
    width: usize,
) -> Line<'a> {
    let dim = Style::default().fg(Color::DarkGray);
    let lead = if current { g.chunk_bar } else { ' ' };
    // In "dim" mode a reviewed chunk's lines are greyed out rather than folded.
    let color = if dimmed { Color::DarkGray } else { color };
    if current {
        let bg = CURRENT_CHUNK_BODY_BG;
        // Pad the content so the wash fills the row out to the right edge.
        let pad = width.saturating_sub(1 + gutter.width());
        Line::from(vec![
            Span::styled(
                lead.to_string(),
                Style::default().fg(CURRENT_CHUNK_BAR).bg(bg),
            ),
            Span::styled(gutter, dim.bg(bg)),
            Span::styled(fit(&content, pad), Style::default().fg(color).bg(bg)),
        ])
    } else {
        Line::from(vec![
            Span::styled(lead.to_string(), dim),
            Span::styled(gutter, dim),
            Span::styled(content, Style::default().fg(color)),
        ])
    }
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

/// Expand tabs to the next tab stop using display width so columns line up,
/// and neutralize any other control characters. Diff content is untrusted repo
/// text; a raw `ESC`/`BEL` could be interpreted by the terminal as an escape
/// sequence, so every control byte (other than tab, expanded here) is replaced
/// with the single-width replacement char.
fn expand_tabs(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut col = 0;
    for ch in s.chars() {
        if ch == '\t' {
            let spaces = TAB_WIDTH - (col % TAB_WIDTH);
            out.extend(std::iter::repeat_n(' ', spaces));
            col += spaces;
        } else if ch.is_control() {
            out.push('\u{FFFD}');
            col += 1;
        } else {
            out.push(ch);
            col += ch.width().unwrap_or(0);
        }
    }
    out
}
