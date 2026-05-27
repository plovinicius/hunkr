//! Top-level frame composition. Lays out the tree | diff body and the status
//! bar, and records panel heights back into `App` so navigation can clamp
//! scrolling correctly.

mod diff_panel;
mod help;
mod statusbar;
mod tree_panel;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};

use crate::app::{App, Mode};

/// Width of the file-tree panel, in columns.
const TREE_WIDTH: u16 = 44;

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    let body = rows[0];
    let status = rows[1];

    let cols =
        Layout::horizontal([Constraint::Length(TREE_WIDTH), Constraint::Min(20)]).split(body);
    let tree_area = cols[0];
    let diff_area = cols[1];

    // Record the diff viewport height (inner area minus the 2 border rows) so
    // scroll clamping and PageUp/Down know how far a page is.
    app.diff_height = diff_area.height.saturating_sub(2) as usize;

    tree_panel::render(f, tree_area, app);
    diff_panel::render(f, diff_area, app);
    statusbar::render(f, status, app);

    if app.mode == Mode::Help {
        help::render(f, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::git::diff::DiffBase;
    use crate::model::file::{ChangeKind, ChangedFile};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

    /// Flatten a rendered TestBackend buffer into plain text (one line per row).
    fn buffer_text(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer();
        let area = *buf.area();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    out.push_str(cell.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn renders_tree_chrome_and_files() {
        let files = vec![
            ChangedFile::new(PathBuf::from("README.md"), ChangeKind::Added),
            ChangedFile::new(PathBuf::from("src/foo.rs"), ChangeKind::Modified),
        ];
        let mut app = App::with_files(PathBuf::from("/repo"), DiffBase::Head, files);

        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let text = buffer_text(&term);

        assert!(
            text.contains("Changed files (2)"),
            "tree title missing:\n{text}"
        );
        assert!(text.contains("README.md"), "file row missing:\n{text}");
        assert!(text.contains("src"), "folder row missing:\n{text}");
        // Default glyphs are ASCII-safe: unreviewed [ ] and an (A) tag.
        assert!(text.contains("[ ]"), "unreviewed glyph missing:\n{text}");
        assert!(text.contains("(A)"), "added kind tag missing:\n{text}");
        // No file selected → diff panel shows its placeholder.
        assert!(
            text.contains("Select a file"),
            "diff placeholder missing:\n{text}"
        );
        // Status bar chrome + review counts (2 unreviewed).
        assert!(text.contains("hunkr"), "status bar missing:\n{text}");
        assert!(
            text.contains("[x] 0 | [ ] 2 | [!] 0"),
            "review counts missing:\n{text}"
        );
        assert!(text.contains("q quit"), "key hints missing:\n{text}");
    }

    #[test]
    fn renders_diff_hunk_and_lines() {
        use crate::git::diff::parse_unified;
        use std::sync::Arc;

        let files = vec![ChangedFile::new(
            PathBuf::from("foo.rs"),
            ChangeKind::Modified,
        )];
        let mut app = App::with_files(PathBuf::from("/repo"), DiffBase::Head, files);

        let raw = concat!(
            "@@ -1,3 +1,3 @@ fn main()\n",
            " keep this line\n",
            "-remove me\n",
            "+add me\n",
        );
        app.set_diff_for_test(parse_unified(Arc::from(raw), PathBuf::from("foo.rs")));

        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let text = buffer_text(&term);

        assert!(
            text.contains("@@ -1,3 +1,3 @@"),
            "hunk header missing:\n{text}"
        );
        assert!(
            text.contains("keep this line"),
            "context line missing:\n{text}"
        );
        assert!(text.contains("remove me"), "deletion missing:\n{text}");
        assert!(text.contains("add me"), "addition missing:\n{text}");
        // Diff panel title shows the file path; status bar shows hunk position.
        assert!(text.contains("foo.rs"), "diff title missing:\n{text}");
        assert!(text.contains("hunk 1/1"), "hunk counter missing:\n{text}");
    }

    #[test]
    fn renders_side_by_side() {
        use crate::app::ViewMode;
        use crate::git::diff::parse_unified;
        use std::sync::Arc;

        let files = vec![ChangedFile::new(
            PathBuf::from("foo.rs"),
            ChangeKind::Modified,
        )];
        let mut app = App::with_files(PathBuf::from("/repo"), DiffBase::Head, files);
        let raw = concat!("@@ -1,2 +1,2 @@\n", " keep\n", "-old line\n", "+new line\n",);
        app.set_diff_for_test(parse_unified(Arc::from(raw), PathBuf::from("foo.rs")));
        app.view = ViewMode::SideBySide;

        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let text = buffer_text(&term);

        assert!(
            text.contains("[side-by-side]"),
            "mode label missing:\n{text}"
        );
        assert!(text.contains("keep"), "context missing:\n{text}");
        // The deleted and added lines sit on the same row, one per side.
        let same_row = text
            .lines()
            .any(|l| l.contains("old line") && l.contains("new line"));
        assert!(same_row, "old/new not rendered side by side:\n{text}");
    }

    #[test]
    fn help_overlay_lists_keybindings() {
        let files = vec![ChangedFile::new(
            PathBuf::from("a.rs"),
            ChangeKind::Modified,
        )];
        let mut app = App::with_files(PathBuf::from("/repo"), DiffBase::Head, files);
        app.mode = Mode::Help;

        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let text = buffer_text(&term);

        assert!(text.contains("Help"), "help title missing:\n{text}");
        assert!(
            text.contains("copy AI reference"),
            "keybinding missing:\n{text}"
        );
        assert!(
            text.contains("press any key to close"),
            "help footer missing:\n{text}"
        );
    }
}
