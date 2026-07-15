//! Secondary cursors survive real edits through the engine's edit funnel (#63).
//!
//! `selection_shift`'s unit tests pin the geometry in isolation. These pin the
//! *wiring*: that `mutate_edit` — the single funnel every mutation goes through
//! — actually rewrites the secondary cursors, so a discipline that adds a caret
//! gets one that tracks the text rather than a stale coordinate.

use hjkl_buffer::{Edit, MotionKind, Position, View};
use hjkl_engine::{DefaultHost, Editor, Options, Sel};

fn editor(content: &str) -> Editor<View, DefaultHost> {
    let mut e = Editor::new(View::new(), DefaultHost::new(), Options::default());
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
fn char_at(e: &Editor<View, DefaultHost>, p: Position) -> Option<char> {
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

// ── Applying an edit at every cursor (#63 Phase B) ───────────────────────────

#[test]
fn insert_at_all_cursors_lands_text_at_every_caret() {
    let mut e = editor("aaa\nbbb\nccc\n");
    // primary at (0,0); secondaries on the other two rows.
    e.add_cursor(pos(1, 0));
    e.add_cursor(pos(2, 0));
    e.edit_at_all_cursors(|at| Edit::InsertStr {
        at,
        text: "X".to_string(),
    });
    assert_eq!(e.line(0).as_deref(), Some("Xaaa"));
    assert_eq!(e.line(1).as_deref(), Some("Xbbb"));
    assert_eq!(e.line(2).as_deref(), Some("Xccc"));
}

#[test]
fn insert_at_several_carets_on_one_row_does_not_smear_them() {
    // Three carets on the SAME row is the case a naive top-down loop gets wrong:
    // the first insert shifts the other two, and they end up in the wrong columns.
    let mut e = editor("abcdef\n");
    e.add_cursor(pos(0, 2));
    e.add_cursor(pos(0, 4));
    // primary is at (0,0).
    e.edit_at_all_cursors(|at| Edit::InsertStr {
        at,
        text: "-".to_string(),
    });
    assert_eq!(e.line(0).as_deref(), Some("-ab-cd-ef"));
}

#[test]
fn every_caret_ends_up_after_its_own_inserted_text() {
    let mut e = editor("abcdef\n");
    e.add_cursor(pos(0, 2));
    e.add_cursor(pos(0, 4));
    e.edit_at_all_cursors(|at| Edit::InsertStr {
        at,
        text: "-".to_string(),
    });
    // "-ab-cd-ef": carets land just past each dash -> cols 1, 4, 7.
    let (pr, pc) = e.cursor();
    let mut all: Vec<Position> = vec![pos(pr, pc)];
    all.extend_from_slice(&e.extra_cursors());
    all.sort_by_key(|p| (p.row, p.col));
    assert_eq!(all, [pos(0, 1), pos(0, 4), pos(0, 7)]);
}

#[test]
fn the_primary_stays_the_primary_after_a_fan_out() {
    // The primary must come back out of the parked set, not get swapped for a
    // secondary — a discipline reads `cursor()` and would otherwise jump.
    let mut e = editor("abcdef\n");
    e.add_cursor(pos(0, 4));
    e.edit_at_all_cursors(|at| Edit::InsertStr {
        at,
        text: "Z".to_string(),
    });
    // primary was (0,0) -> lands at (0,1). The secondary was (0,4) -> (0,6).
    assert_eq!(e.cursor(), (0, 1));
    assert_eq!(e.extra_cursors(), [pos(0, 6)]);
}

#[test]
fn delete_at_all_cursors_removes_a_char_under_each() {
    let mut e = editor("axbxcx\n");
    // Delete the char at each caret: primary (0,1), plus (0,3) and (0,5).
    e.set_cursor_quiet(0, 1);
    e.add_cursor(pos(0, 3));
    e.add_cursor(pos(0, 5));
    e.edit_at_all_cursors(|at| Edit::DeleteRange {
        start: at,
        end: Position::new(at.row, at.col + 1),
        kind: MotionKind::Char,
    });
    assert_eq!(e.line(0).as_deref(), Some("abc"));
}

#[test]
fn fan_out_returns_one_inverse_per_caret() {
    let mut e = editor("abc\n");
    e.add_cursor(pos(0, 2));
    let inverses = e.edit_at_all_cursors(|at| Edit::InsertStr {
        at,
        text: "Q".to_string(),
    });
    assert_eq!(
        inverses.len(),
        2,
        "caller needs every inverse to undo as one step"
    );
}

#[test]
fn a_single_cursor_fan_out_is_just_an_ordinary_edit() {
    let mut e = editor("abc\n");
    e.edit_at_all_cursors(|at| Edit::InsertStr {
        at,
        text: "X".to_string(),
    });
    assert_eq!(e.line(0).as_deref(), Some("Xabc"));
    assert!(e.extra_cursors().is_empty());
    assert_eq!(e.cursor(), (0, 1));
}

#[test]
fn a_fan_out_that_loses_a_caret_collapses_to_the_primary() {
    // SplitLines is untrackable, so the parked carets cannot survive it. Rather
    // than keep carets that no longer know where they are, collapse.
    let mut e = editor("aaa bbb\nccc\n");
    e.add_cursor(pos(1, 0));
    e.edit_at_all_cursors(|_| Edit::SplitLines {
        row: 0,
        cols: vec![3],
        inserted_space: true,
    });
    assert!(e.extra_cursors().is_empty());
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

// ── Ranged secondary selections ──────────────────────────────────────────────
//
// A secondary is a *selection*, not a bare caret: it carries an anchor, and an
// operator fanned out with `edit_at_all_selections` acts on the whole range at
// every one of them. Without this, helix's `d` deletes a range at the primary
// and a single char everywhere else.

#[test]
fn a_secondary_selection_deletes_its_whole_range() {
    let mut e = editor("abcdef\nghijkl\n");
    // Primary selects "ab" on row 0; secondary selects "gh" on row 1.
    e.set_cursor_quiet(0, 1);
    e.add_selection(Sel::new(pos(1, 0), pos(1, 1)));
    let (_, _) = e.edit_at_all_selections(pos(0, 0), |s| Edit::DeleteRange {
        start: s.start(),
        end: Position::new(s.end().row, s.end().col + 1),
        kind: MotionKind::Char,
    });
    assert_eq!(e.line(0).as_deref(), Some("cdef"));
    assert_eq!(e.line(1).as_deref(), Some("ijkl"));
}

#[test]
fn deleting_a_range_leaves_a_caret_at_each_edit_site() {
    let mut e = editor("abcdef\nghijkl\n");
    e.set_cursor_quiet(0, 3);
    e.add_selection(Sel::new(pos(1, 2), pos(1, 3)));
    let (_, anchor) = e.edit_at_all_selections(pos(0, 2), |s| Edit::DeleteRange {
        start: s.start(),
        end: Position::new(s.end().row, s.end().col + 1),
        kind: MotionKind::Char,
    });
    assert_eq!(e.cursor(), (0, 2));
    assert_eq!(
        anchor,
        pos(0, 2),
        "the primary anchor collapses onto the head"
    );
    assert_eq!(e.extra_selections(), [Sel::caret(pos(1, 2))]);
}

#[test]
fn two_selections_on_one_row_do_not_smear_when_both_are_deleted() {
    // The case a top-down fan-out gets wrong: deleting the first range moves the
    // second one's coordinates out from under it.
    let mut e = editor("aaBBccDDee\n");
    e.set_cursor_quiet(0, 3); // primary selects "BB" (cols 2..=3)
    e.add_selection(Sel::new(pos(0, 6), pos(0, 7))); // secondary selects "DD"
    e.edit_at_all_selections(pos(0, 2), |s| Edit::DeleteRange {
        start: s.start(),
        end: Position::new(s.end().row, s.end().col + 1),
        kind: MotionKind::Char,
    });
    assert_eq!(e.line(0).as_deref(), Some("aaccee"));
}

#[test]
fn a_secondary_selection_shifts_both_ends_across_an_unrelated_edit() {
    let mut e = editor("abcdef\n");
    e.add_selection(Sel::new(pos(0, 2), pos(0, 4)));
    e.mutate_edit(Edit::InsertStr {
        at: pos(0, 0),
        text: "XY".to_string(),
    });
    assert_eq!(
        e.extra_selections(),
        [Sel::new(pos(0, 4), pos(0, 6))],
        "anchor and head must move together or the next edit spans the wrong text"
    );
}

#[test]
fn an_untrackable_edit_drops_the_whole_selection_not_half_of_it() {
    let mut e = editor("abc\ndef\n");
    e.add_selection(Sel::new(pos(1, 0), pos(1, 2)));
    e.mutate_edit(Edit::SplitLines {
        row: 0,
        cols: vec![1],
        inserted_space: false,
    });
    assert!(e.extra_selections().is_empty());
}

#[test]
fn history_rewind_drops_the_secondaries() {
    // Undo restores a snapshot; nothing tracked the carets across it.
    let mut e = editor("abc\ndef\n");
    e.add_cursor(pos(1, 0));
    e.push_undo();
    e.mutate_edit(Edit::InsertStr {
        at: pos(0, 0),
        text: "X".to_string(),
    });
    e.undo();
    assert!(e.extra_selections().is_empty());
}

#[test]
fn set_extra_selections_refuses_a_duplicate_head() {
    let mut e = editor("abcdef\n"); // primary head at (0, 0)
    e.set_extra_selections(vec![
        Sel::caret(pos(0, 2)),
        Sel::new(pos(0, 1), pos(0, 2)), // same head as the previous entry
        Sel::caret(pos(0, 0)),          // the primary's head
    ]);
    assert_eq!(e.extra_selections(), [Sel::caret(pos(0, 2))]);
}
