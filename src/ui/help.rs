//! Centered help overlay listing the keybindings.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

const KEYS: &[(&str, &str)] = &[
    ("j / k / arrows", "move cursor / scroll diff"),
    ("Shift+arrows", "page up / down (also PgUp/PgDn)"),
    ("mouse wheel", "scroll diff / move cursor"),
    ("n / p", "next / previous hunk"),
    ("] / [", "next / previous file"),
    ("s", "toggle unified / side-by-side"),
    ("g / G", "top / bottom of diff"),
    ("Tab", "switch tree / diff focus"),
    ("Enter", "expand-collapse folder / focus diff"),
    ("r / u", "mark / unmark reviewed"),
    ("y", "copy AI reference for the hunk"),
    ("e", "open file in $EDITOR at the line"),
    ("/", "filter files (Esc to clear)"),
    ("?", "toggle this help"),
    ("q", "quit"),
];

pub fn render(f: &mut Frame, area: Rect) {
    // Content rows: a leading blank + one per key + a blank + the footer,
    // plus the two border rows.
    let width = 54u16.min(area.width);
    let height = (KEYS.len() as u16 + 5).min(area.height);
    let rect = centered(area, width, height);

    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Help ");

    let mut lines = vec![Line::raw("")];
    for (k, d) in KEYS {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<8}"), Style::default().fg(Color::Yellow)),
            Span::raw(format!("  {d}")),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  press any key to close",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    ));

    f.render_widget(Paragraph::new(lines).block(block), rect);
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    }
}
