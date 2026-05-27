//! Right panel: the virtualized diff, in unified or side-by-side layout.
//!
//! Only the visible window of rows is materialized into `Line`s each frame, so
//! cost is O(viewport height) no matter how large the diff is.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::app::{App, Focus, RowRef, SideRow, ViewMode};
use crate::model::diff::{FileDiff, LineKind};
use crate::render::viewport;

/// Tab stop width used when expanding tabs for display.
const TAB_WIDTH: usize = 4;

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Diff;
    let mode = match app.view {
        ViewMode::Unified => "unified",
        ViewMode::SideBySide => "side-by-side",
    };

    let title = match &app.diff {
        Some(fd) => format!(" {} [{mode}] ", fd.path.display()),
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
                let text = fd.slice(&fd.hunks[h].header);
                lines.push(Line::styled(
                    text.to_string(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
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
                lines.push(Line::from(vec![
                    Span::styled(gutter, dim),
                    Span::styled(content, Style::default().fg(color)),
                ]));
            }
        }
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn render_side_by_side(f: &mut Frame, inner: Rect, app: &App, fd: &FileDiff) {
    let width = inner.width as usize;
    if width < 3 {
        return;
    }
    // Two equal columns separated by a single divider column.
    let left_w = (width - 1) / 2;
    let right_w = width - 1 - left_w;

    let height = inner.height as usize;
    let window = viewport::visible_range(app.scroll, height, app.side_rows.len());
    let divider = Style::default().fg(Color::DarkGray);

    let mut lines = Vec::with_capacity(window.len());
    for &row in &app.side_rows[window] {
        match row {
            SideRow::Header(h) => {
                let text = fit(fd.slice(&fd.hunks[h].header), width);
                lines.push(Line::styled(
                    text,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            SideRow::Pair { left, right } => {
                let (lc, ls) = side_cell(fd, left, left_w, Side::Old);
                let (rc, rs) = side_cell(fd, right, right_w, Side::New);
                lines.push(Line::from(vec![
                    Span::styled(lc, ls),
                    Span::styled("│", divider),
                    Span::styled(rc, rs),
                ]));
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

/// Expand tabs to the next tab stop using display width so columns line up.
fn expand_tabs(s: &str) -> String {
    if !s.contains('\t') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    let mut col = 0;
    for ch in s.chars() {
        if ch == '\t' {
            let spaces = TAB_WIDTH - (col % TAB_WIDTH);
            out.extend(std::iter::repeat_n(' ', spaces));
            col += spaces;
        } else {
            out.push(ch);
            col += ch.width().unwrap_or(0);
        }
    }
    out
}
