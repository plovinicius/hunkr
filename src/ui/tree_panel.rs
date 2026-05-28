//! Left panel: the changed-file tree with review-status indicators.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, Focus};
use crate::model::file::ChangeKind;
use crate::model::review::ReviewStatus;

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Tree;
    let title = format!(" Changed files ({}) ", app.files.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.tree.visible.is_empty() {
        f.render_widget(
            Paragraph::new("No changes").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let dim = Style::default().fg(Color::DarkGray);
    let height = inner.height as usize;
    // Keep the cursor row within the visible window (simple top-anchored scroll).
    let top = app.tree_cursor.saturating_sub(height.saturating_sub(1));

    // When filtering, the tree is a flat list of matching files, so drop the
    // hierarchical indent and show full paths for clarity.
    let filtering = app.is_filtering();
    let mut lines = Vec::with_capacity(height);
    for (i, &node_idx) in app.tree.visible.iter().enumerate().skip(top).take(height) {
        let node = &app.tree.nodes[node_idx];
        let indent = if filtering {
            String::new()
        } else {
            "  ".repeat(node.depth as usize)
        };

        let mut spans: Vec<Span> = vec![Span::raw(indent)];
        if let Some(fi) = node.file {
            let file = &app.files[fi];
            let status = app.review_status(fi);
            spans.push(Span::styled(
                format!("{} ", app.glyphs.status(status)),
                Style::default().fg(status_color(status)),
            ));
            let name = if filtering {
                file.path.to_string_lossy().into_owned()
            } else {
                node.name.clone()
            };
            spans.push(Span::raw(name));
            if let Some(suffix) = kind_suffix(&file.kind) {
                spans.push(Span::styled(suffix, dim));
            }
            if file.additions > 0 || file.deletions > 0 {
                spans.push(Span::styled(
                    format!("  +{} -{}", file.additions, file.deletions),
                    dim,
                ));
            }
        } else {
            let arrow = if node.collapsed {
                app.glyphs.folder_closed
            } else {
                app.glyphs.folder_open
            };
            spans.push(Span::styled(
                format!("{arrow} {}/", node.name),
                Style::default().fg(Color::Blue),
            ));
        }

        let mut line = Line::from(spans);
        if i == app.tree_cursor {
            // Background highlight (not REVERSED) so status colors stay readable.
            let bg = if focused {
                Color::Indexed(240)
            } else {
                Color::Indexed(237)
            };
            line = line.style(Style::default().bg(bg).add_modifier(Modifier::BOLD));
        }
        lines.push(line);
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn status_color(status: ReviewStatus) -> Color {
    match status {
        ReviewStatus::Reviewed => Color::Green,
        ReviewStatus::Unreviewed => Color::DarkGray,
    }
}

/// A short kind tag for anything other than a plain modification.
fn kind_suffix(kind: &ChangeKind) -> Option<String> {
    match kind {
        ChangeKind::Modified => None,
        other => Some(format!(" ({})", other.glyph())),
    }
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}
