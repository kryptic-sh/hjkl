//! Regression tests for the `ap` ("around paragraph") text object, pinned
//! against nvim 0.12.5 via the compat-oracle. Two rules are covered:
//!
//! * From a BLANK-LINE cursor, `Nap` walks whole blank-run + paragraph units:
//!   the last unit stops at its paragraph (it does not take the following blank
//!   run), and running out of units is a no-op rather than a whole-buffer clamp.
//! * A paragraph whose trailing blank run reaches EOF takes the WHOLE blank run
//!   in `ap`, so a following `Nap` unit over-runs and fails.
//!
//! All expected values were captured from real nvim; these unit-level tests
//! re-assert them without needing nvim installed.

use hjkl_engine::{Editor, Input, Key};

fn editor_with(content: &str) -> Editor {
    let opts = hjkl_engine::Options::default();
    let mut e = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::DefaultHost::new(),
        opts,
    );
    e.set_content(content);
    e
}

fn inp(key: Key) -> Input {
    Input {
        key,
        ctrl: false,
        alt: false,
        shift: false,
    }
}

fn dispatch_keys(e: &mut Editor, keys: &str) {
    for c in keys.chars() {
        hjkl_vim::dispatch_input(e, inp(Key::Char(c)));
    }
}

/// The file text of the buffer: `content()` always appends exactly one
/// trailing `\n`, so stripping a single trailing `\n` recovers the true
/// buffer content. An emptied buffer is `""` (NOT `"\n"`).
fn buffer_text(e: &Editor) -> String {
    let mut c = e.content();
    if c.ends_with('\n') {
        c.pop();
    }
    c
}

fn unnamed(e: &Editor) -> String {
    e.with_registers(|r| r.unnamed.text.clone())
}

// ── `ap` from a blank-line cursor ─────────────────────────────────────────

#[test]
fn d2ap_blank_line_between_paragraphs() {
    // Cursor on the blank line between two paragraphs. `d2ap` takes
    // blank+bbb then blank+ccc — everything after "aaa." goes.
    let mut e = editor_with("aaa.\n\nbbb.\n\nccc.\n");
    dispatch_keys(&mut e, "jd2ap");
    assert_eq!(buffer_text(&e), "aaa.\n");
    assert_eq!(e.cursor(), (0, 0));
    assert_eq!(unnamed(&e), "\nbbb.\n\nccc.\n");
}

#[test]
fn d3ap_blank_line_between_paragraphs_over_run_noop() {
    // Only two `ap` units exist from that blank line, so `d3ap` over-runs and
    // fails: buffer, cursor and register are all unchanged.
    let mut e = editor_with("aaa.\n\nbbb.\n\nccc.\n");
    dispatch_keys(&mut e, "jd3ap");
    assert_eq!(buffer_text(&e), "aaa.\n\nbbb.\n\nccc.\n");
    assert_eq!(e.cursor(), (1, 0));
    assert_eq!(unnamed(&e), "");
}

#[test]
fn dap_blank_line_before_last_paragraph() {
    // Single `ap` from a blank line: blank run + following paragraph, NOT the
    // blank run after it.
    let mut e = editor_with("aaa.\n\nbbb.\n\nccc.\n");
    dispatch_keys(&mut e, "jjjdap");
    assert_eq!(buffer_text(&e), "aaa.\n\nbbb.\n");
    assert_eq!(e.cursor(), (2, 0));
    assert_eq!(unnamed(&e), "\nccc.\n");
}

#[test]
fn dap_blank_run_at_eof_noop() {
    // A blank run that reaches EOF has no following paragraph, so `dap` is a
    // no-op.
    let mut e = editor_with("a\n\n");
    dispatch_keys(&mut e, "jdap");
    assert_eq!(buffer_text(&e), "a\n\n");
    assert_eq!(e.cursor(), (1, 0));
    assert_eq!(unnamed(&e), "");
}

// ── `ap` where the trailing blank run reaches EOF ────────────────────────

#[test]
fn dap_trailing_blank_run_to_eof() {
    // `ap` on "a" takes the WHOLE trailing blank run (both blank lines), so the
    // buffer empties.
    let mut e = editor_with("a\n\n\n");
    dispatch_keys(&mut e, "dap");
    assert_eq!(buffer_text(&e), "");
    assert_eq!(e.cursor(), (0, 0));
    assert_eq!(unnamed(&e), "a\n\n\n");
}

#[test]
fn d2ap_trailing_blank_run_to_eof_over_run_noop() {
    // The first unit consumed everything to EOF, so the second unit fails.
    let mut e = editor_with("a\n\n\n");
    dispatch_keys(&mut e, "d2ap");
    assert_eq!(buffer_text(&e), "a\n\n\n");
    assert_eq!(e.cursor(), (0, 0));
    assert_eq!(unnamed(&e), "");
}
