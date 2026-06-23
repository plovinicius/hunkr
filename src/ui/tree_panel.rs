//! Left panel: the changed-file tree with review-status indicators.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, Focus};
use crate::model::file::ChangeKind;
use crate::model::review::ReviewStatus;
use crate::render::sanitize;

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Tree;
    // The hidden view inverts the list: it shows only hidden files (so they can
    // be un-hidden), so its title and count track the hidden set.
    let title = if app.hidden_view {
        format!(" Hidden files ({}) ", app.hidden_count())
    } else {
        format!(" Changed files ({}) ", app.shown_count())
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.tree.visible.is_empty() {
        let empty = if app.hidden_view {
            "No hidden files"
        } else {
            "No changes"
        };
        f.render_widget(
            Paragraph::new(empty).style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

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
            let (reviewed, total) = app.reviewed_chunk_count(fi);
            // ✓ when fully reviewed, a half-circle when only some chunks are,
            // otherwise the unreviewed dot.
            let (glyph, glyph_color) = if status == ReviewStatus::Reviewed {
                (app.glyphs.reviewed, Color::Green)
            } else if reviewed > 0 {
                (app.glyphs.partial, Color::Yellow)
            } else {
                (app.glyphs.unreviewed, Color::DarkGray)
            };
            spans.push(Span::styled(
                format!("{glyph} "),
                Style::default().fg(glyph_color),
            ));
            let name = if filtering {
                file.path.to_string_lossy().into_owned()
            } else {
                node.name.clone()
            };
            // Untrusted: paths/components can carry escape sequences.
            spans.push(Span::raw(sanitize(&name)));
            if let Some((suffix, style)) = kind_suffix(&file.kind) {
                spans.push(Span::styled(suffix, style));
            }
            // Per-chunk progress while a file is partway through review.
            if total > 0 && reviewed > 0 && reviewed < total {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    format!("{reviewed}/{total}"),
                    Style::default().fg(Color::Yellow),
                ));
            }
            if file.additions > 0 || file.deletions > 0 {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    format!("+{}", file.additions),
                    Style::default().fg(Color::Green),
                ));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format!("-{}", file.deletions),
                    Style::default().fg(Color::Red),
                ));
            }
        } else {
            let arrow = if node.collapsed {
                app.glyphs.folder_closed
            } else {
                app.glyphs.folder_open
            };
            spans.push(Span::styled(
                format!("{arrow} {}/", sanitize(&node.name)),
                Style::default().fg(Color::Blue),
            ));
        }

        if i == app.tree_cursor {
            // Pad to inner width so the row highlight extends past the text.
            let used: u16 = spans.iter().map(|s| s.width() as u16).sum();
            if used < inner.width {
                spans.push(Span::raw(" ".repeat((inner.width - used) as usize)));
            }
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

/// A short kind tag for anything other than a plain modification.
fn kind_suffix(kind: &ChangeKind) -> Option<(String, Style)> {
    let dim = Style::default().fg(Color::DarkGray);
    match kind {
        ChangeKind::Modified => None,
        ChangeKind::Added | ChangeKind::Untracked => {
            Some((" (+)".to_string(), Style::default().fg(Color::Green)))
        }
        ChangeKind::Deleted => Some((" (-)".to_string(), Style::default().fg(Color::Red))),
        other => Some((format!(" ({})", other.glyph()), dim)),
    }
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}
