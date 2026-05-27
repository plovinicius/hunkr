//! Left panel: the changed-file tree.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, Focus};

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

    let height = inner.height as usize;
    // Keep the cursor row within the visible window (simple top-anchored scroll).
    let top = app.tree_cursor.saturating_sub(height.saturating_sub(1));

    let mut lines = Vec::with_capacity(height);
    for (i, &node_idx) in app.tree.visible.iter().enumerate().skip(top).take(height) {
        let node = &app.tree.nodes[node_idx];
        let indent = "  ".repeat(node.depth as usize);

        let label = if let Some(fi) = node.file {
            let file = &app.files[fi];
            let counts = if file.additions > 0 || file.deletions > 0 {
                format!("  +{} -{}", file.additions, file.deletions)
            } else {
                String::new()
            };
            format!("{indent}{} {}{counts}", file.kind.glyph(), node.name)
        } else {
            let arrow = if node.collapsed { '▸' } else { '▾' };
            format!("{indent}{arrow} {}/", node.name)
        };

        let mut style = Style::default();
        if node.file.is_none() {
            style = style.fg(Color::Blue);
        }
        if i == app.tree_cursor {
            style = if focused {
                style.add_modifier(Modifier::REVERSED)
            } else {
                style.bg(Color::Indexed(238))
            };
        }
        lines.push(Line::styled(label, style));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}
