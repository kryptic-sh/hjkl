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
//! Today `None` is returned only for `SplitLines`, which is the undo-inverse of
//! a join: it is emitted when history rewinds, not when a user edits, and undo
//! restores a whole snapshot without preserving secondary carets anyway.
//!
//! `JoinLines`, `InsertBlock` and `DeleteBlockChunks` ARE modelled — they mirror
//! `hjkl_buffer`'s own geometry, and they matter: vim's `J` is a `JoinLines`, and
//! visual-block `I`/`A` are the block edits, so dropping carets on those would
//! make multi-cursor collapse under exactly the operations that need it most.
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

/// Where `p` lands after `count` rows are joined onto `row`.
///
/// Mirrors `hjkl_buffer::Edit::JoinLines` exactly: each step drops the `\n`
/// ending `row` and inserts a single space **only when both sides are
/// non-empty**. (It does not strip leading whitespace — vim's `J` does, this
/// buffer's `JoinLines` does not, and guessing the wrong one here would mis-place
/// every caret on a joined row.)
///
/// `line_len` gives the **pre-edit** char length of a row.
fn after_join(
    p: Position,
    row: usize,
    count: usize,
    with_space: bool,
    line_len: &impl Fn(usize) -> usize,
    rows: usize,
) -> Position {
    if p.row < row {
        return p;
    }
    // Walk the joins, tracking where each joined row's text lands in the merged
    // row. `start_col[k]` is the column original row `row + k` begins at.
    let mut cur_len = line_len(row);
    let mut start_col = Vec::with_capacity(count);
    let mut joined = 0usize;
    for k in 1..=count.max(1) {
        if row + k >= rows {
            break;
        }
        let next_len = line_len(row + k);
        let space = with_space && cur_len > 0 && next_len > 0;
        start_col.push(cur_len + usize::from(space));
        cur_len += usize::from(space) + next_len;
        joined += 1;
    }
    if joined == 0 || p.row == row {
        // The anchor row keeps its columns; text is only appended after it.
        return p;
    }
    if p.row <= row + joined {
        let k = p.row - row; // 1..=joined
        Position::new(row, start_col[k - 1] + p.col)
    } else {
        Position::new(p.row - joined, p.col)
    }
}

/// Where `p` lands after a block insert: `chunks[i]` spliced at
/// `(at.row + i, at.col)`. Rows shorter than `at.col` are space-padded first,
/// but every position on such a row sits left of `at.col` and so cannot move.
fn after_insert_block(p: Position, at: Position, chunks: &[String]) -> Position {
    if p.row < at.row || p.row >= at.row + chunks.len() || p.col < at.col {
        return p;
    }
    let width = chunks[p.row - at.row].chars().count();
    Position::new(p.row, p.col + width)
}

/// Where `p` lands after a block delete: `widths[i]` chars removed at
/// `(at.row + i, at.col)`.
fn after_delete_block_chunks(p: Position, at: Position, widths: &[usize]) -> Position {
    if p.row < at.row || p.row >= at.row + widths.len() || p.col <= at.col {
        return p;
    }
    let w = widths[p.row - at.row];
    if p.col >= at.col + w {
        Position::new(p.row, p.col - w)
    } else {
        // Inside the removed chunk.
        Position::new(p.row, at.col)
    }
}

/// Rewrite `p` so it still points at the same text after `edit` lands, or
/// `None` when the edit's geometry is not modelled and the position must be
/// dropped rather than guessed.
///
/// `line_len` returns the **pre-edit** char length of a row, and `rows` the
/// pre-edit row count. Only the row-restructuring edits consult them.
///
/// # Units
///
/// Works in **char columns**, which is what [`Edit`] and `Buffer::cursor` both
/// speak. Deliberately *not* expressed over [`crate::types::Selection`], whose
/// `Pos::col` counts **graphemes**: doing this arithmetic in grapheme columns
/// would silently mis-shift every position sitting after a multi-byte
/// character. Converting between the two units needs the buffer, so it belongs
/// at the call boundary, not here.
pub fn shift_position(
    p: Position,
    edit: &Edit,
    line_len: impl Fn(usize) -> usize,
    rows: usize,
) -> Option<Position> {
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
        Edit::JoinLines {
            row,
            count,
            with_space,
        } => Some(after_join(p, *row, *count, *with_space, &line_len, rows)),
        Edit::InsertBlock { at, chunks } => Some(after_insert_block(p, *at, chunks)),
        Edit::DeleteBlockChunks { at, widths } => Some(after_delete_block_chunks(p, *at, widths)),
        // `SplitLines` is the undo-inverse of a join, emitted when history rewinds
        // rather than when a user edits. Undo restores a whole snapshot and does
        // not preserve secondary carets anyway, so modelling it would buy nothing
        // real — drop rather than write geometry no test could justify.
        Edit::SplitLines { .. } => None,
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
    /// Shift a bare position against a buffer with no interesting geometry.
    /// (`line_len` / `rows` only matter for `JoinLines`; the join tests below
    /// pass real metrics.)
    fn head(row: usize, col: usize, edit: &Edit) -> Option<Position> {
        shift_position(p(row, col), edit, |_| 0, 0)
    }

    /// Shift against explicit pre-edit line lengths.
    fn head_in(row: usize, col: usize, edit: &Edit, lens: &[usize]) -> Option<Position> {
        shift_position(p(row, col), edit, |r| lens[r], lens.len())
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

    // ── Join ─────────────────────────────────────────────────────────────────

    #[test]
    fn join_folds_the_next_row_up_after_the_anchor_plus_a_space() {
        // rows: "abc"(3) "de"(2). J -> "abc de"; a caret at (1,1) lands at col 4+1.
        let e = Edit::JoinLines {
            row: 0,
            count: 1,
            with_space: true,
        };
        assert_eq!(head_in(1, 1, &e, &[3, 2]), Some(p(0, 5)));
    }

    #[test]
    fn join_without_space_folds_flush() {
        let e = Edit::JoinLines {
            row: 0,
            count: 1,
            with_space: false,
        };
        assert_eq!(head_in(1, 1, &e, &[3, 2]), Some(p(0, 4)));
    }

    #[test]
    fn join_inserts_no_space_when_a_side_is_empty() {
        // The buffer only inserts a space when BOTH sides are non-empty.
        let e = Edit::JoinLines {
            row: 0,
            count: 1,
            with_space: true,
        };
        assert_eq!(
            head_in(1, 1, &e, &[0, 2]),
            Some(p(0, 1)),
            "empty prefix -> no space"
        );
    }

    #[test]
    fn join_leaves_the_anchor_rows_own_columns_alone() {
        let e = Edit::JoinLines {
            row: 0,
            count: 1,
            with_space: true,
        };
        assert_eq!(head_in(0, 2, &e, &[3, 2]), Some(p(0, 2)));
    }

    #[test]
    fn join_pulls_rows_below_the_joined_span_up() {
        let e = Edit::JoinLines {
            row: 0,
            count: 1,
            with_space: true,
        };
        assert_eq!(head_in(3, 1, &e, &[3, 2, 4, 4]), Some(p(2, 1)));
    }

    #[test]
    fn multi_row_join_accumulates_each_rows_offset() {
        // "ab"(2) "cd"(2) "ef"(2), J J -> "ab cd ef".
        // row2 col0 -> after "ab"+sp+"cd"+sp = 6.
        let e = Edit::JoinLines {
            row: 0,
            count: 2,
            with_space: true,
        };
        assert_eq!(head_in(2, 0, &e, &[2, 2, 2]), Some(p(0, 6)));
    }

    // ── Block insert / delete (visual-block I / A / d) ───────────────────────

    #[test]
    fn block_insert_pushes_columns_at_or_after_the_block_right() {
        let e = Edit::InsertBlock {
            at: p(0, 2),
            chunks: vec!["xx".into(), "xx".into()],
        };
        assert_eq!(head(1, 4, &e), Some(p(1, 6)));
    }

    #[test]
    fn block_insert_leaves_columns_before_the_block_alone() {
        let e = Edit::InsertBlock {
            at: p(0, 2),
            chunks: vec!["xx".into(), "xx".into()],
        };
        assert_eq!(head(1, 1, &e), Some(p(1, 1)));
    }

    #[test]
    fn block_insert_leaves_rows_outside_the_block_alone() {
        let e = Edit::InsertBlock {
            at: p(0, 2),
            chunks: vec!["xx".into()],
        };
        assert_eq!(head(5, 4, &e), Some(p(5, 4)));
    }

    #[test]
    fn block_chunk_delete_pulls_columns_after_the_chunk_left() {
        let e = Edit::DeleteBlockChunks {
            at: p(0, 2),
            widths: vec![2, 2],
        };
        assert_eq!(head(1, 6, &e), Some(p(1, 4)));
    }

    #[test]
    fn block_chunk_delete_collapses_columns_inside_the_chunk() {
        let e = Edit::DeleteBlockChunks {
            at: p(0, 2),
            widths: vec![3],
        };
        assert_eq!(head(0, 3, &e), Some(p(0, 2)));
    }

    // ── The one edit still dropped ───────────────────────────────────────────

    #[test]
    fn split_lines_drops_rather_than_guessing() {
        // `SplitLines` is the undo-inverse of a join — emitted when history
        // rewinds, not when a user edits. Undo restores a snapshot and does not
        // preserve secondary carets anyway, so there is nothing real to model.
        let e = Edit::SplitLines {
            row: 0,
            cols: vec![3],
            inserted_space: true,
        };
        assert_eq!(shift_position(p(5, 0), &e, |_| 0, 0), None);
    }
}
