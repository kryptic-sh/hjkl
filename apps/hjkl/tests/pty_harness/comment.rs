//! e2e: `gcc` toggles a line comment on and off, driven through the real
//! `hjkl` binary under a pty.
//!
//! Regression coverage for audit D1: `Editor::toggle_comment_range` used to
//! rebuild the whole document as a `Vec<String>` on every call. This test
//! only pins observable behavior (buffer content); the complexity fix
//! itself is guarded by a perf-shaped unit test in `hjkl-engine`.
//!
//! Uses `:set ft=rust` on a plain `.txt` seed file rather than a real `.rs`
//! file: opening a `.rs` file spins up the rust-analyzer LSP client, which
//! this sandbox doesn't have installed — an unrelated pre-existing flake
//! (`:w` on a fresh `.rs` buffer can hang past the harness's save timeout)
//! that has nothing to do with `gcc`. `:set ft=` only sets
//! `settings.filetype` (see `hjkl-ex/src/setopt.rs`), which is all
//! `toggle_comment_range` reads to resolve the comment marker.

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
fn gcc_toggles_line_comment_on_then_off() {
    let (_keep, path) = seed("let x = 1;\n");
    let mut s = TerminalSession::spawn_with_file(&path);
    s.keys(":set ft=rust<Enter>");

    // gcc comments the current line.
    s.keys("gcc");
    s.keys(":w<Enter>");
    let got = wait_for_contents(&path, "// let x = 1;\n");
    assert_eq!(got, "// let x = 1;\n", "gcc must add the comment marker");

    // gcc again removes it — back to the original line.
    s.keys("gcc");
    s.keys(":w<Enter>");
    let got = wait_for_contents(&path, "let x = 1;\n");
    assert_eq!(got, "let x = 1;\n", "gcc must remove the comment marker");
}

#[test]
fn gc_motion_comments_a_multi_line_range() {
    let (_keep, path) = seed("let a = 1;\nlet b = 2;\nlet c = 3;\n");
    let mut s = TerminalSession::spawn_with_file(&path);
    s.keys(":set ft=rust<Enter>");

    // `gcj` from row 0: comment the current line + one motion down (rows 0-1).
    // Row 2 must stay untouched.
    s.keys("gcj");
    s.keys(":w<Enter>");
    let got = wait_for_contents(&path, "// let a = 1;\n// let b = 2;\nlet c = 3;\n");
    assert_eq!(got, "// let a = 1;\n// let b = 2;\nlet c = 3;\n");
}
