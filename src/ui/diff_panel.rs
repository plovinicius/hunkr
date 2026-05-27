//! Right panel: the virtualized stacked diff.
//!
//! Only the visible window of rows is materialized into `Line`s each frame, so
//! cost is O(viewport height) no matter how large the diff is.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::app::{App, Focus, RowRef};
use crate::model::diff::LineKind;
use crate::render::viewport;

/// Tab stop width used when expanding tabs for display.
const TAB_WIDTH: usize = 4;

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Diff;

    let title = match &app.diff {
        Some(fd) => format!(" {} ", fd.path.display()),
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
            Paragraph::new("Binary file — no textual diff").style(dim),
            inner,
        );
        return;
    }
    if app.diff_rows.is_empty() {
        f.render_widget(Paragraph::new("No changes in this file").style(dim), inner);
        return;
    }

    let height = inner.height as usize;
    let total = app.diff_rows.len();
    let window = viewport::visible_range(app.scroll, height, total);

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
                let (marker, color) = match dl.kind {
                    LineKind::Add => ('+', Color::Green),
                    LineKind::Del => ('-', Color::Red),
                    LineKind::Context => (' ', Color::Gray),
                    LineKind::NoNewline => ('\\', Color::DarkGray),
                };
                let gutter = format!(
                    "{:>5} {:>5} {marker} ",
                    fmt_no(dl.old_no),
                    fmt_no(dl.new_no),
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

fn fmt_no(n: Option<u32>) -> String {
    match n {
        Some(n) => n.to_string(),
        None => String::new(),
    }
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
