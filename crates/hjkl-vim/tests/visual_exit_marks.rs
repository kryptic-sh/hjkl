//! Focused `'<` / `'>` pinning for visual-mode exit, one case per visual
//! flavour, exercised through **both** exit paths:
//!
//! - the FSM epilogue (`dispatch_input` → `step::end_step`), and
//! - the out-of-FSM bridge (`VimEditorExt::exit_visual_to_normal`).
//!
//! Both share `vim::visual::visual_marks_range`; these tests are the guard
//! that the two paths keep producing identical marks. The cases picked here
//! are the ones the wider suite did not already cover: charwise with a
//! *reversed* selection, linewise ending on a *shorter* row (the char-column
//! clamp), and block with reversed corners.

use hjkl_engine::{Editor, Input, Key};
use hjkl_vim::VimEditorExt;

fn editor_with(content: &str) -> Editor {
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::DefaultHost::new(),
        hjkl_engine::Options::default(),
    );
    e.set_content(content);
    e
}

/// Push one plain (no-modifier) key through the real FSM entry point.
fn key(e: &mut Editor, k: Key) {
    hjkl_vim::dispatch_input(
        e,
        Input {
            key: k,
            ctrl: false,
            alt: false,
            shift: false,
        },
    );
}

fn ctrl_key(e: &mut Editor, c: char) {
    hjkl_vim::dispatch_input(
        e,
        Input {
            key: Key::Char(c),
            ctrl: true,
            alt: false,
            shift: false,
        },
    );
}

fn keys(e: &mut Editor, s: &str) {
    for c in s.chars() {
        key(e, Key::Char(c));
    }
}

fn marks(e: &Editor) -> ((usize, usize), (usize, usize)) {
    (
        e.mark('<').expect("'< set on visual exit"),
        e.mark('>').expect("'> set on visual exit"),
    )
}

// ── charwise, reversed selection (cursor above/left of anchor) ────────────

#[test]
fn charwise_reversed_exit_marks_are_position_ordered() {
    // Anchor at (1,3), cursor walked back to (0,1). Marks are ordered by
    // position, not by selection direction.
    let mut e = editor_with("abcde\nfghij\n");
    keys(&mut e, "jlll"); // (1,3)
    keys(&mut e, "v");
    keys(&mut e, "khh"); // (0,1)
    key(&mut e, Key::Esc);
    assert_eq!(marks(&e), ((0, 1), (1, 3)));
}

#[test]
fn charwise_reversed_bridge_exit_marks_are_position_ordered() {
    let mut e = editor_with("abcde\nfghij\n");
    e.jump_cursor(1, 3);
    e.enter_visual_char();
    e.jump_cursor(0, 1);
    e.exit_visual_to_normal();
    assert_eq!(marks(&e), ((0, 1), (1, 3)));
}

// ── linewise ending on a shorter row (last-col clamp) ─────────────────────

#[test]
fn linewise_exit_marks_clamp_to_short_bottom_row() {
    // Selection starts on a long row and ends on "bb" — `'>` must land on
    // that row's own last char column (1), not the start row's.
    let mut e = editor_with("aaaaa\nbb\nccccc\n");
    keys(&mut e, "lll"); // (0,3)
    keys(&mut e, "V");
    keys(&mut e, "j"); // rows 0..=1
    key(&mut e, Key::Esc);
    assert_eq!(marks(&e), ((0, 0), (1, 1)));
}

#[test]
fn linewise_bridge_exit_marks_clamp_to_short_bottom_row() {
    let mut e = editor_with("aaaaa\nbb\nccccc\n");
    e.jump_cursor(0, 3);
    e.enter_visual_line();
    e.jump_cursor(1, 0);
    e.exit_visual_to_normal();
    assert_eq!(marks(&e), ((0, 0), (1, 1)));
}

/// Multi-byte rows: the clamp counts **chars**, not bytes. "éé" is 4 bytes
/// but 2 chars, so `'>` is col 1.
#[test]
fn linewise_exit_last_col_counts_chars_not_bytes() {
    let mut e = editor_with("aaaaa\néé\nccccc\n");
    e.jump_cursor(0, 0);
    e.enter_visual_line();
    e.jump_cursor(1, 0);
    e.exit_visual_to_normal();
    assert_eq!(marks(&e), ((0, 0), (1, 1)));
}

// ── block with reversed corners ───────────────────────────────────────────

#[test]
fn block_exit_marks_normalize_reversed_corners() {
    // anchor (0,4), cursor (1,2) — neither is a tuple-ordered bound. `'<` is
    // the top-left corner (0,2), `'>` the bottom-right (1,4).
    let mut e = editor_with("aaaaa\nbbbbb\nccccc\n");
    keys(&mut e, "llll"); // (0,4)
    ctrl_key(&mut e, 'v');
    keys(&mut e, "jhh"); // (1,2)
    key(&mut e, Key::Esc);
    assert_eq!(marks(&e), ((0, 2), (1, 4)));
}

#[test]
fn block_bridge_exit_marks_normalize_reversed_corners() {
    let mut e = editor_with("aaaaa\nbbbbb\nccccc\n");
    e.jump_cursor(0, 4);
    e.enter_visual_block();
    e.jump_cursor(1, 2);
    e.exit_visual_to_normal();
    assert_eq!(marks(&e), ((0, 2), (1, 4)));
}
