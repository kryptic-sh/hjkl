//! Manual folds (`zf`) survive real edits through the engine's edit funnel
//! (audit-r2 fix 1).
//!
//! Before this fix, `mutate_edit` shifted marks / jumplist / changelist by
//! the edit's row delta but left folds alone unless the edit's cursor band
//! directly overlapped them — a fold below or above an edit kept stale row
//! numbers forever. These tests pin the wiring end-to-end (through
//! `mutate_edit`, not just the `hjkl-buffer` unit-level `shift_fold` helper).
//!
//! Each test sets the cursor to the edit's row before calling
//! `mutate_edit`, mirroring what the real vim FSM does (the operator's
//! motion parks the cursor on the affected row before the edit funnel
//! runs) — `mutate_edit` reads the *current* cursor row as the edit's
//! start row, independent of the `Edit`'s own position fields.

use hjkl_buffer::{Edit, MotionKind, Position, View};
use hjkl_engine::{DefaultHost, Editor, Options};

fn editor(content: &str) -> Editor<View, DefaultHost> {
    let mut e = Editor::new(View::new(), DefaultHost::new(), Options::default());
    e.set_content(content);
    e
}

fn pos(row: usize, col: usize) -> Position {
    Position::new(row, col)
}

fn ten_lines() -> Editor<View, DefaultHost> {
    editor("0\n1\n2\n3\n4\n5\n6\n7\n8\n9\n")
}

#[test]
fn insert_above_shifts_fold_down() {
    // Repro from the audit: 10-line file, fold at rows 4..6, `ggO x<Esc>`
    // (insert one line above row 0) — vim shifts the fold to 5..7.
    let mut e = ten_lines();
    e.buffer_mut().add_fold(4, 6, true);
    e.buffer_mut().set_cursor(pos(0, 0));
    e.mutate_edit(Edit::InsertStr {
        at: pos(0, 0),
        text: "x\n".to_string(),
    });
    let folds = e.buffer().folds();
    assert_eq!(folds.len(), 1);
    assert_eq!((folds[0].start_row, folds[0].end_row), (5, 7));
}

#[test]
fn delete_above_shifts_fold_up() {
    let mut e = ten_lines();
    e.buffer_mut().add_fold(4, 6, true);
    e.buffer_mut().set_cursor(pos(0, 0));
    // Delete row 0 only (`MotionKind::Line` is row-inclusive on both ends).
    e.mutate_edit(Edit::DeleteRange {
        start: pos(0, 0),
        end: pos(0, 0),
        kind: MotionKind::Line,
    });
    let folds = e.buffer().folds();
    assert_eq!(folds.len(), 1);
    assert_eq!((folds[0].start_row, folds[0].end_row), (3, 5));
}

#[test]
fn delete_overlapping_the_fold_drops_it() {
    let mut e = ten_lines();
    e.buffer_mut().add_fold(4, 6, true);
    e.buffer_mut().set_cursor(pos(4, 0));
    // Delete rows 4..=6 — fully consumes the fold.
    e.mutate_edit(Edit::DeleteRange {
        start: pos(4, 0),
        end: pos(6, 0),
        kind: MotionKind::Line,
    });
    assert!(e.buffer().folds().is_empty());
}

#[test]
fn delete_overlapping_fold_tail_clips_it() {
    let mut e = ten_lines();
    e.buffer_mut().add_fold(4, 6, true);
    e.buffer_mut().set_cursor(pos(6, 0));
    // Delete rows 6..=8 — the fold's tail; it should clip to end at the
    // last surviving row (5) instead of dropping or keeping a stale end.
    e.mutate_edit(Edit::DeleteRange {
        start: pos(6, 0),
        end: pos(8, 0),
        kind: MotionKind::Line,
    });
    let folds = e.buffer().folds();
    assert_eq!(folds.len(), 1);
    assert_eq!((folds[0].start_row, folds[0].end_row), (4, 5));
}

#[test]
fn edit_inside_a_fold_grows_its_end() {
    // Cursor sits INSIDE the fold's range and opens a new line there —
    // vim's fold adjusts (grows) its end instead of vanishing.
    let mut e = ten_lines();
    e.buffer_mut().add_fold(4, 6, true);
    e.buffer_mut().set_cursor(pos(5, 0));
    e.mutate_edit(Edit::InsertStr {
        at: pos(5, 0),
        text: "x\n".to_string(),
    });
    let folds = e.buffer().folds();
    assert_eq!(
        folds.len(),
        1,
        "edit inside a fold must adjust it, not drop it"
    );
    assert_eq!((folds[0].start_row, folds[0].end_row), (4, 7));
}

#[test]
fn edit_inside_a_fold_shrinks_its_end_on_delete() {
    let mut e = ten_lines();
    e.buffer_mut().add_fold(4, 8, true);
    e.buffer_mut().set_cursor(pos(5, 0));
    // Delete rows 5..=6 (strictly inside the fold's body).
    e.mutate_edit(Edit::DeleteRange {
        start: pos(5, 0),
        end: pos(6, 0),
        kind: MotionKind::Line,
    });
    let folds = e.buffer().folds();
    assert_eq!(folds.len(), 1);
    assert_eq!((folds[0].start_row, folds[0].end_row), (4, 6));
}

#[test]
fn fold_well_below_the_edit_is_untouched() {
    let mut e = ten_lines();
    e.buffer_mut().add_fold(1, 2, true);
    e.buffer_mut().set_cursor(pos(8, 0));
    e.mutate_edit(Edit::InsertStr {
        at: pos(8, 0),
        text: "x\n".to_string(),
    });
    let folds = e.buffer().folds();
    assert_eq!(folds.len(), 1);
    assert_eq!((folds[0].start_row, folds[0].end_row), (1, 2));
}
