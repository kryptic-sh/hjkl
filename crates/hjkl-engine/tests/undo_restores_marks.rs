//! Undo/redo restore mark-ish state alongside the text (audit-r2 fix 2).
//!
//! Before this fix, `restore_text` (the undo/redo funnel) rewrote the
//! buffer's text wholesale but never touched marks, the jumplist, or the
//! per-buffer changelist ring — so a row-shift an edit applied to them
//! survived an undo of that very edit.
//!
//! Repro from the audit: `ma` at row 10, `ggO x<Esc>` (shifts the mark to
//! row 11), `u` — vim restores `'a` to row 10; before this fix it stayed at
//! row 11.

use hjkl_buffer::{Edit, Position, View};
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
fn mark_shifts_on_edit_then_unshifts_on_undo_then_reshifts_on_redo() {
    let mut e = twelve_lines();
    e.buffer_mut().set_cursor(pos(10, 0));
    e.set_mark('a', (10, 0));

    // Snapshot BEFORE the shifting edit — this is what `u` should return to.
    e.push_undo();

    e.buffer_mut().set_cursor(pos(0, 0));
    e.mutate_edit(Edit::InsertStr {
        at: pos(0, 0),
        text: "x\n".to_string(),
    });
    assert_eq!(
        e.mark('a'),
        Some((11, 0)),
        "sanity: the edit itself shifts the mark down"
    );

    e.undo();
    assert_eq!(
        e.mark('a'),
        Some((10, 0)),
        "undo must restore the mark to its pre-edit row"
    );

    e.redo();
    assert_eq!(
        e.mark('a'),
        Some((11, 0)),
        "redo must re-apply the edit's shift to the mark"
    );
}

#[test]
fn jumplist_shifts_on_edit_then_unshifts_on_undo_then_reshifts_on_redo() {
    let mut e = twelve_lines();
    e.buffer_mut().set_cursor(pos(10, 0));
    e.record_jump((10, 0));

    e.push_undo();

    e.buffer_mut().set_cursor(pos(0, 0));
    e.mutate_edit(Edit::InsertStr {
        at: pos(0, 0),
        text: "x\n".to_string(),
    });
    assert_eq!(e.jump_back_list().to_vec(), vec![(11, 0)]);

    e.undo();
    assert_eq!(
        e.jump_back_list().to_vec(),
        vec![(10, 0)],
        "undo must restore the jumplist entry to its pre-edit row"
    );

    e.redo();
    assert_eq!(
        e.jump_back_list().to_vec(),
        vec![(11, 0)],
        "redo must re-apply the edit's shift to the jumplist entry"
    );
}

#[test]
fn changelist_state_survives_undo_redo() {
    let mut e = twelve_lines();

    // Edit A: establishes a changelist entry + dot mark.
    e.buffer_mut().set_cursor(pos(2, 0));
    e.mutate_edit(Edit::InsertStr {
        at: pos(2, 0),
        text: "Z".to_string(),
    });
    let list_after_a = e.change_list();
    let last_edit_after_a = e.last_edit_pos();

    // Snapshot AFTER edit A — this is what undoing edit B should return to.
    e.push_undo();

    // Edit B: appends another changelist entry.
    e.buffer_mut().set_cursor(pos(5, 0));
    e.mutate_edit(Edit::InsertStr {
        at: pos(5, 0),
        text: "Y".to_string(),
    });
    let list_after_b = e.change_list();
    assert_ne!(
        list_after_b, list_after_a,
        "sanity: edit B must actually change the changelist ring"
    );

    e.undo();
    assert_eq!(
        e.change_list(),
        list_after_a,
        "undo must restore the changelist ring to its pre-edit-B state"
    );
    assert_eq!(
        e.last_edit_pos(),
        last_edit_after_a,
        "undo must restore the dot mark to its pre-edit-B state"
    );

    e.redo();
    assert_eq!(
        e.change_list(),
        list_after_b,
        "redo must re-apply edit B's changelist entry"
    );
}

#[test]
fn undo_restore_leaves_other_buffers_global_marks_untouched() {
    let mut e = twelve_lines();
    e.set_current_buffer_id(1);
    e.set_global_mark('A', 1, (3, 0)); // belongs to the current buffer (1)
    e.set_global_mark('B', 2, (5, 0)); // belongs to a DIFFERENT buffer (2)

    // Snapshot: only buffer 1's global marks are captured.
    e.push_undo();

    // Simulate further edits moving both global marks (buffer 1's directly,
    // and buffer 2's as if another window mutated it concurrently).
    e.set_global_mark('A', 1, (99, 0));
    e.set_global_mark('B', 2, (100, 0));

    e.undo();
    assert_eq!(
        e.global_mark('A'),
        Some((1, 3, 0)),
        "undo must restore this buffer's own global mark"
    );
    assert_eq!(
        e.global_mark('B'),
        Some((2, 100, 0)),
        "undo must NOT touch a global mark belonging to a different buffer"
    );
}
