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
fn a_join_folds_a_secondary_cursor_onto_the_joined_row() {
    // "aaa" + "bbb" -> "aaa bbb": the caret at (1,1) lands after "aaa" + space.
    let mut e = editor("aaa\nbbb\nccc\n");
    e.add_cursor(pos(1, 1));
    e.mutate_edit(Edit::JoinLines {
        row: 0,
        count: 1,
        with_space: true,
    });
    assert_eq!(e.extra_cursors(), [pos(0, 5)]);
}

#[test]
fn a_join_pulls_a_secondary_cursor_below_the_join_up() {
    let mut e = editor("aaa\nbbb\nccc\n");
    e.add_cursor(pos(2, 1));
    e.mutate_edit(Edit::JoinLines {
        row: 0,
        count: 1,
        with_space: true,
    });
    assert_eq!(e.extra_cursors(), [pos(1, 1)]);
}

#[test]
fn an_untrackable_edit_drops_secondary_cursors_rather_than_leaving_them_stale() {
    // `SplitLines` is the undo-inverse of a join; nothing models its geometry
    // because undo restores a snapshot and does not preserve secondary carets
    // anyway. Dropping degrades to single-cursor; keeping a guessed position
    // would let a later edit apply somewhere the user never asked for.
    let mut e = editor("aaa bbb\nccc\n");
    e.add_cursor(pos(1, 1));
    e.mutate_edit(Edit::SplitLines {
        row: 0,
        cols: vec![3],
        inserted_space: true,
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

/// The char at `pos`, or `None` past end of row.
fn char_at(e: &Editor<Buffer, DefaultHost>, p: Position) -> Option<char> {
    e.line(p.row)?.chars().nth(p.col)
}

/// The invariant the coordinate assertions above are only a proxy for: after an
/// edit, a secondary cursor must still sit on **the same character** it did
/// before. This is what catches `selection_shift`'s geometry drifting away from
/// what `hjkl-buffer` actually does — a coordinate can be self-consistently
/// wrong, the character cannot.
#[test]
fn a_secondary_cursor_still_points_at_the_same_char_after_each_edit() {
    let cases: Vec<(&str, Position, Edit)> = vec![
        (
            "abcdef\n",
            pos(0, 4),
            Edit::InsertStr {
                at: pos(0, 1),
                text: "XY".into(),
            },
        ),
        (
            "aaa\nbbb\nccc\n",
            pos(2, 1),
            Edit::InsertStr {
                at: pos(0, 0),
                text: "new\n".into(),
            },
        ),
        (
            "abcdef\n",
            pos(0, 5),
            Edit::DeleteRange {
                start: pos(0, 1),
                end: pos(0, 3),
                kind: MotionKind::Char,
            },
        ),
        (
            "aaa\nbbb\nccc\n",
            pos(2, 1),
            Edit::DeleteRange {
                start: pos(0, 0),
                end: pos(0, 0),
                kind: MotionKind::Line,
            },
        ),
        // The ones whose geometry this slice added — mirrored from hjkl-buffer.
        (
            "aaa\nbbb\nccc\n",
            pos(1, 1),
            Edit::JoinLines {
                row: 0,
                count: 1,
                with_space: true,
            },
        ),
        (
            "aaa\nbbb\nccc\n",
            pos(2, 2),
            Edit::JoinLines {
                row: 0,
                count: 2,
                with_space: true,
            },
        ),
        (
            "abcd\nefgh\n",
            pos(1, 3),
            Edit::InsertBlock {
                at: pos(0, 1),
                chunks: vec!["XY".into(), "XY".into()],
            },
        ),
        (
            "abcd\nefgh\n",
            pos(1, 3),
            Edit::DeleteBlockChunks {
                at: pos(0, 1),
                widths: vec![2, 2],
            },
        ),
    ];

    for (content, caret, edit) in cases {
        let mut e = editor(content);
        e.add_cursor(caret);
        let before = char_at(&e, caret);
        let label = format!("{content:?} caret {caret:?} edit {edit:?}");
        e.mutate_edit(edit);
        let moved = e.extra_cursors()[0];
        assert_eq!(
            char_at(&e, moved),
            before,
            "caret drifted off its character: {label}\n  was {caret:?} -> now {moved:?}"
        );
    }
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
