//! Floating top-right notification. Currently used for the persistent config
//! error: it stays pinned in the corner (over the diff panel) until the config
//! is fixed, so it doesn't have to fight the status bar for the bottom row.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::render::sanitize;

/// Draw the config-error toast in the top-right corner of `area`. `edit_key` is
/// the chord bound to "edit config" (so the hint tracks a rebind).
pub fn render_config_error(f: &mut Frame, area: Rect, message: &str, edit_key: &str) {
    // A 1-cell inset from the top-right edge.
    let margin = 1u16;
    let width = 50u16.min(area.width.saturating_sub(margin + 1));
    // Too cramped to draw a useful box — skip rather than render garbage.
    if width < 16 || area.height < 6 {
        return;
    }

    let wrap_w = (width as usize).saturating_sub(4).max(1); // borders + 1-col padding
    let body = sanitize(message);
    let hint = format!("press {edit_key} to edit · fix to dismiss");

    let mut lines: Vec<Line> = wrap(&body, wrap_w)
        .into_iter()
        .map(|l| Line::from(format!(" {l}")))
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        format!(" {hint}"),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    ));

    let height = (lines.len() as u16 + 2).min(area.height); // + top/bottom border
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width + margin),
        y: area.y + margin,
        width,
        height,
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .title(Span::styled(
            " ⚠ Config error ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));

    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::default().bg(Color::Indexed(236)).fg(Color::White)),
        rect,
    );
}

/// Greedy word-wrap by display columns (char count is fine here — the message is
/// ASCII after `sanitize`). Over-long words are hard-split so nothing overflows.
fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;

    for word in s.split_whitespace() {
        let wlen = word.chars().count();
        if wlen > width {
            if cur_len > 0 {
                lines.push(std::mem::take(&mut cur));
            }
            let mut chars: Vec<char> = word.chars().collect();
            while chars.len() > width {
                lines.push(chars[..width].iter().collect());
                chars.drain(..width);
            }
            cur = chars.iter().collect();
            cur_len = chars.len();
        } else if cur_len == 0 {
            cur = word.to_string();
            cur_len = wlen;
        } else if cur_len + 1 + wlen <= width {
            cur.push(' ');
            cur.push_str(word);
            cur_len += 1 + wlen;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
            cur_len = wlen;
        }
    }
    if cur_len > 0 || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_breaks_on_words_and_hard_splits_long_tokens() {
        let out = wrap("parse error at line 2", 10);
        assert!(
            out.iter().all(|l| l.chars().count() <= 10),
            "lines exceed width: {out:?}"
        );
        assert!(out.len() >= 2);

        // A single token longer than the width is hard-split, never dropped.
        let out = wrap("supercalifragilistic", 6);
        assert!(out.iter().all(|l| l.chars().count() <= 6));
        assert_eq!(out.concat(), "supercalifragilistic");
    }
}
