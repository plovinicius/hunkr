//! Top-level frame composition. Lays out the tree | diff body and the status
//! bar, and records panel heights back into `App` so navigation can clamp
//! scrolling correctly.

mod diff_panel;
mod help;
mod notification;
mod statusbar;
mod theme_picker;
mod tree_panel;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};

use crate::app::{App, MIN_DIFF_WIDTH, MIN_TREE_WIDTH, Mode};
use crate::config::Action;

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    let body = rows[0];
    let status = rows[1];

    // Record the body width so resize handlers can clamp the sidebar against
    // the current terminal size, then derive the actually-rendered tree width.
    app.body_width = body.width;
    let max_tree = body
        .width
        .saturating_sub(MIN_DIFF_WIDTH)
        .max(MIN_TREE_WIDTH);
    let tree_width = app.tree_width.clamp(MIN_TREE_WIDTH, max_tree);

    let cols = Layout::horizontal([
        Constraint::Length(tree_width),
        Constraint::Min(MIN_DIFF_WIDTH),
    ])
    .split(body);
    let tree_area = cols[0];
    let diff_area = cols[1];

    // Record the diff viewport height (inner area minus the 2 border rows) so
    // scroll clamping and PageUp/Down know how far a page is, and its left edge
    // so mouse-wheel events can be routed to the panel under the cursor.
    app.diff_height = diff_area.height.saturating_sub(2) as usize;
    app.diff_x = diff_area.x;

    tree_panel::render(f, tree_area, app);
    diff_panel::render(f, diff_area, app);
    statusbar::render(f, status, app);

    if app.mode == Mode::Help {
        help::render(f, area, app);
    }
    if app.mode == Mode::ThemePicker {
        theme_picker::render(f, area, app);
    }

    // Top-right toasts, rendered last so they sit above everything and stack
    // downward: the persistent config error (until fixed), then the transient
    // toast (e.g. "copied for AI", which auto-dismisses).
    let mut next_y = area.y + 1;
    if let Some(err) = &app.config_error {
        let edit_key = app
            .config
            .keys
            .primary(Action::EditConfig)
            .unwrap_or_else(|| "C".to_string());
        next_y = notification::render_config_error(f, area, next_y, err, &edit_key);
    }
    if let Some(toast) = &app.toast {
        notification::render_toast(f, area, next_y, &toast.text);
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
        // Default glyphs are Unicode: unreviewed ● and a (+) added kind tag.
        assert!(text.contains("●"), "unreviewed glyph missing:\n{text}");
        assert!(text.contains("(+)"), "added kind tag missing:\n{text}");
        // No file selected → diff panel shows its placeholder.
        assert!(
            text.contains("Select a file"),
            "diff placeholder missing:\n{text}"
        );
        // Status bar chrome + review counts (2 unreviewed).
        assert!(text.contains("hunkr"), "status bar missing:\n{text}");
        assert!(text.contains("✓ 0 · ● 2"), "review counts missing:\n{text}");
        assert!(text.contains("q quit"), "key hints missing:\n{text}");
    }

    #[test]
    fn renders_diff_chunk_and_lines() {
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
            "chunk header missing:\n{text}"
        );
        assert!(
            text.contains("keep this line"),
            "context line missing:\n{text}"
        );
        assert!(text.contains("remove me"), "deletion missing:\n{text}");
        assert!(text.contains("add me"), "addition missing:\n{text}");
        // Diff panel title shows the file path; status bar shows chunk position.
        assert!(text.contains("foo.rs"), "diff title missing:\n{text}");
        assert!(text.contains("chunk 1/1"), "chunk counter missing:\n{text}");
    }

    #[test]
    fn untrusted_path_and_content_cannot_inject_escapes() {
        use crate::git::diff::parse_unified;
        use std::sync::Arc;

        // A repo can name a file — and contain diff lines / chunk context — with
        // raw ANSI/OSC escapes. None of it may reach the rendered buffer.
        let evil_path = "src/\x1b]0;pwned\x07evil.rs";
        let files = vec![ChangedFile::new(
            PathBuf::from(evil_path),
            ChangeKind::Modified,
        )];
        let mut app = App::with_files(PathBuf::from("/repo"), DiffBase::Head, files);
        let raw = concat!(
            "@@ -1,2 +1,2 @@ fn \x1b[31mctx\x1b[0m()\n",
            " keep \x1b]0;title\x07\n",
            "-old\x1b[1m\n",
            "+new\x07\n",
        );
        app.set_diff_for_test(parse_unified(Arc::from(raw), PathBuf::from(evil_path)));

        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let text = buffer_text(&term);

        assert!(
            !text.contains('\x1b'),
            "ESC leaked into the buffer:\n{text:?}"
        );
        assert!(
            !text.contains('\x07'),
            "BEL leaked into the buffer:\n{text:?}"
        );
        // The inert replacement char is rendered in its place.
        assert!(text.contains('\u{FFFD}'), "expected sanitized placeholder");
    }

    #[test]
    fn current_chunk_header_is_marked() {
        use crate::git::diff::parse_unified;
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::sync::Arc;

        let files = vec![ChangedFile::new(
            PathBuf::from("foo.rs"),
            ChangeKind::Modified,
        )];
        let mut app = App::with_files(PathBuf::from("/repo"), DiffBase::Head, files);
        let raw = concat!(
            "@@ -1,1 +1,1 @@ first\n",
            "-a\n",
            "+b\n",
            "@@ -10,1 +10,1 @@ second\n",
            "-c\n",
            "+d\n",
        );
        app.set_diff_for_test(parse_unified(Arc::from(raw), PathBuf::from("foo.rs")));

        // Advance to the second chunk, as `n` does.
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));

        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let text = buffer_text(&term);

        let first = text.lines().find(|l| l.contains("first")).unwrap();
        let second = text.lines().find(|l| l.contains("second")).unwrap();
        // The current chunk's header carries the accent bar; the other doesn't.
        assert!(
            second.contains("▌ @@"),
            "current chunk header should be marked:\n{text}"
        );
        assert!(
            !first.contains("▌ @@"),
            "non-current chunk header should not be marked:\n{text}"
        );
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
    fn hiding_a_file_drops_it_from_the_sidebar_and_counts() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let files = vec![
            ChangedFile::new(PathBuf::from("keep.rs"), ChangeKind::Modified),
            ChangedFile::new(PathBuf::from("noise.lock"), ChangeKind::Modified),
        ];
        let mut app = App::with_files(PathBuf::from("/repo"), DiffBase::Head, files);
        // Anchor the hidden store at a writable temp dir so the hide persists.
        let dir = std::env::temp_dir().join(format!("hunkr-ui-hide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        app.hidden = crate::persist::HiddenStore::empty(&dir);

        let down = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        let hide = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        let toggle_view = KeyEvent::new(KeyCode::Char('H'), KeyModifiers::NONE);

        // Move to noise.lock (sorts after keep.rs) and hide it.
        app.on_key(down);
        app.on_key(hide);
        // The synthetic /repo path can't satisfy `git diff`; clear that
        // unrelated error so the status bar shows its normal counts segment.
        app.error = None;

        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let text = buffer_text(&term);

        assert!(
            text.contains("Changed files (1)"),
            "shown count should drop to 1:\n{text}"
        );
        assert!(text.contains("keep.rs"), "kept file should remain:\n{text}");
        assert!(
            !text.contains("noise.lock"),
            "hidden file must be gone from the sidebar:\n{text}"
        );
        // The hidden file is excluded from the review totals, and a hidden
        // segment reports the count.
        assert!(
            text.contains("● 1"),
            "review count should exclude the hidden file:\n{text}"
        );
        assert!(text.contains("hidden 1"), "hidden count missing:\n{text}");

        // Toggling the hidden view surfaces only the hidden file.
        app.on_key(toggle_view);
        app.error = None;
        term.draw(|f| render(f, &mut app)).unwrap();
        let text = buffer_text(&term);
        assert!(
            text.contains("Hidden files (1)"),
            "hidden-view title missing:\n{text}"
        );
        assert!(
            text.contains("noise.lock"),
            "hidden view should show the hidden file:\n{text}"
        );
        assert!(
            !text.contains("keep.rs"),
            "hidden view should not show shown files:\n{text}"
        );
    }

    #[test]
    fn syntax_highlighting_colours_code_and_tints_added_lines() {
        use crate::git::diff::parse_unified;
        use ratatui::style::Color;
        use std::sync::Arc;

        let files = vec![ChangedFile::new(
            PathBuf::from("foo.rs"),
            ChangeKind::Modified,
        )];
        let mut app = App::with_files(PathBuf::from("/repo"), DiffBase::Head, files);
        // Force truecolor so syntax colours are exact RGB, independent of $COLORTERM.
        app.truecolor = true;
        let raw = concat!(
            "@@ -1,2 +1,2 @@\n",
            " fn main() {\n",
            "-    let x = 1;\n",
            "+    let y = compute();\n",
        );
        app.set_diff_for_test(parse_unified(Arc::from(raw), PathBuf::from("foo.rs")));
        assert!(
            app.highlight.is_some(),
            "highlighting should be on by default"
        );

        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let area = *buf.area();

        // Find the screen row that renders the added line.
        let mut added_row = None;
        for y in 0..area.height {
            let mut line = String::new();
            for x in 0..area.width {
                if let Some(c) = buf.cell((x, y)) {
                    line.push_str(c.symbol());
                }
            }
            if line.contains("compute") {
                added_row = Some(y);
                break;
            }
        }
        let y = added_row.expect("added line should be rendered");

        // The added line's code cells carry the faint green tint background, and
        // the code shows more than one foreground colour (syntax tokens).
        let mut fg_colours = std::collections::HashSet::new();
        let mut saw_tint = false;
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                let s = cell.symbol();
                // The add tint is theme-derived but always green-dominant: a
                // truecolor RGB whose green channel leads (see highlight::tint).
                if let Color::Rgb(r, g, b) = cell.bg
                    && g > r
                    && g > b
                {
                    saw_tint = true;
                }
                if s.trim() != "" && s != "\u{FFFD}" {
                    fg_colours.insert(cell.fg);
                }
            }
        }
        assert!(
            saw_tint,
            "added line should carry the green tint background"
        );
        assert!(
            fg_colours.len() > 2,
            "expected several syntax foreground colours, saw {fg_colours:?}"
        );
    }

    #[test]
    fn theme_picker_overlay_renders() {
        let files = vec![ChangedFile::new(
            PathBuf::from("a.rs"),
            ChangeKind::Modified,
        )];
        let mut app = App::with_files(PathBuf::from("/repo"), DiffBase::Head, files);
        app.on_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char('T'),
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));

        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let text = buffer_text(&term);

        assert!(
            text.contains("Select theme"),
            "picker title missing:\n{text}"
        );
        assert!(text.contains("search:"), "search input missing:\n{text}");
        assert!(text.contains("apply"), "footer hint missing:\n{text}");
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
