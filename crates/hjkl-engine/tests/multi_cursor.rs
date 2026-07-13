//! Secondary cursors survive real edits through the engine's edit funnel (#63).
//!
//! `selection_shift`'s unit tests pin the geometry in isolation. These pin the
//! *wiring*: that `mutate_edit` — the single funnel every mutation goes through
//! — actually rewrites the secondary cursors, so a discipline that adds a caret
//! gets one that tracks the text rather than a stale coordinate.

use hjkl_buffer::{Buffer, Edit, MotionKind, Position};
use hjkl_engine::{DefaultHost, Editor, Options};

fn editor(content: &str) -> Editor<Buffer, DefaultHost> {
    let mut e = Editor::new(Buffer::new(), DefaultHost::new(), Options::default());
    e.set_content(content);
    e
}

fn pos(row: usize, col: usize) -> Position {
    Position::new(row, col)
}

#[test]
fn secondary_cursor_shifts_when_text_is_inserted_before_it() {
    let mut e = editor("abcdef\n");
    e.add_cursor(pos(0, 4));
    e.mutate_edit(Edit::InsertStr {
        at: pos(0, 1),
        text: "XY".to_string(),
    });
    assert_eq!(e.extra_cursors(), [pos(0, 6)], "caret must follow its text");
}

#[test]
fn secondary_cursor_does_not_move_for_an_edit_after_it() {
    let mut e = editor("abcdef\n");
    e.add_cursor(pos(0, 1));
    e.mutate_edit(Edit::InsertStr {
        at: pos(0, 4),
        text: "XY".to_string(),
    });
    assert_eq!(e.extra_cursors(), [pos(0, 1)]);
}

#[test]
fn secondary_cursor_rides_a_multiline_insert_down() {
    let mut e = editor("aaa\nbbb\nccc\n");
    e.add_cursor(pos(2, 1));
    e.mutate_edit(Edit::InsertStr {
        at: pos(0, 0),
        text: "new\n".to_string(),
    });
    assert_eq!(
        e.extra_cursors(),
        [pos(3, 1)],
        "row must shift by the inserted line"
    );
}

#[test]
fn secondary_cursor_inside_a_deleted_range_collapses_to_its_start() {
    let mut e = editor("abcdef\n");
    e.add_cursor(pos(0, 3));
    e.mutate_edit(Edit::DeleteRange {
        start: pos(0, 1),
        end: pos(0, 5),
        kind: MotionKind::Char,
    });
    assert_eq!(e.extra_cursors(), [pos(0, 1)]);
}

#[test]
fn secondary_cursor_on_a_linewise_deleted_row_collapses() {
    let mut e = editor("aaa\nbbb\nccc\nddd\n");
    e.add_cursor(pos(1, 2));
    e.mutate_edit(Edit::DeleteRange {
        start: pos(1, 0),
        end: pos(2, 0),
        kind: MotionKind::Line,
    });
    assert_eq!(e.extra_cursors(), [pos(1, 0)]);
}

#[test]
fn an_untrackable_edit_drops_secondary_cursors_rather_than_leaving_them_stale() {
    // JoinLines restructures rows in a way `selection_shift` does not model yet.
    // Dropping degrades to single-cursor; keeping a guessed position would let a
    // later edit apply somewhere the user never asked for.
    let mut e = editor("aaa\nbbb\nccc\n");
    e.add_cursor(pos(2, 1));
    e.mutate_edit(Edit::JoinLines {
        row: 0,
        count: 2,
        with_space: true,
    });
    assert!(
        e.extra_cursors().is_empty(),
        "an untrackable edit must drop the caret, not keep a stale one"
    );
}

#[test]
fn add_cursor_refuses_to_duplicate_the_primary() {
    let mut e = editor("abc\n");
    let (row, col) = e.cursor();
    e.add_cursor(pos(row, col));
    assert!(
        e.extra_cursors().is_empty(),
        "a caret on top of the primary would apply every edit twice at one spot"
    );
}

#[test]
fn add_cursor_refuses_to_duplicate_an_existing_secondary() {
    let mut e = editor("abcdef\n");
    e.add_cursor(pos(0, 3));
    e.add_cursor(pos(0, 3));
    assert_eq!(e.extra_cursors().len(), 1);
}

#[test]
fn several_secondaries_all_track_one_edit() {
    let mut e = editor("abcdefghij\n");
    e.add_cursor(pos(0, 2));
    e.add_cursor(pos(0, 5));
    e.add_cursor(pos(0, 8));
    e.mutate_edit(Edit::InsertStr {
        at: pos(0, 0),
        text: "Z".to_string(),
    });
    assert_eq!(e.extra_cursors(), [pos(0, 3), pos(0, 6), pos(0, 9)]);
}

#[test]
fn clear_extra_cursors_collapses_to_the_primary() {
    let mut e = editor("abcdef\n");
    e.add_cursor(pos(0, 2));
    e.clear_extra_cursors();
    assert!(e.extra_cursors().is_empty());
}

#[test]
fn a_single_cursor_editor_carries_no_secondaries() {
    // The whole vim path must stay a zero-secondary editor: the multi-cursor
    // work is additive and must not conjure carets for a discipline that never
    // asked for one.
    let mut e = editor("abc\n");
    e.mutate_edit(Edit::InsertStr {
        at: pos(0, 0),
        text: "x".to_string(),
    });
    assert!(e.extra_cursors().is_empty());
}
