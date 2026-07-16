//! Edit operations on [`crate::View`].
//!
//! Every mutation goes through [`View::apply_edit`] and returns
//! the inverse `Edit` so the host can build an undo stack without
//! snapshotting the whole buffer. Cursor follows edits the way vim
//! does: insertions land the cursor at the end of the inserted
//! text; deletions clamp the cursor to the deletion start.

use crate::buffer::{pos_to_char_idx, rope_line_char_count};
use crate::{Position, View};

/// Granularity of a delete; preserved through undo so a linewise
/// delete doesn't come back as a charwise one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionKind {
    /// Charwise — `[start, end)` byte range, possibly wrapping rows.
    Char,
    /// Linewise — whole rows from `start.row..=end.row`. Endpoint
    /// columns are ignored.
    Line,
    /// Blockwise — rectangle `[start.row..=end.row] × [min_col..=max_col]`.
    Block,
}

/// One unit of buffer mutation. Constructed by the caller (vim
/// engine, ex command, …) and handed to [`View::apply_edit`].
///
/// ## Invariants
///
/// All `Position` arguments must satisfy the bounds documented on
/// [`Position`] before the edit is applied. Out-of-bounds positions
/// are clamped by [`View::clamp_position`] inside
/// [`View::apply_edit`]; if the clamped form changes the edit's
/// meaning the result is implementation-defined.
///
/// See [`View::apply_edit`] for post-conditions that hold after
/// every variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Insert one char at `at`. Cursor lands one position past it.
    ///
    /// `at` must be a valid [`Position`]. `ch` must be a single Unicode
    /// scalar. Multi-grapheme content must use [`Edit::InsertStr`].
    InsertChar { at: Position, ch: char },
    /// Insert `text` (possibly multi-line) at `at`. Cursor lands at
    /// the end of the inserted content.
    ///
    /// `at` must be a valid [`Position`]. `text` may contain `\n` — the
    /// buffer splits on newline. CR (`\r`) is preserved as-is; the host
    /// is responsible for CRLF normalization before insert.
    InsertStr { at: Position, text: String },
    /// Delete `[start, end)` with the given kind.
    ///
    /// `start <= end` in document order. [`MotionKind`] controls whether
    /// trailing newlines are consumed:
    ///
    /// - [`MotionKind::Char`][]: byte-precise; preserves enclosing newlines.
    /// - [`MotionKind::Line`][]: whole rows from `start.row..=end.row`;
    ///   endpoint columns are ignored.
    /// - [`MotionKind::Block`][]: rectangle
    ///   `[start.row..=end.row] × [min_col..=max_col]`.
    DeleteRange {
        start: Position,
        end: Position,
        kind: MotionKind,
    },
    /// `J` (`with_space = true`) / `gJ` (`false`) — fold `count` rows
    /// after `row` into `row`.
    ///
    /// `row + count - 1` must be a valid row. `count >= 1`.
    JoinLines {
        row: usize,
        count: usize,
        with_space: bool,
    },
    /// Inverse of `JoinLines`. Splits `row` back at each char column
    /// in `cols`.
    ///
    /// `inserted_spaces[i]` records whether the join that produced
    /// `cols[i]` ACTUALLY inserted a space there — NOT the caller's
    /// `with_space` intent passed to `JoinLines`, which is uniform for
    /// the whole (possibly multi-join) batch while the per-join outcome
    /// is not: `do_join_lines` skips the space whenever either side of
    /// that specific join is empty. A single bool here (matching the
    /// original, uniform `with_space` intent) can't tell those joins
    /// apart from ones that DID insert a space, so `do_split_lines` would
    /// misidentify — and delete — an unrelated, legitimately-present
    /// space character that happens to sit at that col (audit-r2 fix 6).
    /// Parallel to `cols`.
    SplitLines {
        row: usize,
        cols: Vec<usize>,
        inserted_spaces: Vec<bool>,
    },
    /// Replace `[start, end)` with `with` (charwise, may span rows).
    ///
    /// Same constraints as [`Edit::DeleteRange`] with
    /// [`MotionKind::Char`] for the deleted range, plus the insert
    /// constraints from [`Edit::InsertStr`] for `with`.
    Replace {
        start: Position,
        end: Position,
        with: String,
    },
    /// Insert one chunk per row, each at `(at.row + i, at.col)`.
    /// Inverse of a blockwise delete; preserves the rectangle even
    /// when rows are ragged shorter than `at.col`.
    InsertBlock { at: Position, chunks: Vec<String> },
    /// Inverse of [`Edit::InsertBlock`]. Removes `widths[i]` chars
    /// starting at `(at.row + i, at.col)`, plus `pads[i]` more chars
    /// immediately BEFORE `at.col` on that row. Carrying widths instead
    /// of recomputing means a ragged-row block delete round-trips
    /// exactly.
    ///
    /// `pads` exists because `do_insert_block` space-pads a row that's
    /// shorter than `at.col` before splicing the chunk in, so that the
    /// chunk lands at the intended column; without recording that pad
    /// width here too, this inverse would remove the chunk but leave the
    /// padding behind (audit-r2 fix 6). `pads[i]` is always `0` for a row
    /// that didn't need padding. `DeleteBlockChunks` only ever appears as
    /// `InsertBlock`'s inverse (never constructed by a "forward" edit —
    /// see `do_delete_range`'s `MotionKind::Block` arm, which builds its
    /// own inverse `InsertBlock` directly via `rope_cut_chars`), so this
    /// field has no bearing on any other call site's semantics.
    DeleteBlockChunks {
        at: Position,
        widths: Vec<usize>,
        pads: Vec<usize>,
    },
}

impl View {
    /// Apply `edit` and return the inverse. Pushing the inverse back
    /// through `apply_edit` restores the previous state, making it the
    /// single hook for undo-stack integration.
    ///
    /// `apply_edit` is the **only** way to mutate buffer text.
    ///
    /// ## Post-conditions
    ///
    /// After any [`Edit`] variant:
    ///
    /// - [`View::dirty_gen`] is incremented exactly once.
    /// - The cursor is repositioned to a sensible place for the edit kind
    ///   (insert lands past the inserted content; delete lands at the
    ///   start). Callers that need to override the new cursor must call
    ///   [`View::set_cursor`] immediately after.
    /// - All [`Position`] values the caller held from before the edit may
    ///   be invalid. Re-derive from row / col deltas; do not cache.
    pub fn apply_edit(&mut self, edit: Edit) -> Edit {
        match edit {
            Edit::InsertChar { at, ch } => self.do_insert_str(at, ch.to_string()),
            Edit::InsertStr { at, text } => self.do_insert_str(at, text),
            Edit::DeleteRange { start, end, kind } => self.do_delete_range(start, end, kind),
            Edit::JoinLines {
                row,
                count,
                with_space,
            } => self.do_join_lines(row, count, with_space),
            Edit::SplitLines {
                row,
                cols,
                inserted_spaces,
            } => self.do_split_lines(row, cols, inserted_spaces),
            Edit::Replace { start, end, with } => self.do_replace(start, end, with),
            Edit::InsertBlock { at, chunks } => self.do_insert_block(at, chunks),
            Edit::DeleteBlockChunks { at, widths, pads } => {
                self.do_delete_block_chunks(at, widths, pads)
            }
        }
    }

    fn do_insert_block(&mut self, at: Position, chunks: Vec<String>) -> Edit {
        let mut widths: Vec<usize> = Vec::with_capacity(chunks.len());
        let mut pads: Vec<usize> = Vec::with_capacity(chunks.len());
        for (i, chunk) in chunks.into_iter().enumerate() {
            let row = at.row + i;
            // Pad short rows with spaces so the column position exists
            // before splicing — same semantics as the old Vec<String> impl.
            // Recorded in `pads` so the returned DeleteBlockChunks inverse
            // can remove this padding too, not just the chunk (audit-r2
            // fix 6): otherwise undoing an InsertBlock that padded a
            // ragged row leaves the padding behind.
            let mut pad = 0usize;
            {
                let mut c = self.content.lock().unwrap();
                let n = c.text.len_lines();
                if row < n {
                    let lc = rope_line_char_count(&c.text, row);
                    if lc < at.col {
                        pad = at.col - lc;
                        let insert_char_idx = pos_to_char_idx(&c.text, row, lc);
                        c.text.insert(insert_char_idx, &" ".repeat(pad));
                    }
                }
            }
            pads.push(pad);
            widths.push(chunk.chars().count());
            // Insert chunk at (row, at.col).
            {
                let mut c = self.content.lock().unwrap();
                let n = c.text.len_lines();
                if row < n {
                    let char_idx = pos_to_char_idx(&c.text, row, at.col);
                    c.text.insert(char_idx, &chunk);
                }
            }
        }
        self.dirty_gen_bump();
        self.set_cursor(at);
        Edit::DeleteBlockChunks { at, widths, pads }
    }

    fn do_delete_block_chunks(
        &mut self,
        at: Position,
        widths: Vec<usize>,
        pads: Vec<usize>,
    ) -> Edit {
        let mut chunks: Vec<String> = Vec::with_capacity(widths.len());
        for (i, w) in widths.into_iter().enumerate() {
            let pad = pads.get(i).copied().unwrap_or(0);
            let row = at.row + i;
            let removed = {
                let mut c = self.content.lock().unwrap();
                let n = c.text.len_lines();
                if row >= n {
                    String::new()
                } else {
                    let lc = rope_line_char_count(&c.text, row);
                    // Remove the pad (immediately before at.col) together
                    // with the chunk (at.col..at.col+w) in one contiguous
                    // span — do_insert_block always places them adjacently.
                    let col_start = at.col.saturating_sub(pad).min(lc);
                    let col_end = (at.col + w).min(lc);
                    if col_start >= col_end {
                        String::new()
                    } else {
                        let char_start = pos_to_char_idx(&c.text, row, col_start);
                        let char_end = pos_to_char_idx(&c.text, row, col_end);
                        let removed_span: String =
                            c.text.slice(char_start..char_end).to_string();
                        c.text.remove(char_start..char_end);
                        // Discard the pad portion from the returned chunk —
                        // it's regenerated automatically by do_insert_block
                        // if this inverse is itself later undone (redo).
                        let pad_end = at.col.min(lc);
                        let pad_len = pad_end.saturating_sub(col_start);
                        removed_span.chars().skip(pad_len).collect()
                    }
                }
            };
            chunks.push(removed);
        }
        self.dirty_gen_bump();
        self.set_cursor(at);
        Edit::InsertBlock { at, chunks }
    }

    fn do_insert_str(&mut self, at: Position, text: String) -> Edit {
        let normalised = self.clamp_position(at);
        let inserted_chars = text.chars().count();
        let inserted_lines = text.split('\n').count();
        let end = if inserted_lines > 1 {
            let last_chars = text.rsplit('\n').next().unwrap_or("").chars().count();
            Position::new(normalised.row + inserted_lines - 1, last_chars)
        } else {
            Position::new(normalised.row, normalised.col + inserted_chars)
        };
        {
            let mut c = self.content.lock().unwrap();
            let char_idx = pos_to_char_idx(&c.text, normalised.row, normalised.col);
            c.text.insert(char_idx, &text);
        }
        self.dirty_gen_bump();
        self.set_cursor(end);
        Edit::DeleteRange {
            start: normalised,
            end,
            kind: MotionKind::Char,
        }
    }

    fn do_delete_range(&mut self, start: Position, end: Position, kind: MotionKind) -> Edit {
        let (start, end) = order(start, end);
        match kind {
            MotionKind::Char => {
                let removed = {
                    let mut c = self.content.lock().unwrap();
                    rope_cut_chars(&mut c.text, start, end)
                };
                self.dirty_gen_bump();
                self.set_cursor(start);
                Edit::InsertStr {
                    at: start,
                    text: removed,
                }
            }
            MotionKind::Line => {
                let (removed_text, new_cursor, lo) = {
                    let mut c = self.content.lock().unwrap();
                    let n = c.text.len_lines();
                    // Clamp BOTH endpoints. An unclamped `lo` past the last
                    // row underflows the `hi - lo + 1` capacity below and
                    // panics `line_to_char(lo)`.
                    let lo = start.row.min(n.saturating_sub(1));
                    let hi = end.row.min(n.saturating_sub(1));

                    // Collect the removed rows as a joined string (needed for inverse).
                    let mut removed_lines: Vec<String> = Vec::with_capacity(hi - lo + 1);
                    for r in lo..=hi {
                        removed_lines.push(rope_line_str_locked(&c.text, r));
                    }

                    // Compute char range to remove.
                    // When hi is not the last row, we take [line_to_char(lo), line_to_char(hi+1)).
                    // When hi IS the last row and lo>0, we also remove the '\n' that ends
                    // row lo-1 so we don't leave a trailing newline orphan.
                    // When removing everything (lo==0, hi==last), take [0, len_chars()).
                    let (remove_start, remove_end) = if hi + 1 < n {
                        // Normal case: rows lo..=hi followed by more rows.
                        // char range = [line_to_char(lo), line_to_char(hi+1))
                        (c.text.line_to_char(lo), c.text.line_to_char(hi + 1))
                    } else if lo > 0 {
                        // hi is the last row AND there are rows before lo.
                        // Remove the '\n' that ended row lo-1 as well.
                        (c.text.line_to_char(lo) - 1, c.text.len_chars())
                    } else {
                        // Removing everything (lo==0, hi==last).
                        (0, c.text.len_chars())
                    };

                    c.text.remove(remove_start..remove_end);
                    // ropey guarantees len_lines() >= 1 (empty rope = 1 line).

                    let n2 = c.text.len_lines();
                    let target_row = lo.min(n2.saturating_sub(1));
                    let removed_joined = {
                        let mut s = removed_lines.join("\n");
                        // Add trailing '\n' so the inverse InsertStr re-inserts
                        // correctly (pushes surviving rows down).
                        s.push('\n');
                        s
                    };
                    (removed_joined, Position::new(target_row, 0), lo)
                };
                self.dirty_gen_bump();
                self.set_cursor(new_cursor);
                Edit::InsertStr {
                    at: Position::new(lo, 0),
                    text: removed_text,
                }
            }
            MotionKind::Block => {
                let (left, right) = (start.col.min(end.col), start.col.max(end.col));
                let mut chunks: Vec<String> = Vec::with_capacity(end.row - start.row + 1);
                for row in start.row..=end.row {
                    let removed = {
                        let mut c = self.content.lock().unwrap();
                        let n = c.text.len_lines();
                        if row >= n {
                            String::new()
                        } else {
                            let row_start_pos = Position::new(row, left);
                            let row_end_pos = Position::new(row, right + 1);
                            rope_cut_chars(&mut c.text, row_start_pos, row_end_pos)
                        }
                    };
                    chunks.push(removed);
                }
                self.dirty_gen_bump();
                self.set_cursor(Position::new(start.row, left));
                Edit::InsertBlock {
                    at: Position::new(start.row, left),
                    chunks,
                }
            }
        }
    }

    fn do_join_lines(&mut self, row: usize, count: usize, with_space: bool) -> Edit {
        let count = count.max(1);
        let (actual_row, split_cols, inserted_spaces) = {
            let mut c = self.content.lock().unwrap();
            let n = c.text.len_lines();
            let row = row.min(n.saturating_sub(1));
            let mut split_cols: Vec<usize> = Vec::with_capacity(count);
            // Per-join outcome (did THIS join actually insert a space),
            // NOT the uniform `with_space` intent — see the field doc on
            // `Edit::SplitLines::inserted_spaces` (audit-r2 fix 6).
            let mut inserted_spaces: Vec<bool> = Vec::with_capacity(count);

            for _ in 0..count {
                let n2 = c.text.len_lines();
                if row + 1 >= n2 {
                    break;
                }
                // Current length of row (in chars, sans '\n').
                let join_col = rope_line_char_count(&c.text, row);
                split_cols.push(join_col);

                // The '\n' that ends row is at char index line_to_char(row) + join_col.
                let newline_char = c.text.line_to_char(row) + join_col;
                // Remove the '\n'.
                c.text.remove(newline_char..newline_char + 1);

                // Now row and (what was row+1) are merged. Insert space if needed.
                let mut this_inserted_space = false;
                if with_space {
                    // After removing '\n', the join_col chars of original row are
                    // followed immediately by the next row's content.
                    // Insert space only if both sides are non-empty.
                    let merged_len = rope_line_char_count(&c.text, row);
                    let prefix_empty = join_col == 0;
                    let suffix_empty = join_col >= merged_len;
                    if !prefix_empty && !suffix_empty {
                        // Insert space at newline_char (now the join point).
                        c.text.insert_char(newline_char, ' ');
                        this_inserted_space = true;
                        // Adjust future split_cols: the space shifts subsequent
                        // join points by 1, but split_cols[i] is the char count
                        // of the original row *before* this join, which doesn't
                        // need adjustment — the SplitLines inverse uses it to
                        // split the joined line at the right position.
                    }
                }
                inserted_spaces.push(this_inserted_space);
            }
            (row, split_cols, inserted_spaces)
        };
        self.dirty_gen_bump();
        self.set_cursor(Position::new(actual_row, 0));
        Edit::SplitLines {
            row: actual_row,
            cols: split_cols,
            inserted_spaces,
        }
    }

    fn do_split_lines(&mut self, row: usize, cols: Vec<usize>, inserted_spaces: Vec<bool>) -> Edit {
        let actual_row = {
            let mut c = self.content.lock().unwrap();
            let n = c.text.len_lines();
            let row = row.min(n.saturating_sub(1));

            // Split right-to-left so each col still indexes into the
            // original char positions on the surviving prefix.
            for (idx, &col) in cols.iter().enumerate().rev() {
                let mut split_col = col;
                // Per-col: did the ORIGINAL join at this position actually
                // insert a space? (Not a uniform flag — see the
                // `Edit::SplitLines` field doc, audit-r2 fix 6.)
                if inserted_spaces.get(idx).copied().unwrap_or(false) {
                    // The original join inserted a space at `col`, so the
                    // current content has a space at position `col` which
                    // we need to remove before inserting the '\n'.
                    let lc = rope_line_char_count(&c.text, row);
                    if split_col < lc {
                        let space_char_idx = c.text.line_to_char(row) + split_col;
                        // Check if char at split_col is a space.
                        let ch = c.text.char(space_char_idx);
                        if ch == ' ' {
                            c.text.remove(space_char_idx..space_char_idx + 1);
                        }
                    }
                    // split_col stays the same — the '\n' goes at the same
                    // position (we removed the space, so col is still correct).
                } else {
                    let lc = rope_line_char_count(&c.text, row);
                    split_col = split_col.min(lc);
                }

                // Insert '\n' at (row, split_col).
                let char_idx = c.text.line_to_char(row) + split_col;
                c.text.insert_char(char_idx, '\n');
            }

            row
        };
        self.dirty_gen_bump();
        self.set_cursor(Position::new(actual_row, 0));
        Edit::JoinLines {
            row: actual_row,
            count: cols.len(),
            // Reconstructing a single with_space intent for redo: true iff
            // ANY col in this batch actually inserted a space. When none
            // did, redoing with with_space=false reproduces the identical
            // result anyway (do_join_lines would skip every space here
            // too), so this is a safe, behavior-preserving collapse.
            with_space: inserted_spaces.iter().any(|&b| b),
        }
    }

    fn do_replace(&mut self, start: Position, end: Position, with: String) -> Edit {
        let (start, end) = order(start, end);
        let removed = {
            let mut c = self.content.lock().unwrap();
            rope_cut_chars(&mut c.text, start, end)
        };
        let normalised = self.clamp_position(start);
        let inserted_chars = with.chars().count();
        let inserted_lines = with.split('\n').count();
        let new_end = if inserted_lines > 1 {
            let last_chars = with.rsplit('\n').next().unwrap_or("").chars().count();
            Position::new(normalised.row + inserted_lines - 1, last_chars)
        } else {
            Position::new(normalised.row, normalised.col + inserted_chars)
        };
        {
            let mut c = self.content.lock().unwrap();
            let char_idx = pos_to_char_idx(&c.text, normalised.row, normalised.col);
            c.text.insert(char_idx, &with);
        }
        self.dirty_gen_bump();
        self.set_cursor(new_end);
        Edit::Replace {
            start: normalised,
            end: new_end,
            with: removed,
        }
    }
}

// ── Internals — char surgery (free functions over &mut ropey::Rope) ──

/// Get logical line `row` as a `String`, stripping trailing `\n`.
/// Identical to `rope_line_str` but takes a lock guard's rope by ref
/// (avoids re-importing the pub(crate) helper from buffer.rs inside this module).
fn rope_line_str_locked(rope: &ropey::Rope, row: usize) -> String {
    let slice = rope.line(row);
    let s = slice.to_string();
    if s.ends_with('\n') {
        s[..s.len() - 1].to_string()
    } else {
        s
    }
}

/// Remove `[start, end)` (charwise) from the rope and return the
/// removed text as a `String` (with `\n` between rows).
///
/// `start` and `end` carry `(row, col)` where `col` is a char index
/// within the line. The function converts them to absolute char indices,
/// removes the range, and returns the removed text.
fn rope_cut_chars(rope: &mut ropey::Rope, start: Position, end: Position) -> String {
    let (start, end) = order(start, end);
    let n = rope.len_lines();

    // Clamp to rope bounds.
    let start_row = start.row.min(n.saturating_sub(1));
    let start_col = {
        let lc = crate::buffer::rope_line_char_count(rope, start_row);
        start.col.min(lc)
    };
    let end_row = end.row.min(n.saturating_sub(1));
    let end_col = {
        let lc = crate::buffer::rope_line_char_count(rope, end_row);
        end.col.min(lc)
    };

    let char_start = rope.line_to_char(start_row) + start_col;
    let char_end = rope.line_to_char(end_row) + end_col;

    if char_start >= char_end {
        return String::new();
    }

    let removed: String = rope.slice(char_start..char_end).to_string();
    rope.remove(char_start..char_end);
    removed
}

fn order(a: Position, b: Position) -> (Position, Position) {
    if a <= b { (a, b) } else { (b, a) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::rope_line_str;

    fn round_trip_check(initial: &str, edit: Edit) {
        let mut b = View::from_str(initial);
        let snapshot_before = b.as_string();
        let inverse = b.apply_edit(edit);
        b.apply_edit(inverse);
        assert_eq!(b.as_string(), snapshot_before);
    }

    #[test]
    fn insert_char_round_trip() {
        round_trip_check(
            "abc",
            Edit::InsertChar {
                at: Position::new(0, 1),
                ch: 'X',
            },
        );
    }

    #[test]
    fn insert_str_multiline_round_trip() {
        round_trip_check(
            "abc\ndef",
            Edit::InsertStr {
                at: Position::new(0, 2),
                text: "X\nY\nZ".into(),
            },
        );
    }

    #[test]
    fn delete_charwise_single_row_round_trip() {
        round_trip_check(
            "alpha bravo charlie",
            Edit::DeleteRange {
                start: Position::new(0, 6),
                end: Position::new(0, 11),
                kind: MotionKind::Char,
            },
        );
    }

    #[test]
    fn delete_charwise_multi_row_round_trip() {
        round_trip_check(
            "row0\nrow1\nrow2",
            Edit::DeleteRange {
                start: Position::new(0, 2),
                end: Position::new(2, 2),
                kind: MotionKind::Char,
            },
        );
    }

    #[test]
    fn delete_linewise_round_trip() {
        round_trip_check(
            "a\nb\nc\nd",
            Edit::DeleteRange {
                start: Position::new(1, 0),
                end: Position::new(2, 0),
                kind: MotionKind::Line,
            },
        );
    }

    #[test]
    fn delete_blockwise_round_trip() {
        round_trip_check(
            "abcdef\nghijkl\nmnopqr",
            Edit::DeleteRange {
                start: Position::new(0, 1),
                end: Position::new(2, 3),
                kind: MotionKind::Block,
            },
        );
    }

    #[test]
    fn join_lines_with_space_round_trip() {
        round_trip_check(
            "first\nsecond\nthird",
            Edit::JoinLines {
                row: 0,
                count: 2,
                with_space: true,
            },
        );
    }

    #[test]
    fn join_lines_no_space_round_trip() {
        round_trip_check(
            "first\nsecond",
            Edit::JoinLines {
                row: 0,
                count: 1,
                with_space: false,
            },
        );
    }

    #[test]
    fn replace_round_trip() {
        round_trip_check(
            "foo bar baz",
            Edit::Replace {
                start: Position::new(0, 4),
                end: Position::new(0, 7),
                with: "QUUX".into(),
            },
        );
    }

    // ── Block-op / split-lines round trips (audit-r2 fix 6) ──────────────────
    //
    // These inverses are dead today — nothing currently chains
    // apply(edit) -> apply(inverse) for InsertBlock/DeleteBlockChunks or a
    // JoinLines/SplitLines pair with mixed per-join outcomes — but the
    // contract (`apply_edit` returns an inverse that restores the pre-edit
    // text exactly) must hold the day something does.

    #[test]
    fn insert_block_round_trip_uniform_rows() {
        round_trip_check(
            "ab\ncd\nef",
            Edit::InsertBlock {
                at: Position::new(0, 1),
                chunks: vec!["X".into(), "Y".into(), "Z".into()],
            },
        );
    }

    /// `do_insert_block` space-pads a row shorter than `at.col` before
    /// splicing the chunk in. Round-tripping must remove that padding too,
    /// not just the chunk — pre-fix, `DeleteBlockChunks`'s inverse only
    /// carried the chunk width, leaving the padding behind.
    #[test]
    fn insert_block_round_trip_pads_short_row() {
        // Row 1 ("x") is only 1 char; at.col=3 needs 2 chars of padding
        // before the "Q" chunk lands.
        round_trip_check(
            "abcd\nx\nefgh",
            Edit::InsertBlock {
                at: Position::new(0, 3),
                chunks: vec!["P".into(), "Q".into(), "R".into()],
            },
        );
    }

    /// Same as above but EVERY row needs padding, and by different amounts.
    #[test]
    fn insert_block_round_trip_ragged_pads_vary_per_row() {
        round_trip_check(
            "\na\nab\nabc",
            Edit::InsertBlock {
                at: Position::new(0, 3),
                chunks: vec!["W".into(), "X".into(), "Y".into(), "Z".into()],
            },
        );
    }

    #[test]
    fn delete_block_chunks_round_trip() {
        // Constructed directly (DeleteBlockChunks only ever appears in
        // practice as InsertBlock's returned inverse — see the variant's
        // doc comment) to round-trip the OTHER direction: does re-inserting
        // (InsertBlock) restore what DeleteBlockChunks removed?
        round_trip_check(
            "abcdef\nghijkl",
            Edit::DeleteBlockChunks {
                at: Position::new(0, 1),
                widths: vec![2, 2],
                pads: vec![0, 0],
            },
        );
    }

    /// Regression for the exact scenario fix 6 describes: a join with an
    /// EMPTY prefix (row 0 is blank) skips inserting a space, but the
    /// pulled-up row legitimately STARTS with its own, unrelated space.
    /// Pre-fix, `SplitLines`'s single uniform `inserted_space` flag
    /// couldn't tell "this join skipped the space" from "this join
    /// inserted one", so splitting back mistook the legitimate leading
    /// space for the (never-inserted) join space and ate it.
    #[test]
    fn join_then_split_empty_prefix_preserves_legitimate_leading_space() {
        round_trip_check(
            "\n bar",
            Edit::JoinLines {
                row: 0,
                count: 1,
                with_space: true,
            },
        );
    }

    /// Same failure mode from the empty-SUFFIX side: the row being joined
    /// INTO legitimately ends with a space of its own, and the incoming
    /// (pulled-up) row is empty, so the join skips inserting one.
    #[test]
    fn join_then_split_empty_suffix_preserves_legitimate_trailing_space() {
        round_trip_check(
            "foo \n",
            Edit::JoinLines {
                row: 0,
                count: 1,
                with_space: true,
            },
        );
    }

    /// count > 1 with an empty middle line mixes a skipped-space join and a
    /// real one in the SAME batch — the scenario `content_edit_shape_tests`
    /// (hjkl-engine) exercises for byte-exactness; here we check the
    /// simpler round-trip-restores-original-text property instead.
    #[test]
    fn join_then_split_multi_count_mixed_spaces_round_trip() {
        round_trip_check(
            "foo\n\nbar",
            Edit::JoinLines {
                row: 0,
                count: 2,
                with_space: true,
            },
        );
    }

    /// Regression: a linewise delete whose START row lies past the last
    /// buffer row used to underflow `hi - lo + 1` (capacity math) and panic
    /// `line_to_char(lo)`. Both endpoints must clamp to the last row.
    #[test]
    fn delete_linewise_start_past_end_is_clamped() {
        let mut b = View::from_str("a\nb\nc");
        b.apply_edit(Edit::DeleteRange {
            start: Position::new(10, 0),
            end: Position::new(20, 0),
            kind: MotionKind::Line,
        });
        // Clamps to the last row and removes it.
        assert_eq!(b.as_string(), "a\nb");
    }

    #[test]
    fn delete_clearing_buffer_keeps_one_empty_row() {
        let mut b = View::from_str("only");
        b.apply_edit(Edit::DeleteRange {
            start: Position::new(0, 0),
            end: Position::new(0, 0),
            kind: MotionKind::Line,
        });
        assert_eq!(b.row_count(), 1);
        assert_eq!(rope_line_str(&b.rope(), 0), "");
    }

    #[test]
    fn insert_char_lands_cursor_after() {
        let mut b = View::from_str("abc");
        b.set_cursor(Position::new(0, 1));
        b.apply_edit(Edit::InsertChar {
            at: Position::new(0, 1),
            ch: 'X',
        });
        assert_eq!(b.cursor(), Position::new(0, 2));
        assert_eq!(rope_line_str(&b.rope(), 0), "aXbc");
    }

    #[test]
    fn block_delete_on_ragged_rows_handles_short_lines() {
        // Row 1 is shorter than the block right edge — only the
        // chars that exist get removed.
        let mut b = View::from_str("longline\nhi\nthird row");
        let inv = b.apply_edit(Edit::DeleteRange {
            start: Position::new(0, 2),
            end: Position::new(2, 5),
            kind: MotionKind::Block,
        });
        b.apply_edit(inv);
        assert_eq!(b.as_string(), "longline\nhi\nthird row");
    }

    #[test]
    fn dirty_gen_bumps_per_edit() {
        let mut b = View::from_str("abc");
        let g0 = b.dirty_gen();
        b.apply_edit(Edit::InsertChar {
            at: Position::new(0, 0),
            ch: 'X',
        });
        assert_eq!(b.dirty_gen(), g0 + 1);
        b.apply_edit(Edit::DeleteRange {
            start: Position::new(0, 0),
            end: Position::new(0, 1),
            kind: MotionKind::Char,
        });
        assert_eq!(b.dirty_gen(), g0 + 2);
    }

    /// Regression: a 60 k-row multi-line `InsertStr` into a 60 k-row buffer
    /// used to call `Vec::insert(insert_at + i, …)` per row → O(N²) memmove.
    /// With ropey, InsertStr is O(log N + edit_size) — this test confirms it
    /// stays comfortably under the 200 ms budget.
    #[test]
    fn splice_at_60k_paste_at_row_zero_is_under_200ms() {
        // View with 60 k rows of empty content.
        let initial = "\n".repeat(60_000);
        let mut b = View::from_str(&initial);
        // Multi-line payload: 60 k "x" lines glued by \n.
        let payload = vec!["x"; 60_000].join("\n");
        let t = std::time::Instant::now();
        b.apply_edit(Edit::InsertStr {
            at: Position::new(0, 0),
            text: payload,
        });
        let elapsed = t.elapsed();
        assert!(
            elapsed.as_millis() < 200,
            "60k-row InsertStr took {elapsed:?}; budget 200 ms"
        );
    }
}
