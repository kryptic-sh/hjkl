//! e2e: terminal **bracketed paste** into Insert mode preserves newlines.
//!
//! Regression for the Ctrl+Shift+V bug: in raw mode crossterm maps a pasted
//! `\n` (0x0A) to Ctrl+J, which the Insert-mode dispatcher dropped — so pasted
//! lines bunched onto one line. The fix enables bracketed paste so the terminal
//! delivers the whole blob as one `Event::Paste`, inserted verbatim.
//!
//! These assert against the *on-disk buffer* after `:w` (the ground truth for
//! exact line structure) rather than scraping the rendered screen.

use super::harness::TerminalSession;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Create a writable temp file with the given suffix, seeded with `content`.
/// The returned `NamedTempFile` must be kept alive for the path to stay valid.
fn seed(suffix: &str, content: &str) -> (tempfile::NamedTempFile, PathBuf) {
    let mut f = tempfile::Builder::new()
        .suffix(suffix)
        .tempfile()
        .expect("create temp file");
    f.write_all(content.as_bytes()).expect("seed temp file");
    f.flush().expect("flush temp file");
    let path = f.path().to_owned();
    (f, path)
}

/// Poll the file until it equals `want` (or time out), returning the last read.
/// `:w` lands asynchronously relative to the pty write, so retry briefly.
fn wait_for_contents(path: &Path, want: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut last = String::new();
    while Instant::now() < deadline {
        last = std::fs::read_to_string(path).unwrap_or_default();
        if last == want {
            return last;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    last
}

/// Paste a multi-line `.toml` blob into an empty buffer in Insert mode and
/// confirm every line survives on its own row (no bunching).
#[test]
fn bracketed_paste_toml_preserves_newlines() {
    // A representative .toml with blank lines and section headers — exactly the
    // shape that "bunched" into one line before the fix.
    let toml = "title = \"config\"\n\
                count = 42\n\
                \n\
                [server]\n\
                host = \"localhost\"\n\
                port = 8080\n\
                \n\
                [server.tls]\n\
                enabled = true\n";

    let (_keep, path) = seed(".toml", "");
    let mut s = TerminalSession::spawn_with_file(&path);

    // Enter Insert mode at the top, paste the blob, leave Insert, write.
    s.keys("i");
    s.paste(toml);
    s.keys("<Esc>");
    s.keys(":w<Enter>");

    let got = wait_for_contents(&path, toml);
    assert_eq!(
        got, toml,
        "pasted .toml should round-trip with newlines intact (got:\n{got:?})"
    );
    // Belt-and-suspenders: the bug collapsed everything onto line 1, so assert
    // the section header genuinely sits on its own line.
    assert!(
        got.lines().any(|l| l == "[server]"),
        "[server] must be its own line, not bunched (got:\n{got:?})"
    );
    assert!(
        got.lines().count() >= 9,
        "expected >= 9 lines, got {} (bunched?):\n{got:?}",
        got.lines().count()
    );
}

/// CRLF line endings (Windows clipboard) also split into separate lines.
#[test]
fn bracketed_paste_crlf_normalised() {
    let crlf = "[a]\r\nx = 1\r\n[b]\r\ny = 2\r\n";
    let want = "[a]\nx = 1\n[b]\ny = 2\n";

    let (_keep, path) = seed(".toml", "");
    let mut s = TerminalSession::spawn_with_file(&path);

    s.keys("i");
    s.paste(crlf);
    s.keys("<Esc>");
    s.keys(":w<Enter>");

    let got = wait_for_contents(&path, want);
    assert_eq!(got, want, "CRLF paste should normalise to LF-split lines");
}
