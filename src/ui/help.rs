//! Centered help overlay listing the keybindings, grouped into sections and
//! laid out in two columns so the (now sizeable) list stays readable and short
//! enough to fit a normal terminal.
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

/// A titled group of related actions.
struct Section {
    title: &'static str,
    rows: &'static [(Action, &'static str)],
}

/// Sections shown in the left column, top to bottom.
const LEFT: &[Section] = &[
    Section {
        title: "Navigate",
        rows: &[
            (Action::ScrollDown, "scroll / move down"),
            (Action::ScrollUp, "scroll / move up"),
            (Action::PageDown, "page down"),
            (Action::PageUp, "page up"),
            (Action::Top, "top of diff"),
            (Action::Bottom, "bottom of diff"),
        ],
    },
    Section {
        title: "Move",
        rows: &[
            (Action::NextChunk, "next chunk"),
            (Action::PrevChunk, "previous chunk"),
            (Action::NextFile, "next file"),
            (Action::PrevFile, "previous file"),
            (Action::SwitchFocus, "switch tree / diff"),
            (Action::Activate, "fold folder / focus diff"),
        ],
    },
    Section {
        title: "View",
        rows: &[
            (Action::ToggleView, "unified / side-by-side"),
            (Action::NarrowSidebar, "narrow sidebar"),
            (Action::WidenSidebar, "widen sidebar"),
        ],
    },
];

/// Sections shown in the right column, top to bottom.
const RIGHT: &[Section] = &[
    Section {
        title: "Review",
        rows: &[
            (Action::ToggleChunkReviewed, "reviewed: this chunk"),
            (Action::ToggleReviewed, "reviewed: whole file"),
            (Action::ExpandAllChunks, "reveal collapsed chunks"),
            (Action::CopyReference, "copy AI reference"),
        ],
    },
    Section {
        title: "Files",
        rows: &[
            (Action::ToggleHidden, "hide / un-hide file"),
            (Action::ToggleHiddenView, "hidden-files view"),
            (Action::StartFilter, "filter files"),
            (Action::OpenEditor, "open in $EDITOR"),
        ],
    },
    Section {
        title: "App",
        rows: &[
            (Action::EditConfig, "edit config"),
            (Action::Help, "toggle this help"),
            (Action::Quit, "quit"),
        ],
    },
];

/// Gap (in columns) between the two columns.
const GAP: u16 = 4;

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let keymap = &app.config.keys;
    let chords = |action: Action| {
        let cs = keymap.chords_for(action);
        if cs.is_empty() {
            "—".to_string()
        } else {
            cs.join(" / ")
        }
    };

    // Each column aligns its descriptions to its *own* widest key, so a long
    // chord in one column doesn't leave the other sparsely spaced.
    let key_w = |sections: &[Section]| {
        sections
            .iter()
            .flat_map(|s| s.rows)
            .map(|(a, _)| chords(*a).chars().count())
            .max()
            .unwrap_or(0)
    };
    let left_key_w = key_w(LEFT);
    let right_key_w = key_w(RIGHT);

    let left = column_lines(LEFT, left_key_w, &chords);
    let right = column_lines(RIGHT, right_key_w, &chords);
    let left_w = column_width(LEFT, left_key_w) as u16;
    let right_w = column_width(RIGHT, right_key_w) as u16;
    let rows = left.len().max(right.len()) as u16;

    // Top blank + the tallest column + blank + footer, inside the two borders.
    let width = (left_w + GAP + right_w + 4).min(area.width);
    let height = (rows + 5).min(area.height);
    let rect = centered(area, width, height);

    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Help ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let left_area = Rect {
        x: inner.x + 1,
        y: inner.y + 1,
        width: left_w.min(inner.width.saturating_sub(1)),
        height: rows.min(inner.height),
    };
    f.render_widget(Paragraph::new(left), left_area);

    let right_x = inner.x + 1 + left_w + GAP;
    if right_x < inner.x + inner.width {
        let right_area = Rect {
            x: right_x,
            y: inner.y + 1,
            width: right_w.min(inner.x + inner.width - right_x),
            height: rows.min(inner.height),
        };
        f.render_widget(Paragraph::new(right), right_area);
    }

    // Footer one blank row below the columns.
    let footer_y = inner.y + 1 + rows + 1;
    if footer_y < inner.y + inner.height {
        let footer = Rect {
            x: inner.x + 1,
            y: footer_y,
            width: inner.width.saturating_sub(1),
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Span::styled(
                "press any key to close",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )),
            footer,
        );
    }
}

/// Build the rendered lines for one column: a styled title per section, its rows
/// (`key  description`), and a blank line between sections.
fn column_lines<'a>(
    sections: &[Section],
    key_w: usize,
    chords: &dyn Fn(Action) -> String,
) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    for (i, section) in sections.iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            section.title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ));
        for (action, desc) in section.rows {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{:<key_w$}", chords(*action)),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(format!("  {desc}")),
            ]));
        }
    }
    lines
}

/// The display width a column needs: the widest of its titles and its
/// `indent + key + gap + description` rows.
fn column_width(sections: &[Section], key_w: usize) -> usize {
    let mut w = 0;
    for section in sections {
        w = w.max(section.title.chars().count());
        for (_, desc) in section.rows {
            w = w.max(2 + key_w + 2 + desc.chars().count());
        }
    }
    w
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
        // added — every action must appear in exactly one section.
        for action in Action::ALL {
            let count = LEFT
                .iter()
                .chain(RIGHT)
                .flat_map(|s| s.rows)
                .filter(|(a, _)| *a == action)
                .count();
            assert_eq!(
                count, 1,
                "action {action:?} should appear exactly once in the help overlay, found {count}"
            );
        }
    }
}
