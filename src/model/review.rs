//! Reviewed-state model.
//!
//! Reviewed status is tied to a file's **diff hash**, not its name. A record
//! stores the diff hash at the moment the user marked the file reviewed; status
//! is *derived* by comparing that against the file's current diff hash, so a
//! file that changes again automatically surfaces as "changed after review".

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewStatus {
    /// No review record.
    Unreviewed,
    /// Reviewed and the diff still matches.
    Reviewed,
    /// Reviewed earlier, but the diff has changed since.
    ChangedAfterReview,
}

/// What the user marked: the diff hash at review time, plus when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRecord {
    pub reviewed_hash: u64,
    pub reviewed_at: i64,
}

/// Derive the display status from the stored record (if any) and the file's
/// current diff hash (if known). A reviewed file whose current hash is unknown
/// is treated conservatively as changed rather than claimed reviewed.
pub fn derive(record: Option<&ReviewRecord>, current_hash: Option<u64>) -> ReviewStatus {
    match record {
        None => ReviewStatus::Unreviewed,
        Some(r) => match current_hash {
            Some(h) if h == r.reviewed_hash => ReviewStatus::Reviewed,
            _ => ReviewStatus::ChangedAfterReview,
        },
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
        assert_eq!(
            derive(Some(&rec(7)), Some(8)),
            ReviewStatus::ChangedAfterReview
        );
        // Reviewed but current hash unknown → conservatively "changed".
        assert_eq!(
            derive(Some(&rec(7)), None),
            ReviewStatus::ChangedAfterReview
        );
    }
}
