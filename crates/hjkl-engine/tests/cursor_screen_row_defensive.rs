//! Regression coverage for audit finding A11: `Editor::cursor_screen_row`
//! computed `height as usize - 1`, which underflows when `height == 0`
//! (panics in debug builds, wraps to `usize::MAX` in release). There are
//! no shipped callers that currently pass `height == 0`, but the method
//! is `pub` and defensive against a `0`-height textarea (e.g. a window
//! resized to nothing), so it must not panic or misbehave in that case.

use hjkl_engine::{DefaultHost, Editor, Options};

fn editor(content: &str) -> Editor<hjkl_buffer::View, DefaultHost> {
    let mut e = Editor::new(
        hjkl_buffer::View::new(),
        DefaultHost::new(),
        Options::default(),
    );
    e.set_content(content);
    e
}

#[test]
fn cursor_screen_row_does_not_underflow_at_zero_height() {
    let mut e = editor("a\nb\nc\n");
    // Must not panic (debug-mode underflow) and must clamp to row 0.
    assert_eq!(e.cursor_screen_row(0), 0);
}

#[test]
fn cursor_screen_row_clamps_to_last_visible_row() {
    let mut e = editor("a\nb\nc\nd\ne\n");
    // height 1 means only row 0 of the textarea is visible.
    assert_eq!(e.cursor_screen_row(1), 0);
}
