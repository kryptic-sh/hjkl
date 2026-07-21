//! Regression tests for V1 — the phantom trailing row on linewise deletes
//! that reach EOF. Two fixes are pinned here:
//!
//! * `ap` on the last paragraph (no trailing blank line) falls back to the
//!   LEADING blank lines, so `dap` on the last paragraph matches nvim
//!   (buffer, cursor, and register).
//! * The Visual-line delete path gains the same phantom-row cursor clamp the
//!   `dd` / operator-motion paths already have, so a linewise Visual delete
//!   through the last real row lands on the surviving content row, never the
//!   phantom trailing row.
//!
//! All expected values were captured from real nvim (v0.12) via the
//! compat-oracle; these unit-level tests re-assert them without needing nvim
//! installed.

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

// ── V1b: `dap` on the last paragraph ──────────────────────────────────────

#[test]
fn dap_last_paragraph_takes_leading_blank() {
    // "foo\n\nbar\n", cursor on "bar" (row 2). `ap` has no trailing blank
    // (bar runs to EOF), so it absorbs the leading blank line instead. Result
    // matches nvim: buffer "foo\n", cursor at the origin, register "\nbar\n".
    let mut e = editor_with("foo\n\nbar\n");
    dispatch_keys(&mut e, "jjdap");
    assert_eq!(buffer_text(&e), "foo\n");
    assert_eq!(e.cursor(), (0, 0));
    assert_eq!(unnamed(&e), "\nbar\n");
}

#[test]
fn dap_last_paragraph_absorbs_all_leading_blanks() {
    let mut e = editor_with("foo\n\n\nbar\n");
    dispatch_keys(&mut e, "3jdap");
    assert_eq!(buffer_text(&e), "foo\n");
    assert_eq!(e.cursor(), (0, 0));
    assert_eq!(unnamed(&e), "\n\nbar\n");
}

#[test]
fn dap_middle_paragraph_still_takes_trailing_blank() {
    // Guard: the EOF leading-blank fallback must NOT fire for a paragraph that
    // has a trailing blank line — it keeps taking the trailing one.
    let mut e = editor_with("foo\n\nbar\n\nbaz\n");
    dispatch_keys(&mut e, "jjdap");
    assert_eq!(buffer_text(&e), "foo\n\nbaz\n");
    assert_eq!(e.cursor(), (2, 0));
    assert_eq!(unnamed(&e), "bar\n\n");
}

#[test]
fn dip_last_paragraph_is_inner_only() {
    // `dip` stays inner: only the paragraph, never the surrounding blanks.
    let mut e = editor_with("foo\n\nbar\n");
    dispatch_keys(&mut e, "jjdip");
    assert_eq!(buffer_text(&e), "foo\n\n");
    assert_eq!(e.cursor(), (1, 0));
    assert_eq!(unnamed(&e), "bar\n");
}

#[test]
fn dap_single_paragraph_empties_to_empty_string() {
    // Emptied-buffer edge: `dap` on the only paragraph leaves "" (0 bytes),
    // NOT "\n".
    let mut e = editor_with("bar\n");
    dispatch_keys(&mut e, "dap");
    assert_eq!(buffer_text(&e), "");
    assert_eq!(e.buffer().rope().to_string(), "");
    assert_eq!(e.cursor(), (0, 0));
    assert_eq!(unnamed(&e), "bar\n");
}

// ── V1a: linewise Visual delete register + phantom-row cursor clamp ────────

#[test]
fn vgd_captures_register_without_phantom_newline() {
    // The register must be exactly the four lines with ONE trailing newline —
    // no extra `\n` from the phantom trailing row.
    let mut e = editor_with("one\ntwo\nthree\nfour\n");
    dispatch_keys(&mut e, "VGd");
    assert_eq!(unnamed(&e), "one\ntwo\nthree\nfour\n");
    assert_eq!(buffer_text(&e), "");
    assert_eq!(e.buffer().rope().to_string(), "");
    assert_eq!(e.cursor(), (0, 0));
}

#[test]
fn visual_line_delete_through_eof_clamps_off_phantom_row() {
    // Deleting rows 1..3 (through the last real line) must land the cursor on
    // the surviving "one" row (0), not the phantom trailing row.
    let mut e = editor_with("one\ntwo\nthree\nfour\n");
    dispatch_keys(&mut e, "jVjjd");
    assert_eq!(buffer_text(&e), "one\n");
    assert_eq!(e.cursor(), (0, 0));
    assert_eq!(unnamed(&e), "two\nthree\nfour\n");
}

#[test]
fn visual_line_delete_last_line_clamps_off_phantom_row() {
    let mut e = editor_with("one\ntwo\nthree\nfour\n");
    dispatch_keys(&mut e, "GVd");
    assert_eq!(buffer_text(&e), "one\ntwo\nthree\n");
    assert_eq!(e.cursor(), (2, 0));
    assert_eq!(unnamed(&e), "four\n");
}

#[test]
fn visual_line_delete_non_eof_cursor_unchanged() {
    // The common case (delete not reaching EOF) is byte-for-byte unchanged:
    // deleting rows 1..2 lands on the join row ("four" at row 1).
    let mut e = editor_with("one\ntwo\nthree\nfour\n");
    dispatch_keys(&mut e, "jVjd");
    assert_eq!(buffer_text(&e), "one\nfour\n");
    assert_eq!(e.cursor(), (1, 0));
    assert_eq!(unnamed(&e), "two\nthree\n");
}

#[test]
fn dg_empties_to_empty_string_not_newline() {
    // Emptied-buffer edge via the operator-motion path: `dG` from row 0 leaves
    // "" (0 bytes), not "\n".
    let mut e = editor_with("one\ntwo\nthree\nfour\n");
    dispatch_keys(&mut e, "dG");
    assert_eq!(e.buffer().rope().to_string(), "");
    assert_eq!(buffer_text(&e), "");
    assert_eq!(e.cursor(), (0, 0));
}
