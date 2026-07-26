//! Property-based tests proving the edit/undo round-trip contract.
//!
//! Every [`hjkl_buffer::Edit`] passed to [`View::apply_edit`] yields
//! an inverse `Edit`. Applying the inverse must restore the buffer's
//! content to its prior state. This file proves the property over
//! randomized text + edit shapes.

use hjkl_buffer::{Edit, MotionKind, Position, View, rope_line_str};
use proptest::prelude::*;

fn buf_lines(buf: &View) -> Vec<String> {
    let rope = buf.rope();
    let n = rope.len_lines();
    (0..n).map(|i| rope_line_str(&rope, i)).collect()
}

/// Resolve normalized fractions [0.0, 1.0] to a valid `Position` inside
/// `buf`. Keeps the strategy tree primitive-only — proptest shrinks
/// floats deterministically.
fn pos_from_fractions(buf: &View, row_frac: f64, col_frac: f64) -> Position {
    let rope = buf.rope();
    let n = rope.len_lines();
    if n == 0 {
        return Position::new(0, 0);
    }
    let row = ((row_frac.clamp(0.0, 1.0)) * (n as f64 - 1.0)).round() as usize;
    let line = rope_line_str(&rope, row);
    let line_len = line.len();
    let col = (col_frac.clamp(0.0, 1.0) * line_len as f64).round() as usize;
    Position::new(row, col.min(line_len))
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    /// Round-trip: applying any valid edit and then its inverse leaves
    /// content pointwise-equal to the prior state.
    #[test]
    fn insert_char_roundtrip(
        text in prop::collection::vec("[a-zA-Z0-9 ]{0,20}", 1..=5)
            .prop_map(|lines| lines.join("\n")),
        row_f in 0.0_f64..=1.0,
        col_f in 0.0_f64..=1.0,
        ch in prop::char::range('a', 'z'),
    ) {
        let mut buf = View::from_str(&text);
        let at = pos_from_fractions(&buf, row_f, col_f);
        let lines_before = buf_lines(&buf);
        let inv = buf.apply_edit(Edit::InsertChar { at, ch });
        buf.apply_edit(inv);
        prop_assert_eq!(lines_before, buf_lines(&buf));
    }

    #[test]
    fn insert_str_roundtrip(
        text in prop::collection::vec("[a-zA-Z0-9 ]{0,20}", 1..=5)
            .prop_map(|lines| lines.join("\n")),
        row_f in 0.0_f64..=1.0,
        col_f in 0.0_f64..=1.0,
        ins in "[a-z\n]{0,8}",
    ) {
        let mut buf = View::from_str(&text);
        let at = pos_from_fractions(&buf, row_f, col_f);
        let lines_before = buf_lines(&buf);
        let inv = buf.apply_edit(Edit::InsertStr { at, text: ins });
        buf.apply_edit(inv);
        prop_assert_eq!(lines_before, buf_lines(&buf));
    }

    #[test]
    fn delete_range_charwise_roundtrip(
        text in prop::collection::vec("[a-zA-Z0-9 ]{1,20}", 1..=5)
            .prop_map(|lines| lines.join("\n")),
        a_row_f in 0.0_f64..=1.0,
        a_col_f in 0.0_f64..=1.0,
        b_row_f in 0.0_f64..=1.0,
        b_col_f in 0.0_f64..=1.0,
    ) {
        let mut buf = View::from_str(&text);
        let a = pos_from_fractions(&buf, a_row_f, a_col_f);
        let b = pos_from_fractions(&buf, b_row_f, b_col_f);
        let (start, end) = if (a.row, a.col) <= (b.row, b.col) { (a, b) } else { (b, a) };
        let lines_before = buf_lines(&buf);
        let inv = buf.apply_edit(Edit::DeleteRange {
            start,
            end,
            kind: MotionKind::Char,
        });
        buf.apply_edit(inv);
        prop_assert_eq!(lines_before, buf_lines(&buf));
    }

    /// Round-trip: applying a linewise delete and then its inverse must
    /// restore line-by-line content — including last-row deletes with
    /// rows above them (audit B4) and whole-buffer deletes.
    #[test]
    fn delete_range_linewise_roundtrip(
        text in prop::collection::vec("[a-zA-Z0-9 ]{1,20}", 1..=5)
            .prop_map(|lines| lines.join("\n")),
        a_row_f in 0.0_f64..=1.0,
        b_row_f in 0.0_f64..=1.0,
    ) {
        let mut buf = View::from_str(&text);
        let rope = buf.rope();
        let n = rope.len_lines();
        if n == 0 {
            return Ok(());
        }
        let lo = ((a_row_f.clamp(0.0, 1.0)) * (n as f64 - 1.0)).round() as usize;
        let hi = ((b_row_f.clamp(0.0, 1.0)) * (n as f64 - 1.0)).round() as usize;
        let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
        let lines_before = buf_lines(&buf);
        let inv = buf.apply_edit(Edit::DeleteRange {
            start: Position::new(lo, 0),
            end: Position::new(hi, 0),
            kind: MotionKind::Line,
        });
        buf.apply_edit(inv);
        prop_assert_eq!(lines_before, buf_lines(&buf));
    }
}

#[test]
fn empty_buffer_insert_then_delete_roundtrip() {
    let mut buf = View::new();
    let lines_before = buf_lines(&buf);
    let inv = buf.apply_edit(Edit::InsertStr {
        at: Position::new(0, 0),
        text: "hello".to_string(),
    });
    buf.apply_edit(inv);
    assert_eq!(lines_before, buf_lines(&buf));
}

/// B4 regression: linewise delete of the last row with rows above it
/// must round-trip. Before the fix, the inverse inserted at a row that
/// no longer existed, corrupting the buffer.
#[test]
fn linewise_delete_last_row_roundtrips() {
    let mut buf = View::from_str("a\nb\nc");
    let lines_before = buf_lines(&buf);
    // Delete row 2 ("c") — last row, rows above exist.
    let inv = buf.apply_edit(Edit::DeleteRange {
        start: Position::new(2, 0),
        end: Position::new(2, 0),
        kind: MotionKind::Line,
    });
    // Buffer should now be "a\nb".
    assert_eq!(buf_lines(&buf), &["a".to_string(), "b".to_string()]);
    // Applying the inverse must restore the original.
    buf.apply_edit(inv);
    assert_eq!(buf_lines(&buf), lines_before);
}

/// Regression: a WHOLE-buffer linewise delete must round-trip for ropes
/// both with and without a trailing newline. The inverse used to append a
/// '\n' unconditionally (the register-facing linewise form), so undoing a
/// whole-buffer delete of "a\nb\nc" produced a phantom fourth row.
#[test]
fn whole_buffer_linewise_delete_roundtrips() {
    for text in ["a\nb\nc", "a\nb\nc\n"] {
        let mut buf = View::from_str(text);
        let lines_before = buf_lines(&buf);
        let last = buf.rope().len_lines() - 1;
        let inv = buf.apply_edit(Edit::DeleteRange {
            start: Position::new(0, 0),
            end: Position::new(last, 0),
            kind: MotionKind::Line,
        });
        // Buffer is cleared to the single empty row ropey always keeps.
        assert_eq!(buf_lines(&buf), &["".to_string()], "text = {text:?}");
        buf.apply_edit(inv);
        assert_eq!(buf_lines(&buf), lines_before, "text = {text:?}");
    }
}
