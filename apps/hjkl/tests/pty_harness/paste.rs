//! e2e: terminal **bracketed paste** into Insert mode preserves newlines.
//!
//! Regression for the Ctrl+Shift+V bug: in raw mode crossterm maps a pasted
//! `\n` (0x0A) to Ctrl+J, which the Insert-mode dispatcher dropped — so pasted
//! lines bunched onto one line. The fix enables bracketed paste so the terminal
//! delivers the whole blob as one `Event::Paste`, inserted verbatim.
//!
//! These assert against the *on-disk buffer* after `:w` (the ground truth for
//! exact line structure) rather than scraping the rendered screen.
//!
//! Two things shape the expected bytes:
//!
//! * Pasting a `\n`-terminated blob into an **empty** buffer leaves a trailing
//!   blank line (the blob's final newline opens a new row on top of the empty
//!   buffer's one original row), and `:w` faithfully preserves it — every line
//!   gets its terminator, so the file ends `…\n\n` (audit B2). Vim does exactly
//!   the same, so that is the parity expectation here.
//! * `format_on_save` defaults to **on**, and `.toml` maps to the external
//!   `taplo` formatter. Whether a formatter binary exists is a property of the
//!   *host*, not the editor: with taplo installed the hook reformats and its
//!   output gets `strip_suffix('\n')`, silently eating that trailing blank line;
//!   without taplo the hook is skipped and the buffer is written verbatim. Each
//!   test therefore disables the hook up front so the assertion pins what `:w`
//!   really writes instead of drifting with the host's installed toolchain.

use super::harness::{TerminalSession, wait_for_contents};
use std::io::Write;
use std::path::PathBuf;

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

/// Paste a multi-line `.toml` blob into an empty buffer in Insert mode and
/// confirm every line survives on its own row (no bunching).
///
/// The written file is the blob plus one extra `\n`: the blob's trailing newline
/// leaves a blank final row, which `:w` terminates like any other line.
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

    // Keep the test hermetic: `format_on_save` is on by default and `.toml`
    // resolves to taplo, so on a host that has taplo the formatter would rewrite
    // the buffer and strip the trailing blank line — masking what `:w` actually
    // writes. Disabling the hook makes the outcome identical with or without
    // taplo/prettier installed.
    s.keys(":set format_on_save=off<Enter>");

    // Enter Insert mode at the top, paste the blob, leave Insert, write.
    s.keys("i");
    s.paste(toml);
    s.keys("<Esc>");
    s.keys(":w<Enter>");

    // Trailing blank row from the blob's final newline gets its own terminator.
    let want = format!("{toml}\n");
    let got = wait_for_contents(&path, &want);
    assert_eq!(
        got, want,
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
///
/// The final `\n` leaves a blank fifth row, so the file ends `y = 2\n\n` — vim
/// writes the same, and `:w` preserves it rather than trimming.
#[test]
fn bracketed_paste_crlf_normalised() {
    let crlf = "[a]\r\nx = 1\r\n[b]\r\ny = 2\r\n";
    let want = "[a]\nx = 1\n[b]\ny = 2\n\n";

    let (_keep, path) = seed(".toml", "");
    let mut s = TerminalSession::spawn_with_file(&path);

    // See the sibling test: taplo's availability is host-dependent and its
    // output loses the trailing blank line, so pin the hook off.
    s.keys(":set format_on_save=off<Enter>");

    s.keys("i");
    s.paste(crlf);
    s.keys("<Esc>");
    s.keys(":w<Enter>");

    let got = wait_for_contents(&path, want);
    assert_eq!(got, want, "CRLF paste should normalise to LF-split lines");
}
