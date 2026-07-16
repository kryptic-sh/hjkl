//! e2e: VisualBlock commands driven through the real binary under a pty
//! (audit-r2). Asserts against the on-disk buffer after `:w`. Expected
//! outputs were captured from `nvim --headless` against the same keystrokes.

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

/// Spawn hjkl on `content`, run `keys`, write, and return the resulting
/// on-disk buffer.
fn run_block(content: &str, keys: &str, expect: &str) -> String {
    let (_keep, path) = seed(content);
    let mut s = TerminalSession::spawn_with_file(&path);
    s.keys(keys);
    s.keys(":w<Enter>");
    wait_for_contents(&path, expect)
}

#[test]
fn block_append_pads_rows_shorter_than_the_top_row_to_the_block_edge() {
    // Fix 1: block `A`'s append column used to be clamped by the TOP row's
    // length alone, so on rows LONGER than the top row the typed text
    // landed inside the block instead of past its right edge. vim `v_b_A`
    // pads every row shorter than the block's right edge to reach it, then
    // appends there (`:h v_b_A`).
    let want = "ab    X\nabcdefX\n";
    let got = run_block("ab\nabcdef\n", "j$<C-v>kAX<Esc>", want);
    assert_eq!(got, want);
}
