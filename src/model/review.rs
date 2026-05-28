//! Reviewed-state model.
//!
//! Reviewed status is tied to a file's **diff hash**, not its name. A record
//! stores the diff hash at the moment the user marked the file reviewed; status
//! is *derived* by comparing that against the file's current diff hash, so a
//! file that changes again automatically falls back to unreviewed.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewStatus {
    /// No record, or the file has changed since the user marked it.
    Unreviewed,
    /// Reviewed and the diff still matches.
    Reviewed,
}

/// What the user marked: the diff hash at review time, plus when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRecord {
    pub reviewed_hash: u64,
    pub reviewed_at: i64,
}

/// Derive the display status from the stored record (if any) and the file's
/// current diff hash (if known). Anything other than a record whose hash
/// matches the current one reads as unreviewed.
pub fn derive(record: Option<&ReviewRecord>, current_hash: Option<u64>) -> ReviewStatus {
    match (record, current_hash) {
        (Some(r), Some(h)) if h == r.reviewed_hash => ReviewStatus::Reviewed,
        _ => ReviewStatus::Unreviewed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(hash: u64) -> ReviewRecord {
        ReviewRecord {
            reviewed_hash: hash,
            reviewed_at: 0,
        }
    }

    #[test]
    fn derivation_covers_all_states() {
        assert_eq!(derive(None, Some(1)), ReviewStatus::Unreviewed);
        assert_eq!(derive(Some(&rec(7)), Some(7)), ReviewStatus::Reviewed);
        // Record exists but the file has changed since → unreviewed.
        assert_eq!(derive(Some(&rec(7)), Some(8)), ReviewStatus::Unreviewed);
        // Record exists but the current hash is unknown → unreviewed.
        assert_eq!(derive(Some(&rec(7)), None), ReviewStatus::Unreviewed);
    }
}
