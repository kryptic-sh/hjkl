//! e2e: the `!` filter operator pipes a row range through a shell command,
//! driven through the real `hjkl` binary under a pty.
//!
//! Regression coverage for audit D4: `Editor::filter_range` used to rebuild
//! the whole document as a `Vec<String>` on every call. Drives the real `!`
//! operator prompt (Visual-line select, then `!`, then type the command +
//! Enter) rather than the `:%!cmd` ex-command syntax — `:%!` is dispatched
//! by a separate handler in `hjkl-ex` (`shell::shell_filter_handler`) that
//! does not go through `Editor::filter_range` at all, so it wouldn't
//! exercise this fix.

use super::harness::{TerminalSession, wait_for_contents};
use std::io::Write;
use std::path::PathBuf;

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

#[test]
fn visual_line_filter_sorts_the_whole_buffer() {
    let (_keep, path) = seed("banana\napple\ncherry\n");
    let mut s = TerminalSession::spawn_with_file(&path);

    // ggVG selects every line; `!` opens the shell-command prompt; typing
    // `sort` + Enter pipes the selection through it and replaces it with
    // stdout.
    s.keys("ggVG!");
    s.keys("sort<Enter>");
    s.keys(":w<Enter>");

    let got = wait_for_contents(&path, "apple\nbanana\ncherry\n");
    assert_eq!(got, "apple\nbanana\ncherry\n");
}

#[test]
fn visual_line_filter_on_partial_range_leaves_other_rows_untouched() {
    let (_keep, path) = seed("alpha\nbanana\napple\n");
    let mut s = TerminalSession::spawn_with_file(&path);

    // Move to row 2 (banana), select through the last row, filter through
    // sort. Row 1 (alpha) must stay put.
    s.keys("jVG!");
    s.keys("sort<Enter>");
    s.keys(":w<Enter>");

    let got = wait_for_contents(&path, "alpha\napple\nbanana\n");
    assert_eq!(got, "alpha\napple\nbanana\n");
}
