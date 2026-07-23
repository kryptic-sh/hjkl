//! Char-boundary snapping for rope byte ranges in the highlight path.
//!
//! Tree-sitter node byte offsets are char-aligned *for the exact source that
//! was parsed*. When the retained tree is stale relative to the current rope
//! — a reparse timed out and the old tree was kept (see the highlight path in
//! `hjkl-syntax`, which proceeds with the previous tree when
//! `parse_incremental_rope` returns `false`) — a node's byte range can land in
//! the middle of a multi-byte char in the *current* rope. `ropey`'s
//! `byte_slice` panics on a non-char-boundary index, so a stale frame over
//! multi-byte content (emoji/CJK) crashed the editor instead of merely
//! mis-coloring one frame. These helpers floor/ceil arbitrary byte indices to
//! safe boundaries; for an aligned (non-stale) node they are the identity, so
//! the common path is unchanged.

use std::ops::Range;

/// Largest char-boundary byte index `<= byte_idx` (clamped to rope length).
pub(crate) fn floor_char_boundary(rope: &ropey::Rope, byte_idx: usize) -> usize {
    let byte_idx = byte_idx.min(rope.len_bytes());
    rope.char_to_byte(rope.byte_to_char(byte_idx))
}

/// Smallest char-boundary byte index `>= byte_idx` (clamped to rope length).
pub(crate) fn ceil_char_boundary(rope: &ropey::Rope, byte_idx: usize) -> usize {
    let byte_idx = byte_idx.min(rope.len_bytes());
    let char_idx = rope.byte_to_char(byte_idx);
    let floored = rope.char_to_byte(char_idx);
    if floored == byte_idx {
        byte_idx
    } else {
        rope.char_to_byte(char_idx + 1)
    }
}

/// Snap `[start, end)` to enclosing char boundaries, clamped to the rope and
/// normalised so `start <= end`. Floors `start`, ceils `end`: the result is a
/// superset of whole chars, never a mid-char split. Returns an empty range
/// (`start..start`) when the inputs cross after clamping. For an already
/// char-aligned, in-bounds range this is the identity.
pub(crate) fn safe_char_range(rope: &ropey::Rope, start: usize, end: usize) -> Range<usize> {
    let len = rope.len_bytes();
    let start = floor_char_boundary(rope, start.min(len));
    let end = ceil_char_boundary(rope, end.min(len));
    if start >= end {
        start..start
    } else {
        start..end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    // "aé😀b": a=[0], é=[1..3] (2 bytes), 😀=[3..7] (4 bytes), b=[7..8], len=8.
    #[test]
    fn floor_and_ceil_snap_interior_multibyte() {
        let rope = Rope::from_str("aé😀b");
        assert_eq!(floor_char_boundary(&rope, 2), 1); // inside é
        assert_eq!(ceil_char_boundary(&rope, 2), 3);
        assert_eq!(floor_char_boundary(&rope, 5), 3); // inside 😀
        assert_eq!(ceil_char_boundary(&rope, 5), 7);
        // Aligned indices are identity.
        assert_eq!(floor_char_boundary(&rope, 3), 3);
        assert_eq!(ceil_char_boundary(&rope, 3), 3);
    }

    #[test]
    fn clamps_past_end() {
        let rope = Rope::from_str("aé😀b");
        assert_eq!(floor_char_boundary(&rope, 999), 8);
        assert_eq!(ceil_char_boundary(&rope, 999), 8);
    }

    #[test]
    fn safe_range_never_splits_char() {
        let rope = Rope::from_str("aé😀b");
        let n = rope.len_bytes();
        // Every stale-offset pair (including mid-char and out-of-bounds) must
        // yield a slice-able, ordered, char-aligned range — no panic.
        for s in 0..=n + 4 {
            for e in 0..=n + 4 {
                let r = safe_char_range(&rope, s, e);
                assert!(r.start <= r.end);
                // Must not panic — the whole point of the helper.
                let _ = rope.byte_slice(r).to_string();
            }
        }
    }

    #[test]
    fn aligned_range_is_identity() {
        let rope = Rope::from_str("aé😀b");
        assert_eq!(safe_char_range(&rope, 1, 7), 1..7);
        assert_eq!(safe_char_range(&rope, 3, 3), 3..3);
    }
}
