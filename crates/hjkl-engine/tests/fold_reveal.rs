//! Cursor never gets stranded on a hidden row (audit-r2 fix 3).
//!
//! (a) `undo`/`redo` restoring a cursor position that lands inside a closed
//!     fold's body must reveal it — vim's `'foldopen'` option includes
//!     `undo` as one of the events that opens folds under the cursor.
//! (b) `apply_fold_op` (the `zc`/`za`/`zM`/… dispatch) snapping the cursor
//!     off a row a fold just hid must account for NESTED folds: snapping to
//!     the innermost fold's start_row isn't enough if that row is itself
//!     hidden by an OUTER closed fold.

use hjkl_buffer::{Edit, Position, View};
use hjkl_engine::types::FoldOp;
use hjkl_engine::{DefaultHost, Editor, Options};

fn editor(content: &str) -> Editor<View, DefaultHost> {
    let mut e = Editor::new(View::new(), DefaultHost::new(), Options::default());
    e.set_content(content);
    e
}

fn pos(row: usize, col: usize) -> Position {
    Position::new(row, col)
}

fn twelve_lines() -> Editor<View, DefaultHost> {
    editor("0\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n")
}

#[test]
fn undo_restoring_cursor_inside_a_closed_fold_reveals_it() {
    let mut e = twelve_lines();
    e.buffer_mut().add_fold(3, 6, true); // closed, covers rows 3..=6

    // Cursor lands inside the fold's hidden body (row 4) and gets snapshotted
    // there.
    e.buffer_mut().set_cursor(pos(4, 0));
    e.push_undo();

    // An edit well away from the fold (delta == 0, so the fold's own rows
    // are untouched) gives undo something to restore.
    e.buffer_mut().set_cursor(pos(10, 0));
    e.mutate_edit(Edit::InsertStr {
        at: pos(10, 0),
        text: "z".to_string(),
    });
    assert!(
        e.buffer().is_row_hidden(4),
        "sanity: the fold is still closed and hiding row 4"
    );

    e.undo();
    let (row, _col) = e.cursor();
    assert_eq!(row, 4, "undo restores the snapshotted cursor row");
    assert!(
        !e.buffer().is_row_hidden(row),
        "undo must reveal the fold hiding the restored cursor row"
    );
}

#[test]
fn closing_nested_folds_snaps_past_the_inner_folds_hidden_start_row() {
    let mut e = twelve_lines();
    e.buffer_mut().add_fold(2, 10, false); // outer, initially open
    e.buffer_mut().add_fold(5, 8, false); // inner, initially open
    e.buffer_mut().set_cursor(pos(7, 0)); // inside both

    // `zM` — close every fold at once. The inner fold's start_row (5) ends
    // up hidden by the now-closed outer fold; the cursor must snap all the
    // way out to the outer fold's start_row (2), not stop at 5.
    e.apply_fold_op(FoldOp::CloseAll);

    let (row, _col) = e.cursor();
    assert_eq!(
        row, 2,
        "cursor must snap to the OUTERMOST closed fold's start_row"
    );
    assert!(!e.buffer().is_row_hidden(row));
}
