//! Bottom status bar: context on the left, key hints on the right.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Mode};
use crate::model::review::ReviewStatus;

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let bar = Style::default().bg(Color::Indexed(236)).fg(Color::Gray);
    let g = &app.glyphs;
    let sep = g.sep;

    let left = if app.mode == Mode::Filter {
        // Live filter editing, with a block cursor.
        format!(" filter: {}{}", app.filter, g.cursor)
    } else if let Some(err) = &app.error {
        format!(" {} {err}", g.error)
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
            _ => g.dash.to_string(),
        };
        let filter = if app.is_filtering() {
            format!("{sep}filter:{}", app.filter)
        } else {
            String::new()
        };
        format!(
            " hunkr{sep}{}{reviewed} {}{unreviewed} {}{changed}{sep}{hunk}{filter}",
            g.reviewed, g.unreviewed, g.changed,
        )
    };

    let hints = if app.mode == Mode::Filter {
        format!(" Enter apply{sep}Esc cancel ")
    } else {
        format!(
            " j/k move{sep}n/p hunk{sep}r review{sep}y copy{sep}/ filter{sep}? help{sep}q quit "
        )
    };

    // Background first, then the left text. The right-aligned hints are drawn
    // only when they fit alongside the left text, so they never clobber it on a
    // narrow terminal (left-side info takes priority).
    f.render_widget(Paragraph::new("").style(bar), area);
    let fits = (left.width() + hints.width()) <= area.width as usize;
    f.render_widget(Paragraph::new(left.as_str()).style(bar), area);
    if fits {
        f.render_widget(
            Paragraph::new(hints).style(bar).alignment(Alignment::Right),
            area,
        );
    }
}
