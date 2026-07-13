//! e2e: `[count]>>` / `[count]<<` indent operators driven through the real
//! binary under a pty, in normal AND visual modes.
//!
//! These assert against the *on-disk buffer* after `:w` rather than scraping
//! the rendered gutter — exact whitespace is what matters here and the file is
//! the ground truth. Every session pins `expandtab` + `shiftwidth=4` via
//! `:set` so indentation is deterministic spaces regardless of the default
//! config. Expected outputs were captured from `nvim --headless` with the same
//! settings (see `crates/hjkl-compat-oracle/corpus/tier2_indent_count.toml`).

use super::harness::{TerminalSession, wait_for_contents};
use std::io::Write;
use std::path::PathBuf;

/// Create a writable temp file seeded with `content`. The returned
/// `NamedTempFile` must be kept alive for the path to stay valid.
fn seed(content: &str) -> (tempfile::NamedTempFile, PathBuf) {
    let mut f = tempfile::Builder::new()
        .suffix(".txt")
        .tempfile()
        .expect("create temp file");
    f.write_all(content.as_bytes()).expect("seed temp file");
    f.flush().expect("flush temp file");
    let path = f.path().to_owned();
    (f, path)
}

/// Spawn hjkl on `content`, pin indent settings, run `keys`, write, and return
/// the resulting on-disk buffer.
fn run_indent(content: &str, keys: &str, expect: &str) -> String {
    let (_keep, path) = seed(content);
    let mut s = TerminalSession::spawn_with_file(&path);
    s.keys(":set expandtab shiftwidth=4<Enter>");
    s.keys(keys);
    s.keys(":w<Enter>");
    wait_for_contents(&path, expect)
}

#[test]
fn count_3_indent_three_lines() {
    let got = run_indent(
        "a\nb\nc\nd\ne\nf\n",
        "gg3>>",
        "    a\n    b\n    c\nd\ne\nf\n",
    );
    assert_eq!(got, "    a\n    b\n    c\nd\ne\nf\n");
}

#[test]
fn count_10_indent_clamps_to_end() {
    let want = "    a\n    b\n    c\n    d\n    e\n    f\n";
    let got = run_indent("a\nb\nc\nd\ne\nf\n", "gg10>>", want);
    assert_eq!(got, want);
}

#[test]
fn count_10_outdent_clamps_to_end() {
    let got = run_indent("    a\n    b\n    c\n", "gg10<<", "a\nb\nc\n");
    assert_eq!(got, "a\nb\nc\n");
}

#[test]
fn count_indent_last_line_is_noop() {
    // `5>>` on the final line: the implied `count_` motion can't move down, so
    // vim aborts the whole operator (E16). Buffer must be untouched.
    let got = run_indent("a\nb\nc\n", "G5>>", "a\nb\nc\n");
    assert_eq!(got, "a\nb\nc\n");
}

#[test]
fn visual_line_indent_selected_lines() {
    // Vj selects two lines, > indents them once.
    let got = run_indent("a\nb\nc\n", "ggVj>", "    a\n    b\nc\n");
    assert_eq!(got, "    a\n    b\nc\n");
}

#[test]
fn visual_line_indent_all_lines() {
    let want = "    a\n    b\n    c\n";
    let got = run_indent("a\nb\nc\n", "ggVG>", want);
    assert_eq!(got, want);
}

#[test]
fn visual_count_indent_two_levels() {
    // `Vj2>` indents the two selected lines by TWO shiftwidths.
    let got = run_indent("a\nb\nc\n", "ggVj2>", "        a\n        b\nc\n");
    assert_eq!(got, "        a\n        b\nc\n");
}

#[test]
fn visual_count_outdent_two_levels() {
    // `Vj2<` outdents the two selected lines by TWO shiftwidths.
    let got = run_indent(
        "        a\n        b\n        c\n",
        "ggVj2<",
        "a\nb\n        c\n",
    );
    assert_eq!(got, "a\nb\n        c\n");
}

// NOTE: a literal `\r` is used to submit the ex command rather than `<Enter>`.
// The pty harness's `<…>` tag scanner would otherwise merge a trailing outdent
// `<` with the following `<Enter>` (`:1,3<` + `<Enter>` → tag `<Enter`), so the
// raw carriage return keeps the command terminator unambiguous.
#[test]
fn ex_shift_right_range() {
    // `:1,2>` shifts lines 1-2 right by one shiftwidth.
    let got = run_indent("a\nb\nc\n", ":1,2>\r", "    a\n    b\nc\n");
    assert_eq!(got, "    a\n    b\nc\n");
}

#[test]
fn ex_shift_right_two_levels() {
    // `:1,2>>` shifts lines 1-2 right by two shiftwidths.
    let got = run_indent("a\nb\nc\n", ":1,2>>\r", "        a\n        b\nc\n");
    assert_eq!(got, "        a\n        b\nc\n");
}

#[test]
fn ex_shift_left_range() {
    // `:1,3<` outdents lines 1-3.
    let got = run_indent("    a\n    b\n    c\n", ":1,3<\r", "a\nb\nc\n");
    assert_eq!(got, "a\nb\nc\n");
}
