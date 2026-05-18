/// A `(row, col)` location inside a [`crate::Buffer`].
///
/// - `row` is zero-based, in **logical lines** (newline-separated). Wrapping
///   is a render-only concern; no `Position` ever points at a display line.
/// - `col` is zero-based, **char index within the line** — not bytes, not
///   graphemes, not display columns. Width-aware motions go through helpers in
///   `motion.rs`; do not synthesize `Position` from a column count without
///   consulting them. The accompanying [`Position::byte_offset`] helper
///   converts a char-index `col` back to a byte offset when slicing the
///   underlying `String`.
///
/// ## Bounds
///
/// A `Position` is **valid** for a buffer iff:
///
/// - `row < buffer.lines().len()`
/// - `col <= buffer.line(row).unwrap().chars().count()` (one past the last
///   char is allowed — insert mode lives there).
///
/// Pass an out-of-bounds `Position` to [`crate::Buffer::set_cursor`] and the
/// buffer clamps to the nearest valid one via
/// [`crate::Buffer::clamp_position`]. Pass one to
/// [`crate::Buffer::apply_edit`] and the edit is rejected (returns the no-op
/// inverse).
///
/// ## Sticky column
///
/// [`crate::Buffer`] tracks an optional sticky column for `j` / `k` motions:
/// the target column to land in once the cursor reaches a line long enough to
/// honor it. Never reset it manually outside motion code — it survives
/// [`crate::Buffer::set_cursor`] for that exact reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Position {
    pub row: usize,
    pub col: usize,
}

impl Position {
    pub const fn new(row: usize, col: usize) -> Self {
        Self { row, col }
    }

    /// Byte offset of `self.col` (a char index) into `line`. Returns
    /// `line.len()` when `col` is at or past the end of the line —
    /// matches `String::insert` / `replace_range` boundary semantics.
    pub fn byte_offset(self, line: &str) -> usize {
        line.char_indices()
            .nth(self.col)
            .map(|(b, _)| b)
            .unwrap_or(line.len())
    }
}

#[cfg(test)]
mod tests {
    use super::Position;

    #[test]
    fn byte_offset_ascii() {
        assert_eq!(Position::new(0, 0).byte_offset("hello"), 0);
        assert_eq!(Position::new(0, 3).byte_offset("hello"), 3);
        assert_eq!(Position::new(0, 5).byte_offset("hello"), 5);
        // Past end clamps at line length so callers can use it as an
        // insertion point without bounds-check ceremony.
        assert_eq!(Position::new(0, 99).byte_offset("hello"), 5);
    }

    #[test]
    fn byte_offset_utf8() {
        // "tablé" — 'é' is 2 bytes in UTF-8.
        let line = "tablé";
        assert_eq!(Position::new(0, 4).byte_offset(line), 4);
        assert_eq!(Position::new(0, 5).byte_offset(line), 6);
    }

    #[test]
    fn ord_is_row_major() {
        assert!(Position::new(0, 5) < Position::new(1, 0));
        assert!(Position::new(2, 0) > Position::new(1, 999));
        assert!(Position::new(1, 3) < Position::new(1, 4));
    }
}
