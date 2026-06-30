//! Centered theme-picker overlay: a fuzzy-search input over the available theme
//! names with a scrollable list. The diff behind it live-previews the
//! highlighted theme as the cursor moves (see `App::preview_theme_at_cursor`).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::App;

/// Overlay width, in columns (capped to the available area).
const WIDTH: u16 = 40;

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let Some(picker) = &app.theme_picker else {
        return;
    };

    // Box: search row + a blank + up to `list_rows` themes + a blank + footer,
    // inside the two borders.
    let list_rows: u16 = 12;
    let width = WIDTH.min(area.width);
    let height = (list_rows + 6).min(area.height);
    let rect = centered(area, width, height);

    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Select theme ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Search input row.
    let search = Line::from(vec![
        Span::styled("search: ", Style::default().fg(Color::DarkGray)),
        Span::styled(picker.query.clone(), Style::default().fg(Color::White)),
        Span::styled("▏", Style::default().fg(Color::Cyan)),
    ]);
    f.render_widget(Paragraph::new(search), row(inner, 0));

    // Theme list, scrolled so the cursor stays visible.
    let visible = list_rows.min(inner.height.saturating_sub(3)) as usize;
    let start = scroll_start(picker.cursor, picker.matches.len(), visible);
    let mut lines = Vec::with_capacity(visible);
    if picker.matches.is_empty() {
        lines.push(Line::styled(
            "  (no matching theme)",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ));
    } else {
        for (i, name) in picker.matches.iter().enumerate().skip(start).take(visible) {
            let selected = i == picker.cursor;
            let marker = if selected { "▸ " } else { "  " };
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::styled(format!("{marker}{name}"), style));
        }
    }
    let list_area = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: visible as u16,
    };
    f.render_widget(Paragraph::new(lines), list_area);

    // Footer hints, on the last inner row.
    let footer = Rect {
        x: inner.x,
        y: inner.y + inner.height.saturating_sub(1),
        width: inner.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            "↑↓ move · ⏎ apply · esc cancel",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )),
        footer,
    );
}

/// The first list index to show so `cursor` stays within a `visible`-row window.
fn scroll_start(cursor: usize, len: usize, visible: usize) -> usize {
    if len <= visible || cursor < visible {
        0
    } else {
        (cursor + 1 - visible).min(len - visible)
    }
}

/// A single-row rect `dy` rows below the inner origin.
fn row(inner: Rect, dy: u16) -> Rect {
    Rect {
        x: inner.x,
        y: inner.y + dy,
        width: inner.width,
        height: 1,
    }
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_keeps_cursor_visible() {
        // Small list: no scrolling.
        assert_eq!(scroll_start(3, 5, 12), 0);
        // Cursor within the first window.
        assert_eq!(scroll_start(4, 30, 12), 0);
        // Cursor past the window scrolls down but never past the end.
        assert_eq!(scroll_start(15, 30, 12), 4);
        assert_eq!(scroll_start(29, 30, 12), 18);
    }
}
