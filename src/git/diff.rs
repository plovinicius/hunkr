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
use crate::model::diff::{Chunk, DiffLine, FileDiff, LineKind};
use crate::model::file::{ChangeKind, ChangedFile};
use crate::model::review::FileHashes;

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

/// Stable, deterministic content hash of a single chunk: its `@@` header plus
/// every line. Used to key per-chunk reviewed state, so an unchanged chunk stays
/// reviewed across refreshes and only an edited chunk reverts. seahash with fixed
/// seeds (like [`hash_text`]) so the value persists across sessions.
pub fn hash_chunk(fd: &FileDiff, chunk: &Chunk) -> u64 {
    use std::hash::Hasher;
    let mut h = seahash::SeaHasher::new();
    h.write(fd.slice(&chunk.header).as_bytes());
    for line in &chunk.lines {
        h.write(b"\n");
        h.write(fd.slice(&line.text).as_bytes());
    }
    h.finish()
}

/// The whole-diff hash plus a content hash per chunk for an already-parsed diff.
pub fn file_hashes(fd: &FileDiff) -> FileHashes {
    FileHashes {
        whole: hash_text(&fd.text),
        chunks: fd.chunks.iter().map(|c| hash_chunk(fd, c)).collect(),
    }
}

/// Compute current diff hashes for the given files. Used off the UI thread to
/// detect when a previously-reviewed file (or one of its chunks) has changed.
/// Files whose diff can't be fetched are omitted.
pub fn diff_hashes_for<'a>(
    repo_root: &Path,
    base: DiffBase,
    files: impl IntoIterator<Item = &'a ChangedFile>,
) -> HashMap<PathBuf, FileHashes> {
    let mut map = HashMap::new();
    for f in files {
        if let Ok(text) = fetch_diff_text(repo_root, base, f) {
            let fd = parse_unified(Arc::from(text), f.path.clone());
            map.insert(f.path.clone(), file_hashes(&fd));
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

/// Split an aggregate unified diff (many files, as produced by `git diff` /
/// `git show` / `git log -p`) into per-file `(ChangedFile, FileDiff)` pairs.
///
/// Each file segment starts at a `diff --git ` line and runs to the next one (or
/// EOF); anything before the first segment (commit message, `git show` header) is
/// ignored. The existing [`parse_unified`] is reused verbatim per segment — it
/// already skips the per-file preamble. Combined diffs (`diff --cc` /
/// `diff --combined`, with `@@@` markers from merges) are **skipped**, since the
/// unified parser can't read their multi-column markers.
pub fn split_unified(text: &str) -> Vec<(ChangedFile, Arc<FileDiff>)> {
    // Record the byte offset of every file-header line, flagging combined diffs.
    let mut headers: Vec<(usize, bool)> = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            headers.push((offset, false));
        } else if line.starts_with("diff --cc ") || line.starts_with("diff --combined ") {
            headers.push((offset, true));
        }
        offset += line.len();
    }

    let mut out = Vec::with_capacity(headers.len());
    for (i, &(start, combined)) in headers.iter().enumerate() {
        if combined {
            continue;
        }
        let end = headers.get(i + 1).map(|&(o, _)| o).unwrap_or(text.len());
        let segment = &text[start..end];
        let (path, kind) = classify_segment(segment);
        let fd = Arc::new(parse_unified(Arc::from(segment), path.clone()));
        let mut cf = ChangedFile::new(path, kind);
        cf.additions = fd.additions();
        cf.deletions = fd.deletions();
        out.push((cf, fd));
    }
    out
}

/// Derive a file's path and [`ChangeKind`] from its diff segment by scanning the
/// header lines (everything before the first `@@`). Path sources are tried most-
/// reliable first — single-path lines (`rename to`, `+++`, `---`) before the
/// space-ambiguous `diff --git` line.
fn classify_segment(segment: &str) -> (PathBuf, ChangeKind) {
    let mut new_file = false;
    let mut deleted_file = false;
    let mut rename_from: Option<String> = None;
    let mut rename_to: Option<String> = None;
    let mut plus_path: Option<String> = None;
    let mut minus_path: Option<String> = None;
    let mut git_line: Option<&str> = None;

    for line in segment.lines() {
        if line.starts_with("@@") {
            break;
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            git_line = Some(rest);
        } else if line.starts_with("new file mode ") {
            new_file = true;
        } else if line.starts_with("deleted file mode ") {
            deleted_file = true;
        } else if let Some(p) = line.strip_prefix("rename from ") {
            rename_from = Some(unquote_path(p));
        } else if let Some(p) = line.strip_prefix("rename to ") {
            rename_to = Some(unquote_path(p));
        } else if let Some(p) = line.strip_prefix("copy from ") {
            // A copy keeps the source; treat it like a rename for display.
            rename_from = Some(unquote_path(p));
        } else if let Some(p) = line.strip_prefix("copy to ") {
            rename_to = Some(unquote_path(p));
        } else if let Some(p) = line.strip_prefix("--- ") {
            minus_path = side_path(p);
        } else if let Some(p) = line.strip_prefix("+++ ") {
            plus_path = side_path(p);
        }
    }

    let path = rename_to
        .clone()
        .or(plus_path)
        .or(minus_path)
        .or_else(|| git_line.and_then(git_line_path))
        .unwrap_or_default();

    let kind = if let Some(from) = rename_from {
        ChangeKind::Renamed {
            from: PathBuf::from(from),
        }
    } else if new_file {
        ChangeKind::Added
    } else if deleted_file {
        ChangeKind::Deleted
    } else {
        ChangeKind::Modified
    };

    (PathBuf::from(path), kind)
}

/// Extract the path from a `---`/`+++` side line's argument (the text after the
/// `--- `/`+++ ` marker). Returns `None` for `/dev/null`. Strips the `a/`/`b/`
/// diff prefix (absent under `--no-prefix`) and unquotes C-quoted paths.
fn side_path(arg: &str) -> Option<String> {
    // Plain `diff -u` appends a tab + timestamp; git does not, but split defensively.
    let arg = arg.split('\t').next().unwrap_or(arg);
    if arg == "/dev/null" {
        return None;
    }
    Some(strip_diff_prefix(&unquote_path(arg)))
}

/// Parse the path out of a `diff --git a/<p> b/<p>` line's argument (text after
/// `diff --git `). Used only as a fallback for segments with no `---`/`+++` or
/// rename lines (mode-only changes, binary files). Handles the common
/// no-space-in-path case; with `--no-prefix` and spaces in the path it is
/// inherently ambiguous and we take the leading token.
fn git_line_path(rest: &str) -> Option<String> {
    if let Some(after) = rest.strip_prefix("a/") {
        // `<p> b/<p>` — both sides equal for a mode-only change; split at " b/".
        if let Some(idx) = after.rfind(" b/") {
            return Some(strip_diff_prefix(&after[..idx]));
        }
    }
    rest.split_whitespace()
        .next()
        .map(|t| strip_diff_prefix(&unquote_path(t)))
}

/// Strip a leading `a/` or `b/` diff prefix if present.
fn strip_diff_prefix(p: &str) -> String {
    p.strip_prefix("a/")
        .or_else(|| p.strip_prefix("b/"))
        .unwrap_or(p)
        .to_string()
}

/// Decode a git C-quoted path (`core.quotepath`). Non-quoted input is returned
/// unchanged. Handles `\\`, `\"`, `\n`, `\t`, `\r`, and `\nnn` octal byte escapes.
fn unquote_path(s: &str) -> String {
    if s.len() < 2 || !s.starts_with('"') || !s.ends_with('"') {
        return s.to_string();
    }
    let inner = &s.as_bytes()[1..s.len() - 1];
    let mut out: Vec<u8> = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        let b = inner[i];
        if b == b'\\' && i + 1 < inner.len() {
            match inner[i + 1] {
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                }
                b't' => {
                    out.push(b'\t');
                    i += 2;
                }
                b'r' => {
                    out.push(b'\r');
                    i += 2;
                }
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                }
                b'"' => {
                    out.push(b'"');
                    i += 2;
                }
                b'0'..=b'7' => {
                    let mut val: u32 = 0;
                    let mut k = i + 1;
                    let mut n = 0;
                    while k < inner.len() && n < 3 && matches!(inner[k], b'0'..=b'7') {
                        val = val * 8 + u32::from(inner[k] - b'0');
                        k += 1;
                        n += 1;
                    }
                    out.push(val as u8);
                    i = k;
                }
                _ => {
                    out.push(b);
                    i += 1;
                }
            }
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

    #[test]
    fn chunk_hash_is_stable_and_content_sensitive() {
        let d = parse("@@ -1,1 +1,1 @@\n-a\n+b\n@@ -10,1 +10,1 @@\n-c\n+d\n");
        // Re-parsing identical text yields identical per-chunk hashes.
        let d2 = parse("@@ -1,1 +1,1 @@\n-a\n+b\n@@ -10,1 +10,1 @@\n-c\n+d\n");
        assert_eq!(hash_chunk(&d, &d.chunks[0]), hash_chunk(&d2, &d2.chunks[0]));
        // Distinct chunks hash differently.
        assert_ne!(hash_chunk(&d, &d.chunks[0]), hash_chunk(&d, &d.chunks[1]));
        // Editing a chunk's content changes only that chunk's hash.
        let edited = parse("@@ -1,1 +1,1 @@\n-a\n+B\n@@ -10,1 +10,1 @@\n-c\n+d\n");
        assert_ne!(
            hash_chunk(&d, &d.chunks[0]),
            hash_chunk(&edited, &edited.chunks[0])
        );
        assert_eq!(
            hash_chunk(&d, &d.chunks[1]),
            hash_chunk(&edited, &edited.chunks[1])
        );
    }

    #[test]
    fn file_hashes_cover_whole_and_each_chunk() {
        let d = parse("@@ -1,1 +1,1 @@\n-a\n+b\n@@ -10,1 +10,1 @@\n-c\n+d\n");
        let fh = file_hashes(&d);
        assert_eq!(fh.whole, hash_text(&d.text));
        assert_eq!(fh.chunks.len(), 2);
        assert_eq!(fh.chunks[0], hash_chunk(&d, &d.chunks[0]));
    }

    #[test]
    fn split_separates_files_and_classifies_kinds() {
        // A modified file, a new file, a deleted file, and a rename — the four
        // kinds `git diff` emits in a working-tree comparison.
        let raw = concat!(
            "diff --git a/src/lib.rs b/src/lib.rs\n",
            "index 111..222 100644\n",
            "--- a/src/lib.rs\n",
            "+++ b/src/lib.rs\n",
            "@@ -1,2 +1,2 @@\n",
            " keep\n",
            "-old\n",
            "+new\n",
            "diff --git a/new.txt b/new.txt\n",
            "new file mode 100644\n",
            "index 000..333\n",
            "--- /dev/null\n",
            "+++ b/new.txt\n",
            "@@ -0,0 +1,1 @@\n",
            "+hello\n",
            "diff --git a/gone.txt b/gone.txt\n",
            "deleted file mode 100644\n",
            "index 444..000\n",
            "--- a/gone.txt\n",
            "+++ /dev/null\n",
            "@@ -1,1 +0,0 @@\n",
            "-bye\n",
            "diff --git a/old/name.rs b/new/name.rs\n",
            "similarity index 100%\n",
            "rename from old/name.rs\n",
            "rename to new/name.rs\n",
        );
        let files = split_unified(raw);
        assert_eq!(files.len(), 4);

        assert_eq!(files[0].0.path, PathBuf::from("src/lib.rs"));
        assert_eq!(files[0].0.kind, ChangeKind::Modified);
        assert_eq!(files[0].0.additions, 1);
        assert_eq!(files[0].0.deletions, 1);

        assert_eq!(files[1].0.path, PathBuf::from("new.txt"));
        assert_eq!(files[1].0.kind, ChangeKind::Added);
        assert_eq!(files[1].0.additions, 1);

        assert_eq!(files[2].0.path, PathBuf::from("gone.txt"));
        assert_eq!(files[2].0.kind, ChangeKind::Deleted);
        assert_eq!(files[2].0.deletions, 1);

        assert_eq!(files[3].0.path, PathBuf::from("new/name.rs"));
        assert_eq!(
            files[3].0.kind,
            ChangeKind::Renamed {
                from: PathBuf::from("old/name.rs")
            }
        );
        // A pure 100%-rename carries no chunk body.
        assert!(files[3].1.chunks.is_empty());
    }

    #[test]
    fn split_handles_binary_and_mode_only() {
        // Binary and mode-only segments have no `---`/`+++` lines: the path must
        // come from the `diff --git` line.
        let raw = concat!(
            "diff --git a/img.png b/img.png\n",
            "index 111..222 100644\n",
            "Binary files a/img.png and b/img.png differ\n",
            "diff --git a/run.sh b/run.sh\n",
            "old mode 100644\n",
            "new mode 100755\n",
        );
        let files = split_unified(raw);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0.path, PathBuf::from("img.png"));
        assert!(files[0].1.is_binary);
        assert_eq!(files[1].0.path, PathBuf::from("run.sh"));
        assert_eq!(files[1].0.kind, ChangeKind::Modified);
        assert!(files[1].1.chunks.is_empty());
    }

    #[test]
    fn split_skips_combined_merge_diffs() {
        // `diff --cc` segments (merge/conflict) use `@@@` two-column markers the
        // unified parser can't read, so they're dropped — but normal segments in
        // the same stream still parse.
        let raw = concat!(
            "diff --cc merged.rs\n",
            "index 111,222..333\n",
            "@@@ -1,1 -1,1 +1,1 @@@\n",
            "- a\n",
            " -b\n",
            "++c\n",
            "diff --git a/plain.rs b/plain.rs\n",
            "--- a/plain.rs\n",
            "+++ b/plain.rs\n",
            "@@ -1,1 +1,1 @@\n",
            "-x\n",
            "+y\n",
        );
        let files = split_unified(raw);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0.path, PathBuf::from("plain.rs"));
    }

    #[test]
    fn split_ignores_show_preamble_and_handles_no_prefix() {
        // `git show` prepends a commit header before the first `diff --git`; it
        // must be ignored. Also exercise `--no-prefix` (no `a/`/`b/`).
        let raw = concat!(
            "commit deadbeef\n",
            "Author: Someone <s@example.com>\n",
            "\n",
            "    a commit message\n",
            "\n",
            "diff --git foo.rs foo.rs\n",
            "--- foo.rs\n",
            "+++ foo.rs\n",
            "@@ -1,1 +1,1 @@\n",
            "-a\n",
            "+b\n",
        );
        let files = split_unified(raw);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0.path, PathBuf::from("foo.rs"));
        assert_eq!(files[0].0.additions, 1);
    }

    #[test]
    fn split_unquotes_quoted_unicode_path() {
        // With core.quotepath on, a non-ASCII path is C-quoted with octal escapes.
        // "café.txt" → é is UTF-8 0xC3 0xA9 → \303\251.
        let raw = concat!(
            "diff --git \"a/caf\\303\\251.txt\" \"b/caf\\303\\251.txt\"\n",
            "--- \"a/caf\\303\\251.txt\"\n",
            "+++ \"b/caf\\303\\251.txt\"\n",
            "@@ -1,1 +1,1 @@\n",
            "-a\n",
            "+b\n",
        );
        let files = split_unified(raw);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0.path, PathBuf::from("café.txt"));
    }

    #[test]
    fn split_empty_input_is_empty() {
        assert!(split_unified("").is_empty());
        assert!(split_unified("commit abc\n\n    no diff here\n").is_empty());
    }
}
