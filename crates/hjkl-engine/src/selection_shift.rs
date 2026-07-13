//! Keep selections valid across an edit (#63).
//!
//! Multi-cursor edits cascade: an edit made at selection N moves every position
//! after it, so the other selections have to be rewritten or they silently point
//! at the wrong text. This module is that rewrite, as a pure function over
//! `(Position, Edit)` — no editor, no buffer mutation.
//!
//! # Contract
//!
//! [`shift_position`] is called with the **pre-edit** buffer geometry, i.e.
//! before `edit` is applied. It answers: where does this selection end up once
//! the edit lands?
//!
//! It returns `Option`, and `None` means **"this position cannot be tracked
//! through this edit — drop it"**. That is deliberate. The alternative for an
//! edit whose geometry we do not model exactly is to guess, and a guessed
//! position is a selection pointing at the wrong text: the edit still applies,
//! just somewhere the user did not ask for. Dropping degrades multi-cursor to
//! single-cursor, which is visible and harmless; guessing corrupts the buffer,
//! which is neither.
//!
//! Today `None` is returned for the four structural edits — `JoinLines`,
//! `SplitLines`, `InsertBlock`, `DeleteBlockChunks`. Their geometry is
//! row-restructuring and needs pre-edit line metrics to model exactly; that is
//! the next slice, not a silent approximation in this one.
//!
//! # Position semantics
//!
//! A position exactly at an insertion point moves right (the text lands before
//! it). A position strictly inside a deleted range collapses to the range start.

use hjkl_buffer::{Edit, MotionKind, Position};

/// Order positions in document order.
fn key(p: Position) -> (usize, usize) {
    (p.row, p.col)
}

/// Where `p` lands after `text` is inserted at `at`.
fn after_insert(p: Position, at: Position, text: &str) -> Position {
    if key(p) < key(at) {
        return p;
    }
    let added_rows = text.matches('\n').count();
    // Chars on the final line of the inserted text — what a position on `at`'s
    // row gets pushed right by once the newlines have moved it down.
    let tail = text.rsplit('\n').next().unwrap_or("");
    let tail_len = tail.chars().count();

    if p.row == at.row {
        if added_rows == 0 {
            Position::new(p.row, p.col + text.chars().count())
        } else {
            // Everything at/after `at.col` slides onto the last inserted row.
            Position::new(p.row + added_rows, tail_len + (p.col - at.col))
        }
    } else {
        // Strictly below the insertion: only the row shifts.
        Position::new(p.row + added_rows, p.col)
    }
}

/// Where `p` lands after the charwise range `[start, end)` is deleted.
fn after_delete_char(p: Position, start: Position, end: Position) -> Position {
    if key(p) <= key(start) {
        return p;
    }
    if key(p) < key(end) {
        // Inside the hole — collapse to where the text used to begin.
        return start;
    }
    if p.row == end.row {
        // Tail of the end row folds up onto the start row.
        Position::new(start.row, start.col + (p.col - end.col))
    } else {
        Position::new(p.row - (end.row - start.row), p.col)
    }
}

/// Where `p` lands after whole rows `start_row..=end_row` are deleted.
fn after_delete_lines(p: Position, start_row: usize, end_row: usize) -> Position {
    if p.row < start_row {
        p
    } else if p.row <= end_row {
        // The row the selection lived on is gone.
        Position::new(start_row, 0)
    } else {
        Position::new(p.row - (end_row - start_row + 1), p.col)
    }
}

/// Where `p` lands after the rectangle `rows × [lo_col, hi_col]` is deleted.
fn after_delete_block(p: Position, start: Position, end: Position) -> Position {
    let (lo_row, hi_row) = (start.row.min(end.row), start.row.max(end.row));
    let (lo_col, hi_col) = (start.col.min(end.col), start.col.max(end.col));
    if p.row < lo_row || p.row > hi_row {
        return p;
    }
    let width = hi_col - lo_col + 1;
    if p.col > hi_col {
        Position::new(p.row, p.col - width)
    } else if p.col >= lo_col {
        Position::new(p.row, lo_col)
    } else {
        p
    }
}

/// Rewrite `p` so it still points at the same text after `edit` lands, or
/// `None` when the edit's geometry is not modelled and the position must be
/// dropped rather than guessed.
///
/// # Units
///
/// Works in **char columns**, which is what [`Edit`] and `Buffer::cursor` both
/// speak. Deliberately *not* expressed over [`crate::types::Selection`], whose
/// `Pos::col` counts **graphemes**: doing this arithmetic in grapheme columns
/// would silently mis-shift every position sitting after a multi-byte
/// character. Converting between the two units needs the buffer, so it belongs
/// at the call boundary, not here.
pub fn shift_position(p: Position, edit: &Edit) -> Option<Position> {
    match edit {
        // A `\n` typed as a char restructures rows exactly like the 1-char
        // string would, so route both through the same insert geometry.
        Edit::InsertChar { at, ch } => {
            let mut buf = [0u8; 4];
            Some(after_insert(p, *at, ch.encode_utf8(&mut buf)))
        }
        Edit::InsertStr { at, text } => Some(after_insert(p, *at, text)),
        Edit::DeleteRange { start, end, kind } => Some(match kind {
            MotionKind::Char => after_delete_char(p, *start, *end),
            MotionKind::Line => after_delete_lines(p, start.row, end.row),
            MotionKind::Block => after_delete_block(p, *start, *end),
        }),
        Edit::Replace { start, end, with } => {
            // Delete then insert at the (now collapsed) start.
            let deleted = after_delete_char(p, *start, *end);
            Some(after_insert(deleted, *start, with))
        }
        // Row-restructuring edits: exact tracking needs pre-edit line metrics.
        // Drop rather than approximate — see module docs.
        Edit::JoinLines { .. }
        | Edit::SplitLines { .. }
        | Edit::InsertBlock { .. }
        | Edit::DeleteBlockChunks { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(row: usize, col: usize) -> Position {
        Position::new(row, col)
    }
    fn ins(row: usize, col: usize, text: &str) -> Edit {
        Edit::InsertStr {
            at: p(row, col),
            text: text.to_string(),
        }
    }
    fn del(s: (usize, usize), e: (usize, usize), kind: MotionKind) -> Edit {
        Edit::DeleteRange {
            start: p(s.0, s.1),
            end: p(e.0, e.1),
            kind,
        }
    }
    /// Shift a bare position.
    fn head(row: usize, col: usize, edit: &Edit) -> Option<Position> {
        shift_position(p(row, col), edit)
    }

    // ── Insert ───────────────────────────────────────────────────────────────

    #[test]
    fn insert_before_on_same_row_pushes_right() {
        assert_eq!(head(0, 5, &ins(0, 2, "ab")), Some(p(0, 7)));
    }

    #[test]
    fn insert_after_on_same_row_does_not_move() {
        assert_eq!(head(0, 1, &ins(0, 2, "ab")), Some(p(0, 1)));
    }

    #[test]
    fn position_exactly_at_insertion_point_moves_right() {
        // Text lands *before* the caret, so the caret slides.
        assert_eq!(head(0, 2, &ins(0, 2, "xy")), Some(p(0, 2 + 2)));
    }

    #[test]
    fn insert_on_earlier_row_does_not_move_later_col() {
        assert_eq!(head(3, 4, &ins(1, 0, "abc")), Some(p(3, 4)));
    }

    #[test]
    fn multiline_insert_pushes_later_rows_down() {
        assert_eq!(head(3, 4, &ins(1, 0, "a\nb\n")), Some(p(5, 4)));
    }

    #[test]
    fn multiline_insert_relocates_tail_of_the_insert_row() {
        // "ab|cd" + insert "X\nY" at col 2 -> row0 "abX", row1 "Ycd";
        // the caret that was at col 2 is now on row 1 after "Y".
        assert_eq!(head(0, 2, &ins(0, 2, "X\nY")), Some(p(1, 1)));
    }

    #[test]
    fn insert_char_newline_restructures_like_a_string() {
        let e = Edit::InsertChar {
            at: p(0, 2),
            ch: '\n',
        };
        assert_eq!(head(0, 5, &e), Some(p(1, 3)));
    }

    // ── Charwise delete ──────────────────────────────────────────────────────

    #[test]
    fn delete_before_pulls_left() {
        assert_eq!(
            head(0, 9, &del((0, 2), (0, 5), MotionKind::Char)),
            Some(p(0, 6))
        );
    }

    #[test]
    fn delete_after_does_not_move() {
        assert_eq!(
            head(0, 1, &del((0, 2), (0, 5), MotionKind::Char)),
            Some(p(0, 1))
        );
    }

    #[test]
    fn position_inside_deleted_range_collapses_to_start() {
        assert_eq!(
            head(0, 3, &del((0, 2), (0, 5), MotionKind::Char)),
            Some(p(0, 2))
        );
    }

    #[test]
    fn delete_end_is_exclusive() {
        // A caret exactly at `end` survives; it is the first char kept.
        assert_eq!(
            head(0, 5, &del((0, 2), (0, 5), MotionKind::Char)),
            Some(p(0, 2))
        );
    }

    #[test]
    fn cross_row_delete_folds_tail_onto_start_row() {
        // Deleting (1,2)..(3,4): a caret at (3,6) lands at (1, 2 + (6-4)).
        assert_eq!(
            head(3, 6, &del((1, 2), (3, 4), MotionKind::Char)),
            Some(p(1, 4))
        );
    }

    #[test]
    fn row_below_a_cross_row_delete_shifts_up() {
        assert_eq!(
            head(7, 3, &del((1, 2), (3, 4), MotionKind::Char)),
            Some(p(5, 3))
        );
    }

    // ── Linewise delete ──────────────────────────────────────────────────────

    #[test]
    fn linewise_delete_shifts_rows_below_up() {
        assert_eq!(
            head(9, 3, &del((2, 0), (4, 0), MotionKind::Line)),
            Some(p(6, 3))
        );
    }

    #[test]
    fn linewise_delete_of_the_selections_own_row_collapses_it() {
        assert_eq!(
            head(3, 7, &del((2, 0), (4, 0), MotionKind::Line)),
            Some(p(2, 0))
        );
    }

    #[test]
    fn linewise_delete_above_leaves_earlier_rows_alone() {
        assert_eq!(
            head(1, 7, &del((2, 0), (4, 0), MotionKind::Line)),
            Some(p(1, 7))
        );
    }

    // ── Block delete ─────────────────────────────────────────────────────────

    #[test]
    fn block_delete_pulls_columns_right_of_the_rectangle_left() {
        assert_eq!(
            head(2, 9, &del((1, 2), (3, 5), MotionKind::Block)),
            Some(p(2, 5))
        );
    }

    #[test]
    fn block_delete_collapses_columns_inside_the_rectangle() {
        assert_eq!(
            head(2, 3, &del((1, 2), (3, 5), MotionKind::Block)),
            Some(p(2, 2))
        );
    }

    #[test]
    fn block_delete_leaves_rows_outside_the_rectangle_alone() {
        assert_eq!(
            head(9, 9, &del((1, 2), (3, 5), MotionKind::Block)),
            Some(p(9, 9))
        );
    }

    // ── Replace ──────────────────────────────────────────────────────────────

    #[test]
    fn replace_shorter_pulls_left() {
        let e = Edit::Replace {
            start: p(0, 2),
            end: p(0, 6),
            with: "x".to_string(),
        };
        // "ab[cdef]gh" -> "ab x gh": a caret at col 8 moves to 2 + 1 + (8-6) = 5.
        assert_eq!(head(0, 8, &e), Some(p(0, 5)));
    }

    #[test]
    fn replace_longer_pushes_right() {
        let e = Edit::Replace {
            start: p(0, 2),
            end: p(0, 3),
            with: "xyz".to_string(),
        };
        assert_eq!(head(0, 5, &e), Some(p(0, 7)));
    }

    // ── Untracked edits drop rather than guess ───────────────────────────────

    #[test]
    fn structural_edits_drop_the_selection_instead_of_guessing() {
        // Dropping degrades multi-cursor to single-cursor (visible, harmless).
        // Guessing would leave a selection pointing at the wrong text and let a
        // later edit apply somewhere the user never asked for.
        let joins = Edit::JoinLines {
            row: 0,
            count: 2,
            with_space: true,
        };
        assert_eq!(shift_position(p(5, 0), &joins), None);

        let splits = Edit::SplitLines {
            row: 0,
            cols: vec![3],
            inserted_space: true,
        };
        assert_eq!(shift_position(p(5, 0), &splits), None);

        let block_ins = Edit::InsertBlock {
            at: p(0, 0),
            chunks: vec!["x".into()],
        };
        assert_eq!(shift_position(p(5, 0), &block_ins), None);

        let block_del = Edit::DeleteBlockChunks {
            at: p(0, 0),
            widths: vec![1],
        };
        assert_eq!(shift_position(p(5, 0), &block_del), None);
    }
}
