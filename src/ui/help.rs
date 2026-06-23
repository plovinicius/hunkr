//! Centered help overlay listing the keybindings.
//!
//! The bound chords are read from the live keymap so the overlay reflects the
//! user's config (a remapped or extra binding shows up here automatically).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::App;
use crate::config::Action;

/// Each action with its human description, in display order. The keys shown
/// next to each are resolved from the active keymap at render time.
const ROWS: &[(Action, &str)] = &[
    (Action::ScrollDown, "scroll diff / move cursor down"),
    (Action::ScrollUp, "scroll diff / move cursor up"),
    (Action::PageDown, "page down"),
    (Action::PageUp, "page up"),
    (Action::NextChunk, "next chunk"),
    (Action::PrevChunk, "previous chunk"),
    (Action::NextFile, "next file"),
    (Action::PrevFile, "previous file"),
    (Action::Top, "top of diff"),
    (Action::Bottom, "bottom of diff"),
    (Action::SwitchFocus, "switch tree / diff focus"),
    (Action::Activate, "expand-collapse folder / focus diff"),
    (Action::NarrowSidebar, "narrow sidebar (drag divider too)"),
    (Action::WidenSidebar, "widen sidebar"),
    (Action::ToggleView, "toggle unified / side-by-side"),
    (
        Action::ToggleChunkReviewed,
        "toggle reviewed (current chunk)",
    ),
    (Action::ToggleReviewed, "toggle reviewed (whole file)"),
    (Action::ExpandAllChunks, "reveal all collapsed chunks"),
    (Action::ToggleHidden, "hide / un-hide the selected file"),
    (Action::ToggleHiddenView, "toggle the hidden-files view"),
    (Action::CopyReference, "copy AI reference for the chunk"),
    (Action::OpenEditor, "open file in $EDITOR at the line"),
    (Action::EditConfig, "edit the config file"),
    (Action::StartFilter, "filter files (Esc to clear)"),
    (Action::Help, "toggle this help"),
    (Action::Quit, "quit"),
];

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let keymap = &app.config.keys;
    let entries: Vec<(String, &str)> = ROWS
        .iter()
        .map(|(action, desc)| {
            let chords = keymap.chords_for(*action);
            let keys = if chords.is_empty() {
                "—".to_string()
            } else {
                chords.join(" / ")
            };
            (keys, *desc)
        })
        .collect();

    let key_w = entries
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    let desc_w = entries
        .iter()
        .map(|(_, d)| d.chars().count())
        .max()
        .unwrap_or(0);

    // Content rows: a leading blank + one per entry + a blank + the footer,
    // plus the two border rows. Width fits the widest key + description.
    let width = ((key_w + desc_w + 8) as u16).min(area.width);
    let height = (entries.len() as u16 + 5).min(area.height);
    let rect = centered(area, width, height);

    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Help ");

    let mut lines = vec![Line::raw("")];
    for (k, d) in &entries {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {k:<width$}", width = key_w),
                Style::default().fg(Color::Yellow),
            ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_lists_every_action() {
        // Guards against the overlay drifting out of sync when a new `Action` is
        // added — every action must have a help row.
        for action in Action::ALL {
            assert!(
                ROWS.iter().any(|(a, _)| *a == action),
                "action {action:?} is missing from the help overlay"
            );
        }
    }
}
