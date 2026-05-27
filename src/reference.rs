//! AI reference generation + clipboard copy.
//!
//! Builds a compact `path:line` reference for the hunk under the cursor and
//! copies it to the clipboard, so it can be pasted straight back to a coding
//! agent that resolves it against the working tree. We deliberately omit the
//! diff snippet: an agent with repo access reads the live file at those lines,
//! which is cheaper than shipping the hunk and immune to a stale snippet. Copy
//! prefers the system clipboard (`arboard`, reliable locally) and falls back to
//! an OSC 52 terminal escape so it still works over tmux/SSH.

use std::io::Write;

use anyhow::{Context, Result};

use crate::model::diff::{FileDiff, Hunk};

/// Build the AI-ready reference for hunk `hunk_index` of `fd`: a compact
/// `path:start-end` pointer (collapsed to `path:line` when the hunk touches a
/// single line) that an agent resolves against the working tree.
pub fn build_hunk_reference(fd: &FileDiff, hunk_index: usize) -> String {
    let (start, end) = new_line_range(&fd.hunks[hunk_index]);
    let path = fd.path.display();
    if start == end {
        format!("{path}:{start}")
    } else {
        format!("{path}:{start}-{end}")
    }
}

/// The new-file line range the hunk touches (falls back to old-file numbers for
/// a pure-deletion hunk).
fn new_line_range(hunk: &Hunk) -> (u32, u32) {
    let pick = |f: fn(&crate::model::diff::DiffLine) -> Option<u32>| {
        let nums: Vec<u32> = hunk.lines.iter().filter_map(f).collect();
        match (nums.iter().min(), nums.iter().max()) {
            (Some(&lo), Some(&hi)) => Some((lo, hi)),
            _ => None,
        }
    };
    pick(|l| l.new_no)
        .or_else(|| pick(|l| l.old_no))
        .unwrap_or((0, 0))
}

/// Where a copy ended up, for the status message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyMethod {
    Clipboard,
    Osc52,
}

impl std::fmt::Display for CopyMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CopyMethod::Clipboard => f.write_str("clipboard"),
            CopyMethod::Osc52 => f.write_str("terminal (OSC52)"),
        }
    }
}

/// Copy `text` to the clipboard. Tries the system clipboard first, then falls
/// back to OSC 52 (works over tmux/SSH where there's no local display).
pub fn copy(text: &str) -> Result<CopyMethod> {
    if let Ok(mut cb) = arboard::Clipboard::new()
        && cb.set_text(text.to_owned()).is_ok()
    {
        return Ok(CopyMethod::Clipboard);
    }
    osc52_copy(text).context("OSC52 clipboard write failed")?;
    Ok(CopyMethod::Osc52)
}

fn osc52_copy(text: &str) -> std::io::Result<()> {
    // OSC 52: ESC ] 52 ; c ; <base64> BEL — interpreted by the terminal, not
    // drawn, so it's safe to emit while the TUI owns the screen.
    let seq = format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
    let mut out = std::io::stdout().lock();
    out.write_all(seq.as_bytes())?;
    out.flush()
}

/// Minimal standard-alphabet base64 (avoids a dependency for one small need).
fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn diff(raw: &str) -> FileDiff {
        crate::git::diff::parse_unified(Arc::from(raw), std::path::PathBuf::from("src/foo.rs"))
    }

    #[test]
    fn reference_is_path_and_line_range() {
        let raw = concat!(
            "@@ -10,3 +10,4 @@ fn main()\n",
            " keep\n",
            "-old\n",
            "+new\n",
            "+extra\n",
        );
        // New-file line numbers present are 10 (keep), 11 (new), 12 (extra).
        assert_eq!(build_hunk_reference(&diff(raw), 0), "src/foo.rs:10-12");
    }

    #[test]
    fn single_line_reference_omits_the_range() {
        let raw = concat!("@@ -5,1 +5,1 @@\n", "-before\n", "+after\n");
        assert_eq!(build_hunk_reference(&diff(raw), 0), "src/foo.rs:5");
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }
}
