//! UI glyph set. ASCII by default so icons render in any terminal/font/tmux;
//! `--unicode` opts into the prettier symbols. This is the single source of
//! truth for every non-text marker the UI draws.

use crate::model::review::ReviewStatus;

#[derive(Debug, Clone, Copy)]
pub struct Glyphs {
    pub reviewed: &'static str,
    pub unreviewed: &'static str,
    pub changed: &'static str,
    pub folder_open: char,
    pub folder_closed: char,
    /// Prefix on error messages in the status bar.
    pub error: &'static str,
    /// Block cursor shown while editing the filter.
    pub cursor: char,
    /// Separator between status-bar segments.
    pub sep: &'static str,
    /// Placeholder when there's no current hunk.
    pub dash: &'static str,
}

impl Glyphs {
    pub fn ascii() -> Self {
        Glyphs {
            reviewed: "[x]",
            unreviewed: "[ ]",
            changed: "[!]",
            folder_open: 'v',
            folder_closed: '>',
            error: "!",
            cursor: '_',
            sep: " | ",
            dash: "-",
        }
    }

    pub fn unicode() -> Self {
        Glyphs {
            // Note: every glyph here must be single-width in common fonts, or it
            // will swallow the following space and misalign the row. `↻`/`⚠` are
            // frequently rendered double-width, so we avoid them.
            reviewed: "✓",
            unreviewed: "●",
            changed: "!",
            folder_open: '▾',
            folder_closed: '▸',
            error: "!",
            cursor: '│',
            sep: " · ",
            dash: "—",
        }
    }

    /// The marker for a review status.
    pub fn status(&self, status: ReviewStatus) -> &'static str {
        match status {
            ReviewStatus::Reviewed => self.reviewed,
            ReviewStatus::Unreviewed => self.unreviewed,
            ReviewStatus::ChangedAfterReview => self.changed,
        }
    }
}
