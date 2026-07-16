//! E2e tests for `@{reg}` macro replay (audit R2 — iterative work-queue
//! replay). Guards the basic record/replay-with-count path end-to-end
//! through a real pty.
//!
//! The self-referential-macro (`qaj@aq` style) and huge-count bounds are
//! covered by App-level unit tests in `app::tests::marks_registers`
//! (`self_referential_macro_terminates_without_stack_overflow`,
//! `huge_count_macro_replay_is_bounded`) — they are deliberately NOT
//! duplicated here because their abort paths are timing-sensitive under a
//! loaded pty and the unit tests exercise the identical routing stack.

use super::harness::TerminalSession;
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pty_harness/fixtures")
        .join(name)
}

/// Record `qaxq` on "line1" (deletes the `l`), then `5@a` must delete
/// exactly min(5, remaining chars) = 4 more chars — the whole line is gone,
/// and line2 below is untouched.
#[test]
fn count_macro_replay_deletes_remaining_chars() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_30.txt"));

    // Wait for the buffer to render before typing.
    assert!(
        s.wait_for_line(0, "line1", 2000),
        "buffer must render line1 before recording, got {:?}",
        s.line(0)
    );

    // Record: qa x q — deletes 'l', leaving "ine1".
    s.keys("qaxq");
    assert!(
        s.wait_for_line(0, "ine1", 2000),
        "recording x must delete the leading 'l', got {:?}",
        s.line(0)
    );

    // Replay with count: 5@a — deletes the remaining 4 chars ("ine1");
    // the 5th x is a no-op on the now-empty line.
    s.keys("5@a");

    let l0 = s.line(0);
    assert!(
        !l0.contains("ine1"),
        "5@a must delete the remaining chars of line1, got {l0:?}"
    );
    let l1 = s.line(1);
    assert!(
        l1.contains("line2"),
        "line2 must be untouched by the replay, got {l1:?}"
    );
}
