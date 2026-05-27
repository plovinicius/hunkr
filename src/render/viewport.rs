//! Virtualization math: given a scroll offset, a viewport height, and a total
//! row count, decide which rows are visible. The diff panel renders only this
//! window, so per-frame cost is O(viewport height) regardless of diff size.

use std::ops::Range;

/// Clamp a scroll offset so the last row can sit at the bottom of the viewport
/// but we never scroll past the end.
pub fn clamp_offset(offset: usize, height: usize, total: usize) -> usize {
    let max = total.saturating_sub(height);
    offset.min(max)
}

/// The half-open range of row indices visible for a given (clamped) scroll.
pub fn visible_range(offset: usize, height: usize, total: usize) -> Range<usize> {
    let start = clamp_offset(offset, height, total);
    let end = (start + height).min(total);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_never_exceeds_max() {
        // 100 rows, viewport 10 → max top offset is 90.
        assert_eq!(clamp_offset(95, 10, 100), 90);
        assert_eq!(clamp_offset(50, 10, 100), 50);
        assert_eq!(clamp_offset(0, 10, 100), 0);
    }

    #[test]
    fn small_content_pins_to_top() {
        // Fewer rows than the viewport → always offset 0.
        assert_eq!(clamp_offset(5, 20, 3), 0);
        assert_eq!(visible_range(5, 20, 3), 0..3);
    }

    #[test]
    fn window_is_height_bounded() {
        assert_eq!(visible_range(40, 10, 100), 40..50);
        // Clamped at the bottom.
        assert_eq!(visible_range(200, 10, 100), 90..100);
    }
}
