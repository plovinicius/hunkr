//! Bottom status bar: context on the left, key hints on the right.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::model::review::ReviewStatus;

const HINTS: &str = " j/k move · n/p hunk · r/u review · ]/[ file · q quit ";

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let bar = Style::default().bg(Color::Indexed(236)).fg(Color::Gray);

    let left = if let Some(err) = &app.error {
        format!(" ⚠ {err}")
    } else if let Some(msg) = &app.status_msg {
        format!(" {msg}")
    } else {
        let (mut reviewed, mut unreviewed, mut changed) = (0u32, 0u32, 0u32);
        for i in 0..app.files.len() {
            match app.review_status(i) {
                ReviewStatus::Reviewed => reviewed += 1,
                ReviewStatus::Unreviewed => unreviewed += 1,
                ReviewStatus::ChangedAfterReview => changed += 1,
            }
        }
        let hunk = match &app.diff {
            Some(fd) if !fd.hunks.is_empty() => {
                format!("hunk {}/{}", app.current_hunk + 1, fd.hunks.len())
            }
            _ => "—".to_string(),
        };
        format!(" hunkr · ✓{reviewed} ●{unreviewed} ↻{changed} · {hunk}")
    };

    // Background first, then the two text layers (left text is drawn first so
    // the right-aligned hints overwrite only their own cells).
    f.render_widget(Paragraph::new("").style(bar), area);
    f.render_widget(Paragraph::new(left).style(bar), area);
    f.render_widget(
        Paragraph::new(HINTS).style(bar).alignment(Alignment::Right),
        area,
    );
}
