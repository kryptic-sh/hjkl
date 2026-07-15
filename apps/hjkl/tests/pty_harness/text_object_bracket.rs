//! E2e regression for audit finding A1: `retreat_one` (in
//! `crates/hjkl-vim/src/vim/sneak.rs`) converts a text object's exclusive end
//! into the Visual-mode INCLUSIVE cursor position, and used to compute the
//! previous line's BYTE length (`buf_line_bytes`) where a CHAR column was
//! needed — wrong on any multi-byte previous line.
//!
//! These pin the verified-correct behavior for the shape the audit flagged:
//!
//! ```text
//! fn foo() {
//!     body
//! }
//! ```
//!
//! `vi{d` with the cursor in `body` collapses the buffer to `"fn foo() {\n}\n"`
//! — the closing brace's line is pulled up onto the open-brace line, NOT left
//! as an empty line in between. This was checked directly against
//! `nvim --headless` (`normal! jw`, `normal vi{d`) and matches
//! `hjkl-compat-oracle`'s `vi_brace_open_trailing_charwise` case, which is
//! the same shape with content preceding the newline. An earlier, incorrect
//! version of the fix (landing on the last REAL char instead of the "one
//! past end" virtual column) produced `"fn foo() {\n\n}\n"` instead — plausible
//! at a glance, but wrong: it broke `cargo test -p hjkl-compat-oracle`
//! (57 -> 56) and diverged from live nvim.
//!
//! The multi-byte case doesn't distinguish old vs. new code at THIS call
//! site — `View::set_cursor` clamps `col.min(line_chars)`, and byte-length
//! is always >= char-length for UTF-8, so the old byte-length value got
//! silently clamped down to the correct char-length anyway. It's kept here
//! as a defensive regression guard (in case that incidental clamping ever
//! changes) and because the unit tests in `sneak.rs` exercise `retreat_one`
//! directly, without the clamp, where the byte/char distinction DOES show up
//! in the raw return value.

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

/// `vi{d` with the cursor inside `body` and `}` alone on its own line must
/// collapse to `"fn foo() {\n}\n"` — matching real vim/nvim, which pulls the
/// closing brace's line up rather than leaving an empty line behind.
#[test]
fn vi_brace_delete_joins_closing_brace_line() {
    let (_keep, path) = seed("fn foo() {\n    body\n}\n");
    let mut s = TerminalSession::spawn_with_file(&path);
    // Land the cursor on 'b' of "body": down one line, then first word.
    s.keys("jw");
    s.keys("vi{d");
    s.keys(":w<Enter>");
    let got = wait_for_contents(&path, "fn foo() {\n}\n");
    assert_eq!(
        got, "fn foo() {\n}\n",
        "vi{{d should collapse to \"fn foo() {{\\n}}\\n\" (matches nvim), not \
         leave an empty inner line and not corrupt the buffer"
    );
}

/// Multibyte variant: the inner line contains a 2-byte UTF-8 char (`é`), so
/// its byte length and char length diverge. Same expected collapse as the
/// ascii case — this also guards against the selection landing mid-codepoint
/// or leaving stray bytes behind.
#[test]
fn vi_brace_delete_multibyte_joins_closing_brace_line() {
    let (_keep, path) = seed("fn foo() {\n    héllo\n}\n");
    let mut s = TerminalSession::spawn_with_file(&path);
    s.keys("jw");
    s.keys("vi{d");
    s.keys(":w<Enter>");
    let got = wait_for_contents(&path, "fn foo() {\n}\n");
    assert_eq!(
        got, "fn foo() {\n}\n",
        "vi{{d should collapse to \"fn foo() {{\\n}}\\n\" (matches nvim) even \
         with a multibyte char on the inner line"
    );
}
