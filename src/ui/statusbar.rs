//! Bottom status bar: context on the left, key hints on the right.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Mode};
use crate::model::review::ReviewStatus;
use crate::render::sanitize;

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let bar = Style::default().bg(Color::Indexed(236)).fg(Color::Gray);
    let g = &app.glyphs;
    let sep = g.sep;

    let left = if app.mode == Mode::Filter {
        // Live filter editing, with a block cursor.
        format!(" filter: {}{}", app.filter, g.cursor)
    } else if let Some(err) = &app.error {
        // Errors can echo untrusted paths from git; strip control bytes.
        format!(" {} {}", g.error, sanitize(err))
    } else if let Some(msg) = &app.status_msg {
        format!(" {}", sanitize(msg))
    } else {
        let (mut reviewed, mut unreviewed) = (0u32, 0u32);
        for i in 0..app.files.len() {
            match app.review_status(i) {
                ReviewStatus::Reviewed => reviewed += 1,
                ReviewStatus::Unreviewed => unreviewed += 1,
            }
        }
        let chunk = match &app.diff {
            Some(fd) if !fd.chunks.is_empty() => {
                format!("chunk {}/{}", app.current_chunk + 1, fd.chunks.len())
            }
            _ => g.dash.to_string(),
        };
        let filter = if app.is_filtering() {
            format!("{sep}filter:{}", app.filter)
        } else {
            String::new()
        };
        // Each count is its own separator-delimited segment so the groups read
        // as evenly spaced regardless of glyph width.
        format!(
            " hunkr{sep}{} {reviewed}{sep}{} {unreviewed}{sep}{chunk}{filter}",
            g.reviewed, g.unreviewed,
        )
    };

    let hints = if app.mode == Mode::Filter {
        format!(" Enter apply{sep}Esc cancel ")
    } else {
        format!(" n/p chunk{sep}s split{sep}r review{sep}y copy{sep}? help{sep}q quit ")
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
