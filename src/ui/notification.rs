//! Floating top-right notifications. Two callers today:
//! - the **persistent** config-error toast (red, stays until the config is fixed)
//! - a **transient** toast (green) for quick acknowledgements like "copied for AI"
//!
//! Both anchor to the top-right corner and stack downward, so a copy toast that
//! fires while the config is broken sits just below the error box.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::render::sanitize;

const MARGIN: u16 = 1;
const MAX_WIDTH: u16 = 50;

/// Draw the persistent config-error toast starting at `top_y`. Returns the `y`
/// just below it (for stacking), or `top_y` unchanged if it didn't fit.
pub fn render_config_error(
    f: &mut Frame,
    area: Rect,
    top_y: u16,
    message: &str,
    edit_key: &str,
) -> u16 {
    let width = box_width(area);
    let wrap_w = wrap_width(width);
    let mut body: Vec<Line> = wrap(&sanitize(message), wrap_w)
        .into_iter()
        .map(|l| Line::from(format!(" {l}")))
        .collect();
    body.push(Line::raw(""));
    body.push(Line::styled(
        format!(" press {edit_key} to edit · fix to dismiss"),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    ));
    draw(f, area, top_y, " ⚠ Config error ", Color::Red, body)
}

/// Draw a transient toast (green) starting at `top_y`.
pub fn render_toast(f: &mut Frame, area: Rect, top_y: u16, text: &str) -> u16 {
    let width = box_width(area);
    let wrap_w = wrap_width(width);
    let body: Vec<Line> = wrap(&sanitize(text), wrap_w)
        .into_iter()
        .map(|l| Line::from(format!(" {l}")))
        .collect();
    draw(f, area, top_y, " ✓ Copied ", Color::Green, body)
}

/// Render a bordered box of `body` lines in the top-right, titled and colored.
/// Returns the `y` immediately below it (plus a one-row gap) for stacking.
fn draw(f: &mut Frame, area: Rect, top_y: u16, title: &str, accent: Color, body: Vec<Line>) -> u16 {
    let width = box_width(area);
    let bottom = area.y + area.height;
    if width < 16 || top_y + 4 > bottom {
        return top_y; // too small / no vertical room — skip
    }
    let height = (body.len() as u16 + 2).min(bottom - top_y);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width + MARGIN),
        y: top_y,
        width,
        height,
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(accent))
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ));

    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(body)
            .block(block)
            .style(Style::default().bg(Color::Indexed(236)).fg(Color::White)),
        rect,
    );

    top_y + height + 1
}

fn box_width(area: Rect) -> u16 {
    MAX_WIDTH.min(area.width.saturating_sub(MARGIN + 1))
}

fn wrap_width(width: u16) -> usize {
    (width as usize).saturating_sub(4).max(1) // borders + 1-col padding
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
