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

use crate::app::{App, Focus, RowRef, SideRow, ViewMode};
use crate::glyphs::Glyphs;
use crate::model::diff::{FileDiff, LineKind};
use crate::render::{sanitize, viewport};

/// Tab stop width used when expanding tabs for display.
const TAB_WIDTH: usize = 4;

/// Background fill behind the current hunk's header row — the brightest part of
/// the active block.
const CURRENT_HUNK_HEADER_BG: Color = Color::Indexed(238);
/// Subtler fill behind the current hunk's body rows, so the whole hunk reads as
/// one active block without washing out the `+`/`-` colours.
const CURRENT_HUNK_BODY_BG: Color = Color::Indexed(236);
/// Colour of the left accent bar marking the current hunk's left edge.
const CURRENT_HUNK_BAR: Color = Color::Yellow;

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
    let dim = Style::default().fg(Color::DarkGray);
    let height = inner.height as usize;
    let window = viewport::visible_range(app.scroll, height, app.diff_rows.len());

    let mut lines = Vec::with_capacity(window.len());
    for &row in &app.diff_rows[window] {
        match row {
            RowRef::Header(h) => {
                let text = sanitize(fd.slice(&fd.hunks[h].header));
                lines.push(header_line(
                    &text,
                    h == app.current_hunk,
                    &app.glyphs,
                    inner.width as usize,
                ));
            }
            RowRef::Line(h, l) => {
                let dl = &fd.hunks[h].lines[l];
                let content = expand_tabs(fd.slice(&dl.text));
                let (marker, color) = line_marker(dl.kind);
                let gutter = format!(
                    "{:>5} {:>5} {marker} ",
                    fmt_no(dl.old_no),
                    fmt_no(dl.new_no)
                );
                lines.push(body_line(
                    h == app.current_hunk,
                    gutter,
                    content,
                    color,
                    &app.glyphs,
                    inner.width as usize,
                    dim,
                ));
            }
        }
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn render_side_by_side(f: &mut Frame, inner: Rect, app: &App, fd: &FileDiff) {
    let width = inner.width as usize;
    if width < 4 {
        return;
    }
    // Reserve one column for the current-hunk accent bar; the rest splits into
    // two equal columns separated by a single divider column.
    let content_w = width - 1;
    let left_w = (content_w - 1) / 2;
    let right_w = content_w - 1 - left_w;

    let height = inner.height as usize;
    let window = viewport::visible_range(app.scroll, height, app.side_rows.len());
    let divider = Style::default().fg(Color::DarkGray);

    let mut lines = Vec::with_capacity(window.len());
    for &row in &app.side_rows[window] {
        match row {
            SideRow::Header(h) => {
                let text = sanitize(fd.slice(&fd.hunks[h].header));
                lines.push(header_line(
                    &text,
                    h == app.current_hunk,
                    &app.glyphs,
                    width,
                ));
            }
            SideRow::Pair { left, right } => {
                let current = left.or(right).map(|(h, _)| h) == Some(app.current_hunk);
                let lead = if current { app.glyphs.hunk_bar } else { ' ' };
                let (lc, ls) = side_cell(fd, left, left_w, Side::Old);
                let (rc, rs) = side_cell(fd, right, right_w, Side::New);
                if current {
                    let bg = CURRENT_HUNK_BODY_BG;
                    lines.push(Line::from(vec![
                        Span::styled(
                            lead.to_string(),
                            Style::default().fg(CURRENT_HUNK_BAR).bg(bg),
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
) -> (String, Style) {
    let Some((h, l)) = cell else {
        return (" ".repeat(width), Style::default());
    };
    let dl = &fd.hunks[h].lines[l];
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
    (text, Style::default().fg(color))
}

/// Build a styled hunk-header line. The hunk the `n`/`p` cursor is on gets the
/// accent bar, a contrasting colour, and a full-width background fill so it's
/// obvious which change is selected; others keep a blank lead column so the
/// header text doesn't shift as you navigate. The line is padded/truncated to
/// `width` so the highlight fills the row.
fn header_line<'a>(text: &str, current: bool, g: &Glyphs, width: usize) -> Line<'a> {
    let lead = if current { g.hunk_bar } else { ' ' };
    let body = fit(&format!("{lead} {text}"), width);
    let style = if current {
        Style::default()
            .fg(Color::Yellow)
            .bg(CURRENT_HUNK_HEADER_BG)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    };
    Line::styled(body, style)
}

/// Build a styled unified body row. Rows inside the current hunk get the left
/// accent bar and a subtle full-row background wash, so the whole hunk reads as
/// one active block. The leading bar column is present (as a blank) on every
/// row so content stays vertically aligned as the cursor moves between hunks.
fn body_line<'a>(
    current: bool,
    gutter: String,
    content: String,
    color: Color,
    g: &Glyphs,
    width: usize,
    dim: Style,
) -> Line<'a> {
    let lead = if current { g.hunk_bar } else { ' ' };
    if current {
        let bg = CURRENT_HUNK_BODY_BG;
        // Pad the content so the wash fills the row out to the right edge.
        let pad = width.saturating_sub(1 + gutter.width());
        Line::from(vec![
            Span::styled(
                lead.to_string(),
                Style::default().fg(CURRENT_HUNK_BAR).bg(bg),
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
