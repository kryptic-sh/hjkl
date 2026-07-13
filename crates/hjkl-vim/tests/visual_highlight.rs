//! Char-visual highlight accessors that migrated from `hjkl-engine` onto
//! [`hjkl_vim::VimEditorExt`] (#267). These live here — not as inline engine
//! unit tests — because the trait is implemented on `hjkl_engine::Editor` as
//! seen through the dependency graph; an inline test inside hjkl-engine sees a
//! distinct `crate::Editor` identity to which the impl does not apply.

use hjkl_buffer::Buffer;
use hjkl_engine::Editor;
use hjkl_engine::types::{DefaultHost, Options};
use hjkl_vim::VimEditorExt;

/// Helper: create an editor in Insert mode with `content`.
fn make_ed(content: &str) -> Editor<Buffer, DefaultHost> {
    let buf = Buffer::from_str(content);
    let mut ed = hjkl_vim::vim_editor(buf, DefaultHost::default(), Options::default());
    ed.enter_insert_i(1);
    ed
}

/// Helper: create an editor with `selection_exclusive = true` in Insert mode.
fn make_ed_exclusive(content: &str) -> Editor<Buffer, DefaultHost> {
    let mut ed = make_ed(content);
    ed.settings_mut().selection_exclusive = true;
    ed
}

// ── inclusive (default, vim) ──────────────────────────────────────────────

#[test]
fn inclusive_default_single_char_right() {
    // Buffer "abc". Cursor at col 0, enter visual, move right 1 → selects 'a'.
    let mut ed = make_ed("abc");
    ed.exit_visual_to_normal();
    ed.set_cursor_doc(0, 0);
    ed.enter_visual_char();
    ed.set_cursor_doc(0, 1);
    let hl = ed.char_highlight();
    // Inclusive: (0,0)..(0,1) both included.
    assert_eq!(hl, Some(((0, 0), (0, 1))));
}

#[test]
fn inclusive_default_not_none_for_same_pos() {
    // With inclusive mode, anchor == cursor still returns Some (single char).
    let mut ed = make_ed("abc");
    ed.exit_visual_to_normal();
    ed.set_cursor_doc(0, 0);
    ed.enter_visual_char();
    let hl = ed.char_highlight();
    assert_eq!(hl, Some(((0, 0), (0, 0))));
}

// ── exclusive (VSCode) ────────────────────────────────────────────────────

#[test]
fn exclusive_single_char_right() {
    // Buffer "hello". Cursor at 0, enter visual, advance caret to col 1.
    // Exclusive range: 0..1 (only 'h' selected, caret before 'e').
    let mut ed = make_ed_exclusive("hello");
    ed.exit_visual_to_normal();
    ed.set_cursor_doc(0, 0);
    ed.enter_visual_char();
    ed.set_cursor_doc(0, 1);
    let hl = ed.char_highlight();
    assert_eq!(hl, Some(((0, 0), (0, 1))));
}

#[test]
fn exclusive_multi_char() {
    // "hello", anchor 0, caret 3 → chars 0,1,2 ("hel") selected.
    let mut ed = make_ed_exclusive("hello");
    ed.exit_visual_to_normal();
    ed.set_cursor_doc(0, 0);
    ed.enter_visual_char();
    ed.set_cursor_doc(0, 3);
    let hl = ed.char_highlight();
    assert_eq!(hl, Some(((0, 0), (0, 3))));
}

#[test]
fn exclusive_leftward_cursor_before_anchor() {
    // "hello", anchor at col 3, caret moved left to col 1.
    // start = (0,1), end = (0,3).
    let mut ed = make_ed_exclusive("hello");
    ed.exit_visual_to_normal();
    ed.set_cursor_doc(0, 3);
    ed.enter_visual_char();
    ed.set_cursor_doc(0, 1);
    let hl = ed.char_highlight();
    assert_eq!(hl, Some(((0, 1), (0, 3))));
}

#[test]
fn exclusive_multi_line() {
    // "abc\ndef", anchor (0,2), caret (1,1) → half-open multiline.
    let mut ed = make_ed_exclusive("abc\ndef");
    ed.exit_visual_to_normal();
    ed.set_cursor_doc(0, 2);
    ed.enter_visual_char();
    ed.set_cursor_doc(1, 1);
    let hl = ed.char_highlight();
    assert_eq!(hl, Some(((0, 2), (1, 1))));
}

#[test]
fn exclusive_empty_returns_none() {
    // Anchor == cursor → empty selection → None.
    let mut ed = make_ed_exclusive("hello");
    ed.exit_visual_to_normal();
    ed.set_cursor_doc(0, 2);
    ed.enter_visual_char();
    // Caret stays at anchor (no movement).
    let hl = ed.char_highlight();
    assert_eq!(hl, None, "exclusive empty selection should be None");
}

// ── visual_char_range_exclusive ───────────────────────────────────────────

#[test]
fn range_exclusive_rightward() {
    let mut ed = make_ed("hello");
    ed.exit_visual_to_normal();
    ed.set_cursor_doc(0, 0);
    ed.enter_visual_char();
    ed.set_cursor_doc(0, 2);
    let r = ed.visual_char_range_exclusive();
    assert_eq!(r, Some(((0, 0), (0, 2))));
}

#[test]
fn range_exclusive_leftward() {
    let mut ed = make_ed("hello");
    ed.exit_visual_to_normal();
    ed.set_cursor_doc(0, 4);
    ed.enter_visual_char();
    ed.set_cursor_doc(0, 1);
    let r = ed.visual_char_range_exclusive();
    assert_eq!(r, Some(((0, 1), (0, 4))));
}

#[test]
fn range_exclusive_empty_returns_none() {
    let mut ed = make_ed("hello");
    ed.exit_visual_to_normal();
    ed.set_cursor_doc(0, 2);
    ed.enter_visual_char();
    let r = ed.visual_char_range_exclusive();
    assert_eq!(r, None);
}

// ── buffer_selection (render path) ────────────────────────────────────────

#[test]
fn buffer_selection_exclusive_drops_head_cell() {
    use hjkl_buffer::{Position, Selection};
    // "hello", caret at col 5, select left to col 3 → exclusive chars [3,5).
    // The renderer paints row_span inclusively, so buffer_selection must
    // return head = col 4 (one back) so cols 3..=4 = "lo" highlight.
    let mut ed = make_ed_exclusive("hello");
    ed.exit_visual_to_normal();
    ed.set_cursor_doc(0, 5);
    ed.enter_visual_char();
    ed.set_cursor_doc(0, 3);
    match ed.buffer_selection() {
        Some(Selection::Char { anchor, head }) => {
            assert_eq!(anchor, Position::new(0, 3));
            assert_eq!(head, Position::new(0, 4), "head cell must be dropped");
        }
        other => panic!("expected exclusive Char selection, got {other:?}"),
    }
}

#[test]
fn buffer_selection_inclusive_keeps_head_cell() {
    use hjkl_buffer::{Position, Selection};
    // Vim default: head stays at the cursor cell (inclusive).
    let mut ed = make_ed("hello");
    ed.exit_visual_to_normal();
    ed.set_cursor_doc(0, 5);
    ed.enter_visual_char();
    ed.set_cursor_doc(0, 3);
    match ed.buffer_selection() {
        Some(Selection::Char { anchor, head }) => {
            assert_eq!(anchor, Position::new(0, 5));
            assert_eq!(head, Position::new(0, 3));
        }
        other => panic!("expected Char selection, got {other:?}"),
    }
}
