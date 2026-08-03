//! e2e: a paste rejected for exceeding the byte budget says so.
//!
//! `do_paste` bailed out with `false` for an over-budget paste and nothing
//! downstream could tell that apart from "empty register", so the keystroke
//! did nothing and reported nothing. `hjkl-vim` cannot reach the message bar
//! itself — it does not depend on `hjkl-holler`, and the `Host` trait's
//! `emit_intent` hook carries `()` in every implementor — so the message is
//! queued on the editor (`Editor::push_error`) and drained by the app after
//! each key.
//!
//! This is the piece that could not be landed on its own: without the drain
//! in `apps/hjkl`, the queue is a channel with no consumer.

use super::harness::TerminalSession;
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pty_harness/fixtures")
        .join(name)
}

/// Errors render as a top-right toast, not on the status line, so scan the
/// whole screen (same convention as `backward_range.rs`).
fn screen_contains(s: &TerminalSession, needle: &str) -> bool {
    (0..24).any(|row| s.line(row).contains(needle))
}

/// `yy` then a paste count large enough to blow `MAX_PASTE_BYTES` must show
/// vim's `E342` and leave the buffer untouched.
///
/// The count is what pushes the request over the budget — the register holds
/// one short line — so the test never allocates anything near the cap.
#[test]
fn oversized_paste_reports_e342_and_leaves_the_buffer_unchanged() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_30.txt"));

    assert!(
        s.line(1).contains("line2"),
        "sanity: row 1 should show line2 before any edit, got {:?}",
        s.line(1)
    );

    // `MAX_PASTE_BYTES` is 64 MiB; a 6-byte register at this count is well
    // past it, and the guard runs before anything is allocated.
    s.keys("yy99999999p");

    assert!(
        screen_contains(&s, "E342"),
        "an over-budget paste must report E342 somewhere on screen; got rows \
         {:?}",
        (0..24).map(|r| s.line(r)).collect::<Vec<_>>()
    );
    assert!(
        s.line(1).contains("line2"),
        "a rejected paste must leave the buffer unchanged — expected line2 \
         still on row 1, got {:?}",
        s.line(1)
    );
}

/// Contrast: an in-budget paste of the same shape still pastes, and says
/// nothing. Without this the test above would pass against a build that
/// rejected every paste.
#[test]
fn in_budget_paste_still_pastes_and_reports_nothing() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_30.txt"));

    s.keys("yy3p");

    assert!(
        !screen_contains(&s, "E342"),
        "an in-budget paste must not report E342"
    );
    assert!(
        s.line(1).contains("line1"),
        "`yy3p` on row 0 must put a copy of line1 on row 1, got {:?}",
        s.line(1)
    );
}
