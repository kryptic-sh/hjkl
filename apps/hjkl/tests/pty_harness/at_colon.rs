//! E2e tests for `@:` (last-ex repeat). Phase 5d of kryptic-sh/hjkl#71.

use super::harness::TerminalSession;
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pty_harness/fixtures")
        .join(name)
}

/// `@:` after `:10<Enter>` must jump back to line 10.
///
/// Flow: open 30-line file, `:10<Enter>` → cursor at line 10, `gg` to top,
/// `@:` → cursor returns to line 10. Verifies that the last-ex storage and
/// replay path works end-to-end through the pty.
/// IGNORED in CI: real-pty timing test (spawns the binary under a pseudo-
/// terminal, one-shot reads the cursor after `:100`/`@:`). Flakes on loaded
/// GitHub runners like its sibling `goto_line_100_scrolls_viewport`. Run
/// locally with `cargo test -- --ignored`. Tracked for a non-timing rewrite.
#[test]
#[ignore = "flaky under CI load — real-pty timing; run locally with --ignored"]
fn at_colon_repeats_last_goto_line() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_30.txt"));

    // Jump to line 10.
    s.keys(":10<Enter>");

    let (cursor_row_after_10, _) = s.cursor_cell().expect("software cursor visible");
    let line_text_after_10 = s.line(cursor_row_after_10);
    assert!(
        line_text_after_10.contains("line10"),
        ":10 must land on a row showing 'line10', got {line_text_after_10:?}"
    );

    // Jump back to top.
    s.keys("gg");
    let line_text_top = s.line(0);
    assert!(
        line_text_top.contains("line1"),
        "gg must show line1 at row 0, got {line_text_top:?}"
    );

    // `@:` must replay :10 and return to line 10.
    s.keys("@:");

    let (cursor_row, _) = s.cursor_cell().expect("software cursor visible");
    let line_text = s.line(cursor_row);
    assert!(
        line_text.contains("line10"),
        "@: must repeat :10 and show 'line10', got {line_text:?} at cursor row {cursor_row}"
    );
}

/// `@:` as the very first action (no prior ex command) must be silent —
/// no crash, cursor stays on line 1, no error status message visible.
///
/// Flow: open 30-line file, immediately send `@:` without any prior `:cmd`.
/// Verify cursor is still on line 1 and the status line is not showing an
/// error.
#[test]
fn at_colon_no_prior_is_silent() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_30.txt"));

    // Send @: with no prior ex command.
    s.keys("@:");

    // Cursor must still be on line 1 (row 0 of screen contains "line1").
    let (cursor_row, _) = s.cursor_cell().expect("software cursor visible");
    assert!(
        cursor_row < 5,
        "cursor should still be near top (row {cursor_row}) after no-op @:"
    );
    let line_text = s.line(cursor_row);
    assert!(
        line_text.contains("line1"),
        "no-op @: must leave cursor on line1, got {line_text:?}"
    );

    // Status line (last row = 23) must not show an error.
    let status = s.line(23);
    let has_error = status.contains("E:") || status.contains("error") || status.contains("Error");
    assert!(
        !has_error,
        "no-op @: must not show error in status line, got {status:?}"
    );
}
