//! Parse `git status --porcelain=v2 -z` into the changed-file list.
//!
//! We use porcelain v2 (stable, machine-readable, includes rename info) with
//! `-z` so records are NUL-delimited — robust to spaces and non-ASCII in paths.
//! Scope is "everything that differs from the base": staged + unstaged + all
//! untracked, surfaced once per path.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::git::command;
use crate::model::file::{ChangeKind, ChangedFile};

/// Fetch and parse the changed-file list for the repository.
pub fn changed_files(repo_root: &Path) -> Result<Vec<ChangedFile>> {
    let out = command::capture(
        repo_root,
        &[
            "-c",
            "core.quotepath=false",
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
        ],
    )?;
    Ok(parse_status_v2(&out))
}

/// Parse the raw NUL-delimited porcelain-v2 byte stream.
pub fn parse_status_v2(bytes: &[u8]) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    // Records are NUL-terminated. Rename/copy ("2") records are special: the
    // original path is a *separate* trailing NUL field, so we pull it from the
    // iterator when we see one.
    let mut tokens = bytes
        .split(|&b| b == 0)
        .filter(|t| !t.is_empty())
        .map(|t| String::from_utf8_lossy(t).into_owned());

    while let Some(rec) = tokens.next() {
        match rec.as_bytes().first() {
            Some(b'1') => {
                if let Some((kind, path)) = parse_ordinary(&rec) {
                    files.push(ChangedFile::new(path, kind));
                }
            }
            Some(b'2') => {
                let orig = tokens.next().unwrap_or_default();
                if let Some(path) = field(&rec, 10) {
                    files.push(ChangedFile::new(
                        PathBuf::from(path),
                        ChangeKind::Renamed {
                            from: PathBuf::from(orig),
                        },
                    ));
                }
            }
            Some(b'?') => {
                // "? <path>"
                let path = rec[2..].to_string();
                files.push(ChangedFile::new(PathBuf::from(path), ChangeKind::Untracked));
            }
            Some(b'u') => {
                // Unmerged; treat as modified for review purposes.
                if let Some(path) = field(&rec, 11) {
                    files.push(ChangedFile::new(PathBuf::from(path), ChangeKind::Modified));
                }
            }
            _ => {} // "!" ignored, or anything unexpected
        }
    }
    files
}

/// Parse an ordinary ("1") record: `1 XY sub mH mI mW hH hI <path>`.
fn parse_ordinary(rec: &str) -> Option<(ChangeKind, PathBuf)> {
    let mut it = rec.splitn(9, ' ');
    it.next()?; // "1"
    let xy = it.next()?;
    for _ in 0..6 {
        it.next()?; // sub mH mI mW hH hI
    }
    let path = it.next()?; // remainder (may contain spaces)
    Some((kind_from_xy(xy), PathBuf::from(path)))
}

/// Return the n-th space-delimited field counting from 1, where the n-th field
/// is the remainder of the string (so a trailing path keeps embedded spaces).
fn field(rec: &str, n: usize) -> Option<String> {
    let mut it = rec.splitn(n, ' ');
    for _ in 0..n - 1 {
        it.next()?;
    }
    it.next().map(|s| s.to_string())
}

/// Derive a [`ChangeKind`] from the two-letter XY status code, taking whichever
/// of the staged (X) or unstaged (Y) columns is most significant.
fn kind_from_xy(xy: &str) -> ChangeKind {
    let b = xy.as_bytes();
    let codes = [
        b.first().copied().unwrap_or(b'.'),
        b.get(1).copied().unwrap_or(b'.'),
    ];
    if codes.contains(&b'A') {
        ChangeKind::Added
    } else if codes.contains(&b'D') {
        ChangeKind::Deleted
    } else if codes.contains(&b'T') {
        ChangeKind::TypeChange
    } else {
        ChangeKind::Modified
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ordinary_and_untracked() {
        // Two ordinary records + one untracked, each NUL-terminated.
        let raw = b"1 .M N... 100644 100644 100644 abc abc src/foo.rs\0\
                    1 A. N... 000000 100644 100644 000 def src/bar.rs\0\
                    ? notes.txt\0";
        let files = parse_status_v2(raw);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, PathBuf::from("src/foo.rs"));
        assert_eq!(files[0].kind, ChangeKind::Modified);
        assert_eq!(files[1].kind, ChangeKind::Added);
        assert_eq!(files[2].kind, ChangeKind::Untracked);
    }

    #[test]
    fn parses_rename_with_trailing_orig_path() {
        // "2" record followed by its original-path field.
        let raw = b"2 R. N... 100644 100644 100644 abc abc R100 new name.rs\0old name.rs\0";
        let files = parse_status_v2(raw);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("new name.rs"));
        assert_eq!(
            files[0].kind,
            ChangeKind::Renamed {
                from: PathBuf::from("old name.rs")
            }
        );
    }

    #[test]
    fn path_with_spaces_is_preserved() {
        let raw = b"1 .M N... 100644 100644 100644 abc abc dir/a b c.txt\0";
        let files = parse_status_v2(raw);
        assert_eq!(files[0].path, PathBuf::from("dir/a b c.txt"));
    }
}
