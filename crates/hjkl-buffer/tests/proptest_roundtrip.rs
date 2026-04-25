//! Property-based tests proving the edit/undo round-trip contract.
//!
//! Every [`hjkl_buffer::Edit`] passed to [`Buffer::apply_edit`] yields
//! an inverse `Edit`. Applying the inverse must restore the buffer's
//! `lines()` to its prior state. This file proves the property over
//! randomized text + edit shapes.

use hjkl_buffer::{Buffer, Edit, MotionKind, Position};
use proptest::prelude::*;

/// Resolve normalized fractions [0.0, 1.0] to a valid `Position` inside
/// `buf`. Keeps the strategy tree primitive-only — proptest shrinks
/// floats deterministically.
fn pos_from_fractions(buf: &Buffer, row_frac: f64, col_frac: f64) -> Position {
    let lines = buf.lines();
    if lines.is_empty() {
        return Position::new(0, 0);
    }
    let row = ((row_frac.clamp(0.0, 1.0)) * (lines.len() as f64 - 1.0)).round() as usize;
    let line_len = lines[row].len();
    let col = (col_frac.clamp(0.0, 1.0) * line_len as f64).round() as usize;
    Position::new(row, col.min(line_len))
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    /// Round-trip: applying any valid edit and then its inverse leaves
    /// `lines()` pointwise-equal to the prior state.
    #[test]
    fn insert_char_roundtrip(
        text in prop::collection::vec("[a-zA-Z0-9 ]{0,20}", 1..=5)
            .prop_map(|lines| lines.join("\n")),
        row_f in 0.0_f64..=1.0,
        col_f in 0.0_f64..=1.0,
        ch in prop::char::range('a', 'z'),
    ) {
        let mut buf = Buffer::from_str(&text);
        let at = pos_from_fractions(&buf, row_f, col_f);
        let lines_before: Vec<String> = buf.lines().to_vec();
        let inv = buf.apply_edit(Edit::InsertChar { at, ch });
        buf.apply_edit(inv);
        prop_assert_eq!(lines_before, buf.lines().to_vec());
    }

    #[test]
    fn insert_str_roundtrip(
        text in prop::collection::vec("[a-zA-Z0-9 ]{0,20}", 1..=5)
            .prop_map(|lines| lines.join("\n")),
        row_f in 0.0_f64..=1.0,
        col_f in 0.0_f64..=1.0,
        ins in "[a-z\n]{0,8}",
    ) {
        let mut buf = Buffer::from_str(&text);
        let at = pos_from_fractions(&buf, row_f, col_f);
        let lines_before: Vec<String> = buf.lines().to_vec();
        let inv = buf.apply_edit(Edit::InsertStr { at, text: ins });
        buf.apply_edit(inv);
        prop_assert_eq!(lines_before, buf.lines().to_vec());
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
        let mut buf = Buffer::from_str(&text);
        let a = pos_from_fractions(&buf, a_row_f, a_col_f);
        let b = pos_from_fractions(&buf, b_row_f, b_col_f);
        let (start, end) = if (a.row, a.col) <= (b.row, b.col) { (a, b) } else { (b, a) };
        let lines_before: Vec<String> = buf.lines().to_vec();
        let inv = buf.apply_edit(Edit::DeleteRange {
            start,
            end,
            kind: MotionKind::Char,
        });
        buf.apply_edit(inv);
        prop_assert_eq!(lines_before, buf.lines().to_vec());
    }
}

#[test]
fn empty_buffer_insert_then_delete_roundtrip() {
    let mut buf = Buffer::new();
    let lines_before = buf.lines().to_vec();
    let inv = buf.apply_edit(Edit::InsertStr {
        at: Position::new(0, 0),
        text: "hello".to_string(),
    });
    buf.apply_edit(inv);
    assert_eq!(lines_before, buf.lines().to_vec());
}
