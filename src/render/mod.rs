pub mod viewport;

/// Replace control characters with the Unicode replacement char (`U+FFFD`).
///
/// Repo-derived strings — file paths, diff text, chunk-header context — are
/// untrusted: a file can be named, or contain a line, with embedded ANSI/OSC
/// escape sequences (git allows any byte but NUL and `/` in a path). Some
/// render paths write a cell's symbol to the terminal verbatim, so a raw `ESC`
/// would be interpreted as an escape sequence (screen/title spoofing, clipboard
/// writes, …). Rendering every control byte inert closes that off at the source
/// rather than relying on a particular widget to drop zero-width graphemes.
///
/// `U+FFFD` is single-width, so callers that track display columns can treat
/// each replaced char as width 1. Tabs are intentionally preserved for callers
/// (e.g. the diff body) that expand them to spaces themselves.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '\t' || !c.is_control() {
                c
            } else {
                '\u{FFFD}'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_control_and_escape_bytes() {
        // A filename/line carrying an ANSI escape becomes inert; printable
        // text (including the leading ESC's `[` payload) survives, tabs pass.
        let evil = "src/\x1b[31mfoo\x07.rs\tok";
        let clean = sanitize(evil);
        assert!(!clean.contains('\x1b'), "ESC must be removed: {clean:?}");
        assert!(!clean.contains('\x07'), "BEL must be removed: {clean:?}");
        assert!(clean.contains("foo"));
        assert!(clean.contains('\t'), "tab is preserved for the caller");
        assert_eq!(clean, "src/\u{FFFD}[31mfoo\u{FFFD}.rs\tok");
    }

    #[test]
    fn leaves_plain_text_untouched() {
        assert_eq!(sanitize("src/main.rs"), "src/main.rs");
    }
}
