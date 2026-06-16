//! Side-by-side diff alignment for diff mode (#208, Phase 2).
//!
//! Computes a line-by-line alignment between two buffers using the [`similar`]
//! text-diff crate, plus character-level ranges within changed lines. The host
//! renderer turns this into vim-style diff highlighting:
//!
//! - [`DiffRowKind::Equal`] — unchanged line, present on both sides.
//! - [`DiffRowKind::Change`] — line present on both sides but different
//!   (rendered with a `DiffChange` band + `DiffText` char highlight).
//! - [`DiffRowKind::Insert`] — line only on the `b` side; the `a` side renders a
//!   filler row to keep both windows aligned.
//! - [`DiffRowKind::Delete`] — line only on the `a` side; the `b` side renders a
//!   filler row.
//!
//! The alignment is pure and dependency-light so it can be unit-tested and
//! cached by the host (keyed on the two buffers' dirty generations).

use std::ops::Range;

use similar::{ChangeTag, DiffOp, TextDiff};

/// Classification of one aligned display row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRowKind {
    /// Identical on both sides.
    Equal,
    /// Present on both sides but textually different.
    Change,
    /// Present only on the `b` side (the `a` side gets a filler row).
    Insert,
    /// Present only on the `a` side (the `b` side gets a filler row).
    Delete,
}

/// One row of the aligned side-by-side grid.
///
/// `a` / `b` are 0-based line indices into the respective buffers, or `None`
/// when that side renders a filler row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlignedRow {
    /// Line index in buffer `a`, or `None` for a filler on the `a` side.
    pub a: Option<usize>,
    /// Line index in buffer `b`, or `None` for a filler on the `b` side.
    pub b: Option<usize>,
    /// What kind of change this row represents.
    pub kind: DiffRowKind,
}

/// The full line-level alignment of two buffers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineDiff {
    /// Aligned rows, top to bottom.
    pub rows: Vec<AlignedRow>,
}

impl LineDiff {
    /// `true` when the two inputs are identical (no change rows).
    pub fn is_empty_diff(&self) -> bool {
        self.rows
            .iter()
            .all(|r| matches!(r.kind, DiffRowKind::Equal))
    }
}

/// Compute the line-level alignment between `a` and `b`.
///
/// Lines are split on `\n` (a trailing newline does not create a phantom empty
/// line, matching how the editor counts lines). A `Replace` op pairs as many
/// lines as it can into `Change` rows and emits the leftover as
/// `Delete`/`Insert` filler — the same shape vim uses.
pub fn align_lines(a: &str, b: &str) -> LineDiff {
    let diff = TextDiff::from_lines(a, b);
    let mut rows = Vec::new();
    for op in diff.ops() {
        match *op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for i in 0..len {
                    rows.push(AlignedRow {
                        a: Some(old_index + i),
                        b: Some(new_index + i),
                        kind: DiffRowKind::Equal,
                    });
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for i in 0..old_len {
                    rows.push(AlignedRow {
                        a: Some(old_index + i),
                        b: None,
                        kind: DiffRowKind::Delete,
                    });
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for i in 0..new_len {
                    rows.push(AlignedRow {
                        a: None,
                        b: Some(new_index + i),
                        kind: DiffRowKind::Insert,
                    });
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                let paired = old_len.min(new_len);
                for i in 0..paired {
                    rows.push(AlignedRow {
                        a: Some(old_index + i),
                        b: Some(new_index + i),
                        kind: DiffRowKind::Change,
                    });
                }
                // Leftover old lines → deletions (filler on b).
                for i in paired..old_len {
                    rows.push(AlignedRow {
                        a: Some(old_index + i),
                        b: None,
                        kind: DiffRowKind::Delete,
                    });
                }
                // Leftover new lines → insertions (filler on a).
                for i in paired..new_len {
                    rows.push(AlignedRow {
                        a: None,
                        b: Some(new_index + i),
                        kind: DiffRowKind::Insert,
                    });
                }
            }
        }
    }
    LineDiff { rows }
}

/// Character-level diff of a single changed line pair.
///
/// Returns `(a_ranges, b_ranges)` where each is a list of **byte** ranges into
/// the respective line that differ — `a_ranges` are deletions (highlight in the
/// `a` window), `b_ranges` are insertions (highlight in the `b` window).
/// Adjacent differing characters are coalesced into one range.
pub fn char_ranges(a_line: &str, b_line: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let diff = TextDiff::from_chars(a_line, b_line);
    let mut a_ranges: Vec<Range<usize>> = Vec::new();
    let mut b_ranges: Vec<Range<usize>> = Vec::new();
    let mut ai = 0usize;
    let mut bi = 0usize;
    for change in diff.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                ai += len;
                bi += len;
            }
            ChangeTag::Delete => {
                push_coalesced(&mut a_ranges, ai..ai + len);
                ai += len;
            }
            ChangeTag::Insert => {
                push_coalesced(&mut b_ranges, bi..bi + len);
                bi += len;
            }
        }
    }
    (a_ranges, b_ranges)
}

/// Append `r` to `ranges`, merging it into the previous range when contiguous.
fn push_coalesced(ranges: &mut Vec<Range<usize>>, r: Range<usize>) {
    if let Some(last) = ranges.last_mut()
        && last.end == r.start
    {
        last.end = r.end;
        return;
    }
    ranges.push(r);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(d: &LineDiff) -> Vec<DiffRowKind> {
        d.rows.iter().map(|r| r.kind).collect()
    }

    #[test]
    fn identical_inputs_are_all_equal() {
        let d = align_lines("a\nb\nc\n", "a\nb\nc\n");
        assert!(d.is_empty_diff());
        assert_eq!(kinds(&d), vec![DiffRowKind::Equal; 3]);
        for row in &d.rows {
            assert_eq!(row.a, row.b);
        }
    }

    #[test]
    fn single_changed_line_is_a_change_row() {
        let d = align_lines("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(
            kinds(&d),
            vec![DiffRowKind::Equal, DiffRowKind::Change, DiffRowKind::Equal]
        );
        let chg = d.rows[1];
        assert_eq!((chg.a, chg.b), (Some(1), Some(1)));
    }

    #[test]
    fn pure_insertion_fillers_a_side() {
        // b has an extra line; the a side row is a filler (a == None).
        let d = align_lines("a\nc\n", "a\nb\nc\n");
        let ins: Vec<_> = d
            .rows
            .iter()
            .filter(|r| r.kind == DiffRowKind::Insert)
            .collect();
        assert_eq!(ins.len(), 1);
        assert_eq!(ins[0].a, None);
        assert_eq!(ins[0].b, Some(1));
    }

    #[test]
    fn pure_deletion_fillers_b_side() {
        let d = align_lines("a\nb\nc\n", "a\nc\n");
        let del: Vec<_> = d
            .rows
            .iter()
            .filter(|r| r.kind == DiffRowKind::Delete)
            .collect();
        assert_eq!(del.len(), 1);
        assert_eq!(del[0].a, Some(1));
        assert_eq!(del[0].b, None);
    }

    #[test]
    fn uneven_replace_pairs_then_fillers() {
        // 1 old line replaced by 3 new → 1 Change + 2 Insert (filler on a).
        let d = align_lines("x\nold\ny\n", "x\nn1\nn2\nn3\ny\n");
        assert_eq!(
            kinds(&d),
            vec![
                DiffRowKind::Equal,
                DiffRowKind::Change,
                DiffRowKind::Insert,
                DiffRowKind::Insert,
                DiffRowKind::Equal,
            ]
        );
    }

    #[test]
    fn char_ranges_highlight_only_the_difference() {
        // "let x = 1" vs "let x = 2": only the final char differs.
        let (a, b) = char_ranges("let x = 1", "let x = 2");
        assert_eq!(a, vec![8..9]);
        assert_eq!(b, vec![8..9]);
    }

    #[test]
    fn char_ranges_coalesce_adjacent() {
        let (a, b) = char_ranges("abcd", "axyd");
        // middle two chars differ → one coalesced range on each side.
        assert_eq!(a, vec![1..3]);
        assert_eq!(b, vec![1..3]);
    }

    #[test]
    fn char_ranges_identical_is_empty() {
        let (a, b) = char_ranges("same", "same");
        assert!(a.is_empty());
        assert!(b.is_empty());
    }

    #[test]
    fn char_ranges_utf8_byte_offsets() {
        // Multi-byte char before the difference: ranges must be byte offsets.
        let (a, b) = char_ranges("café 1", "café 2");
        // "café " is 6 bytes (é = 2), the digit starts at byte 6.
        assert_eq!(a, vec![6..7]);
        assert_eq!(b, vec![6..7]);
    }
}
