//! Per-file added/deleted line counts, fetched up front so the tree can show
//! `+N -N` for every changed file *before* its diff is hydrated.
//!
//! `git diff --numstat` reports counts for the whole changed set in one cheap
//! command (far cheaper than parsing each file's full diff), keyed by path.
//! Untracked files aren't part of a diff against the base, so their additions
//! are counted directly from the working-tree file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::git::command;
use crate::git::diff::DiffBase;
use crate::model::file::ChangedFile;

/// Files larger than this are left without an up-front count (the exact numbers
/// still appear once the file's diff is hydrated). Guards against reading a huge
/// untracked blob just to draw a tree row.
const MAX_UNTRACKED_BYTES: usize = 1 << 20; // 1 MiB

/// Fill in `additions`/`deletions` for every entry in `files`. Tracked files are
/// taken from a single `git diff --numstat`; untracked files are counted from
/// disk. Best-effort: anything we can't determine is left at its current value.
pub fn fill_line_counts(repo_root: &Path, base: DiffBase, files: &mut [ChangedFile]) {
    let tracked = tracked_counts(repo_root, base);
    for f in files.iter_mut() {
        if f.kind.is_untracked() {
            if let Some(added) = untracked_additions(repo_root, &f.path) {
                f.additions = added;
                f.deletions = 0;
            }
        } else if let Some(&(a, d)) = tracked.get(&f.path) {
            f.additions = a;
            f.deletions = d;
        }
    }
}

/// Run `git diff --numstat` against `base` and parse it into a path → (added,
/// deleted) map. Returns an empty map on any failure (counts just stay 0).
fn tracked_counts(repo_root: &Path, base: DiffBase) -> HashMap<PathBuf, (u32, u32)> {
    match command::capture_diff(
        repo_root,
        &[
            "diff",
            "--numstat",
            "--no-ext-diff",
            "-z",
            "-M",
            base.as_arg(),
        ],
    ) {
        Ok(out) => parse_numstat_z(&out),
        Err(_) => HashMap::new(),
    }
}

/// Parse the NUL-delimited `--numstat -z` stream. Each entry is
/// `added\tdeleted\t<path>`; for a rename the path field is empty and the pre-
/// and post-image paths follow as two separate NUL-terminated fields (we key on
/// the post-image, matching how the rest of the app names a renamed file).
/// Binary files report `-` for both counts and are skipped.
pub fn parse_numstat_z(bytes: &[u8]) -> HashMap<PathBuf, (u32, u32)> {
    let mut map = HashMap::new();
    let mut chunks = bytes.split(|&b| b == 0);
    while let Some(chunk) = chunks.next() {
        if chunk.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(chunk);
        let mut parts = s.splitn(3, '\t');
        let (Some(a), Some(d), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let path = if rest.is_empty() {
            // Rename: the next two fields are the old and new paths.
            let _old = chunks.next();
            match chunks.next() {
                Some(new) => String::from_utf8_lossy(new).into_owned(),
                None => continue,
            }
        } else {
            rest.to_string()
        };
        if let (Ok(added), Ok(deleted)) = (a.parse::<u32>(), d.parse::<u32>()) {
            map.insert(PathBuf::from(path), (added, deleted));
        }
    }
    map
}

/// Count the lines of an untracked working-tree file (its additions, since the
/// whole file is new). `None` for a missing, oversized, or binary file.
fn untracked_additions(repo_root: &Path, path: &Path) -> Option<u32> {
    let data = std::fs::read(repo_root.join(path)).ok()?;
    if data.len() > MAX_UNTRACKED_BYTES {
        return None;
    }
    if data.iter().take(8192).any(|&b| b == 0) {
        return None; // binary
    }
    if data.is_empty() {
        return Some(0);
    }
    let newlines = data.iter().filter(|&&b| b == b'\n').count() as u32;
    // A file without a trailing newline still has a final (unterminated) line.
    let trailing = u32::from(data.last() != Some(&b'\n'));
    Some(newlines + trailing)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Join entries with NUL separators, avoiding escape literals where a digit
    /// follows a NUL (which reads as an octal-looking escape).
    fn z(entries: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for e in entries {
            out.extend_from_slice(e.as_bytes());
            out.push(0);
        }
        out
    }

    #[test]
    fn parses_normal_entries() {
        let raw = z(&["3\t1\tsrc/foo.rs", "10\t0\tnew.txt"]);
        let map = parse_numstat_z(&raw);
        assert_eq!(map.get(Path::new("src/foo.rs")), Some(&(3, 1)));
        assert_eq!(map.get(Path::new("new.txt")), Some(&(10, 0)));
    }

    #[test]
    fn parses_rename_keyed_on_new_path() {
        // `added\tdeleted\t` then preimage then postimage.
        let raw = z(&["2\t2\t", "old/name.rs", "new/name.rs"]);
        let map = parse_numstat_z(&raw);
        assert_eq!(map.get(Path::new("new/name.rs")), Some(&(2, 2)));
        assert!(!map.contains_key(Path::new("old/name.rs")));
    }

    #[test]
    fn skips_binary_entries() {
        let raw = z(&["-\t-\timage.png"]);
        let map = parse_numstat_z(&raw);
        assert!(map.is_empty());
    }

    #[test]
    fn handles_mixed_stream_with_rename() {
        let raw = z(&["5\t0\ta.rs", "7\t3\t", "from.rs", "to.rs", "-\t-\tbin.dat"]);
        let map = parse_numstat_z(&raw);
        assert_eq!(map.get(Path::new("a.rs")), Some(&(5, 0)));
        assert_eq!(map.get(Path::new("to.rs")), Some(&(7, 3)));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn paths_with_spaces_survive() {
        let raw = z(&["1\t1\tdir/a b c.txt"]);
        let map = parse_numstat_z(&raw);
        assert_eq!(map.get(Path::new("dir/a b c.txt")), Some(&(1, 1)));
    }
}
