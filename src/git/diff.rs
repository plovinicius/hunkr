//! Lazily fetch and parse a single file's diff.
//!
//! Diffs are fetched per file on selection (never eagerly for the whole repo).
//! The parser is a small line-by-line state machine over `git`'s unified-diff
//! output; cost is O(diff size), which is bounded by *what changed*, so even a
//! huge file with a small edit parses instantly.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;

use crate::git::command;
use crate::model::diff::{DiffLine, FileDiff, Chunk, LineKind};
use crate::model::file::ChangedFile;

/// The left-hand side of the diff. Normally `HEAD`; when the repo has no
/// commits yet we diff against git's well-known empty-tree object so that
/// staged/tracked files still show as additions.
#[derive(Debug, Clone, Copy)]
pub enum DiffBase {
    Head,
    EmptyTree,
}

/// The empty tree object id — stable across all git repositories.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

impl DiffBase {
    pub fn as_arg(&self) -> &'static str {
        match self {
            DiffBase::Head => "HEAD",
            DiffBase::EmptyTree => EMPTY_TREE,
        }
    }
}

/// Detect whether the repo has a resolvable `HEAD`.
pub fn detect_base(repo_root: &Path) -> DiffBase {
    if command::succeeds(repo_root, &["rev-parse", "--verify", "--quiet", "HEAD"]) {
        DiffBase::Head
    } else {
        DiffBase::EmptyTree
    }
}

/// Fetch the raw unified-diff text for one file.
fn fetch_diff_text(repo_root: &Path, base: DiffBase, file: &ChangedFile) -> Result<String> {
    let path = file.path.to_string_lossy();
    let out = if file.kind.is_untracked() {
        // No blob in the base → synthesize an all-addition diff. Exit code 1 is
        // expected here and tolerated by `capture_diff`.
        command::capture_diff(
            repo_root,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-index",
                "--",
                "/dev/null",
                path.as_ref(),
            ],
        )?
    } else {
        command::capture_diff(
            repo_root,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "-M",
                base.as_arg(),
                "--",
                path.as_ref(),
            ],
        )?
    };
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Fetch and parse the diff for one file.
pub fn fetch_file_diff(repo_root: &Path, base: DiffBase, file: &ChangedFile) -> Result<FileDiff> {
    let text: Arc<str> = Arc::from(fetch_diff_text(repo_root, base, file)?);
    Ok(parse_unified(text, file.path.clone()))
}

/// Stable, deterministic hash of a diff's text. seahash (not the default,
/// per-process-seeded hasher) so a hash persisted this session still matches
/// next session.
pub fn hash_text(text: &str) -> u64 {
    seahash::hash(text.as_bytes())
}

/// Compute current diff hashes for the given files. Used off the UI thread to
/// detect when a previously-reviewed file has changed. Files whose diff can't
/// be fetched are omitted.
pub fn diff_hashes_for<'a>(
    repo_root: &Path,
    base: DiffBase,
    files: impl IntoIterator<Item = &'a ChangedFile>,
) -> HashMap<PathBuf, u64> {
    let mut map = HashMap::new();
    for f in files {
        if let Ok(text) = fetch_diff_text(repo_root, base, f) {
            map.insert(f.path.clone(), hash_text(&text));
        }
    }
    map
}

/// Parse unified-diff `text` into a [`FileDiff`]. Header preamble lines
/// (`diff --git`, `index`, `---`, `+++`, mode lines) before the first `@@` are
/// skipped; only chunk bodies are retained.
pub fn parse_unified(text: Arc<str>, path: PathBuf) -> FileDiff {
    let s: &str = &text;
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut cur: Option<Chunk> = None;
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    let mut is_binary = false;
    let mut offset = 0usize;

    for line in s.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let had_nl = line.ends_with('\n');
        let end = offset - had_nl as usize; // content end, excluding '\n'
        let content = &s[start..end];

        if content.starts_with("@@") {
            if let Some(h) = cur.take() {
                chunks.push(h);
            }
            let (os, ns) = parse_chunk_header(content);
            old_no = os;
            new_no = ns;
            cur = Some(Chunk {
                header: start..end,
                lines: Vec::new(),
            });
        } else if content.starts_with("Binary files") || content.starts_with("GIT binary patch") {
            is_binary = true;
        } else if let Some(h) = cur.as_mut() {
            match content.as_bytes().first().copied() {
                Some(b'+') => {
                    h.lines.push(DiffLine {
                        kind: LineKind::Add,
                        old_no: None,
                        new_no: Some(new_no),
                        text: (start + 1)..end,
                    });
                    new_no += 1;
                }
                Some(b'-') => {
                    h.lines.push(DiffLine {
                        kind: LineKind::Del,
                        old_no: Some(old_no),
                        new_no: None,
                        text: (start + 1)..end,
                    });
                    old_no += 1;
                }
                Some(b' ') => {
                    h.lines.push(DiffLine {
                        kind: LineKind::Context,
                        old_no: Some(old_no),
                        new_no: Some(new_no),
                        text: (start + 1)..end,
                    });
                    old_no += 1;
                    new_no += 1;
                }
                Some(b'\\') => {
                    // "\ No newline at end of file" — keep the whole line.
                    h.lines.push(DiffLine {
                        kind: LineKind::NoNewline,
                        old_no: None,
                        new_no: None,
                        text: start..end,
                    });
                }
                _ => {} // blank/unexpected line inside a chunk
            }
        }
        // else: preamble before the first chunk → skip
    }
    if let Some(h) = cur.take() {
        chunks.push(h);
    }

    FileDiff {
        path,
        text,
        chunks,
        is_binary,
    }
}

/// Parse the `-old_start[,n]` / `+new_start[,n]` fields of a chunk header,
/// returning the starting line numbers. Only looks inside the `@@ ... @@`
/// delimiters so a trailing function-context string can't confuse it.
fn parse_chunk_header(h: &str) -> (u32, u32) {
    let after = h.strip_prefix("@@").unwrap_or(h);
    let core = match after.find("@@") {
        Some(i) => &after[..i],
        None => after,
    };
    let mut old_start = 0;
    let mut new_start = 0;
    for tok in core.split_whitespace() {
        if let Some(rest) = tok.strip_prefix('-') {
            old_start = rest
                .split(',')
                .next()
                .and_then(|x| x.parse().ok())
                .unwrap_or(0);
        } else if let Some(rest) = tok.strip_prefix('+') {
            new_start = rest
                .split(',')
                .next()
                .and_then(|x| x.parse().ok())
                .unwrap_or(0);
        }
    }
    (old_start, new_start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> FileDiff {
        parse_unified(Arc::from(s), PathBuf::from("x"))
    }

    #[test]
    fn parses_chunks_and_line_numbers() {
        // `concat!` (not `\`-continuation) so the leading space on context
        // lines is preserved exactly as git emits it.
        let raw = concat!(
            "diff --git a/x b/x\n",
            "index 111..222 100644\n",
            "--- a/x\n",
            "+++ b/x\n",
            "@@ -1,3 +1,4 @@\n",
            " ctx one\n",
            "-old two\n",
            "+new two\n",
            "+added three\n",
            " ctx four\n",
        );
        let d = parse(raw);
        assert_eq!(d.chunks.len(), 1);
        let h = &d.chunks[0];
        assert_eq!(h.lines.len(), 5);
        // First context line keeps both numbers starting at 1.
        assert_eq!(h.lines[0].old_no, Some(1));
        assert_eq!(h.lines[0].new_no, Some(1));
        // The deletion advances old, the additions advance new.
        assert_eq!(d.deletions(), 1);
        assert_eq!(d.additions(), 2);
        // Content excludes the leading marker.
        assert_eq!(d.slice(&h.lines[1].text), "old two");
        assert_eq!(d.slice(&h.lines[2].text), "new two");
    }

    #[test]
    fn detects_binary() {
        let raw = "diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n";
        let d = parse(raw);
        assert!(d.is_binary);
        assert!(d.chunks.is_empty());
    }

    #[test]
    fn handles_multiple_chunks() {
        let raw = "@@ -1,1 +1,1 @@\n-a\n+b\n@@ -10,1 +10,1 @@\n-c\n+d\n";
        let d = parse(raw);
        assert_eq!(d.chunks.len(), 2);
        assert_eq!(d.chunks[1].lines[0].old_no, Some(10));
    }
}
