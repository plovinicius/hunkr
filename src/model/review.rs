//! Reviewed-state model.
//!
//! Reviewed status is tied to a diff's **content hash**, not a name. Review is
//! tracked per *chunk*: a record stores the content hash of every chunk the user
//! marked reviewed. Status is *derived* by comparing those against the file's
//! current chunk hashes, so a chunk that changes again automatically falls back
//! to unreviewed — and only that chunk, not the whole file. Files with no chunks
//! (binary, mode-only) fall back to a whole-diff hash comparison.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewStatus {
    /// No record, or the file has changed since the user marked it.
    Unreviewed,
    /// Reviewed and the diff still matches.
    Reviewed,
}

/// What the user marked: the whole-diff hash at review time (for binary/no-chunk
/// files and legacy records), the content hashes of reviewed chunks, plus when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRecord {
    pub reviewed_hash: u64,
    pub reviewed_at: i64,
    /// Content hashes of the chunks the user has marked reviewed. Empty for
    /// binary/no-chunk files (and for records written by schema 1).
    #[serde(default)]
    pub reviewed_chunks: Vec<u64>,
}

/// Current hashes for one file's diff: the whole-diff hash plus a content hash
/// per chunk, in order. Computed off the UI thread for reviewed files and on
/// hydration for the selected file.
#[derive(Debug, Clone, Default)]
pub struct FileHashes {
    pub whole: u64,
    pub chunks: Vec<u64>,
}

/// Derive a file's display status from its record (if any) and current hashes
/// (if known). A file with chunks reads as reviewed only when *every* current
/// chunk hash is in the reviewed set; a file with no chunks falls back to a
/// whole-diff hash match.
pub fn derive_file(record: Option<&ReviewRecord>, hashes: Option<&FileHashes>) -> ReviewStatus {
    match (record, hashes) {
        (Some(r), Some(h)) => {
            let reviewed = if h.chunks.is_empty() {
                r.reviewed_hash == h.whole
            } else {
                h.chunks.iter().all(|c| r.reviewed_chunks.contains(c))
            };
            if reviewed {
                ReviewStatus::Reviewed
            } else {
                ReviewStatus::Unreviewed
            }
        }
        _ => ReviewStatus::Unreviewed,
    }
}

/// How many of the file's current chunks are reviewed (for the sidebar `n/m`).
pub fn reviewed_chunk_count(record: Option<&ReviewRecord>, hashes: Option<&FileHashes>) -> usize {
    match (record, hashes) {
        (Some(r), Some(h)) => h
            .chunks
            .iter()
            .filter(|c| r.reviewed_chunks.contains(c))
            .count(),
        _ => 0,
    }
}

/// Whether a single chunk (by content hash) is marked reviewed.
pub fn chunk_reviewed(record: Option<&ReviewRecord>, chunk_hash: u64) -> bool {
    record.is_some_and(|r| r.reviewed_chunks.contains(&chunk_hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(chunks: &[u64]) -> ReviewRecord {
        ReviewRecord {
            reviewed_hash: 0,
            reviewed_at: 0,
            reviewed_chunks: chunks.to_vec(),
        }
    }

    fn fh(whole: u64, chunks: &[u64]) -> FileHashes {
        FileHashes {
            whole,
            chunks: chunks.to_vec(),
        }
    }

    #[test]
    fn file_reviewed_only_when_all_chunks_reviewed() {
        let h = fh(0, &[1, 2, 3]);
        assert_eq!(derive_file(None, Some(&h)), ReviewStatus::Unreviewed);
        assert_eq!(
            derive_file(Some(&rec(&[1, 2])), Some(&h)),
            ReviewStatus::Unreviewed
        );
        assert_eq!(
            derive_file(Some(&rec(&[1, 2, 3])), Some(&h)),
            ReviewStatus::Reviewed
        );
        // A changed chunk (hash 3 → 9) drops the file back to unreviewed.
        assert_eq!(
            derive_file(Some(&rec(&[1, 2, 3])), Some(&fh(0, &[1, 2, 9]))),
            ReviewStatus::Unreviewed
        );
    }

    #[test]
    fn binary_file_falls_back_to_whole_hash() {
        let rec = ReviewRecord {
            reviewed_hash: 42,
            reviewed_at: 0,
            reviewed_chunks: vec![],
        };
        assert_eq!(
            derive_file(Some(&rec), Some(&fh(42, &[]))),
            ReviewStatus::Reviewed
        );
        assert_eq!(
            derive_file(Some(&rec), Some(&fh(43, &[]))),
            ReviewStatus::Unreviewed
        );
    }

    #[test]
    fn counts_reviewed_chunks() {
        let h = fh(0, &[1, 2, 3, 4]);
        assert_eq!(reviewed_chunk_count(Some(&rec(&[1, 3])), Some(&h)), 2);
        assert_eq!(reviewed_chunk_count(None, Some(&h)), 0);
        assert!(chunk_reviewed(Some(&rec(&[1, 3])), 3));
        assert!(!chunk_reviewed(Some(&rec(&[1, 3])), 2));
    }
}
