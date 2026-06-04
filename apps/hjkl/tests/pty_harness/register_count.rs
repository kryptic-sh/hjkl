//! E2e tests for register + count audit. Phase 5e of kryptic-sh/hjkl#71.

use super::harness::TerminalSession;
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pty_harness/fixtures")
        .join(name)
}

/// `"add` then `"ap` round-trips: delete line into register 'a', paste it back.
///
/// Flow: open 30-line file, `"add` deletes line1 into reg 'a', `j` moves to
/// next line, `"ap` pastes line1 text below cursor. Assert the pasted text
/// shows "line1" on the screen row below the cursor.
#[test]
fn register_a_dd_then_p_round_trips() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_30.txt"));

    // Delete line1 into register 'a'.
    s.keys("\"add");

    // Cursor is now on line2 (the new first line).
    let (cur_row, _) = s.cursor_cell().expect("software cursor visible");
    let line_after_delete = s.line(cur_row);
    assert!(
        line_after_delete.contains("line2"),
        "after \"add line2 must be at cursor row {cur_row}, got {line_after_delete:?}"
    );

    // Paste from register 'a' below current line.
    s.keys("\"ap");

    // The pasted content should appear one row below the cursor.
    let (cur_row2, _) = s.cursor_cell().expect("software cursor visible");
    // `p` moves cursor to the pasted line for linewise yanks; check nearby rows.
    let found_line1 =
        (cur_row2.saturating_sub(1)..=cur_row2 + 1).any(|r| s.line(r).contains("line1"));
    assert!(
        found_line1,
        "after \"ap the screen near cursor (row {cur_row2}) must show 'line1'"
    );
}

/// `5"add` — count typed BEFORE `"` must be applied (regression for the
/// `BeginPendingSelectRegister` count-reset bug fixed in Phase 5e).
///
/// Flow: open 30-line file, `5"add` must delete lines 1-5 into register 'a'.
/// Assert: first visible line is "line6".
#[test]
fn count_5_quote_a_dd_deletes_5_lines() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_30.txt"));

    // `5"add` — outer count 5, register 'a', doubled delete.
    s.keys("5\"add");

    // Cursor should be at top; first content line must be "line6".
    let (cur_row, _) = s.cursor_cell().expect("software cursor visible");
    let visible = s.line(cur_row);
    assert!(
        visible.contains("line6"),
        "5\"add must delete lines 1-5; cursor row {cur_row} must show 'line6', got {visible:?}"
    );
}

/// `3@a` plays macro 'a' three times.
///
/// Flow: open 30-line file, record macro `qa j q` (move down one line), then
/// `3@a` replays it three times. Assert cursor moved 3 rows from where it was
/// after recording.
#[test]
fn count_3_at_a_repeats_macro_three_times() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_30.txt"));

    // Record macro 'a': just move down one line.
    s.keys("qajq");

    // After recording: cursor moved once (row 1 in 0-based; screen row 1).
    let (row_after_record, _) = s.cursor_cell().expect("software cursor visible");

    // Play macro 'a' three times.
    s.keys("3@a");

    let (row_after_play, _) = s.cursor_cell().expect("software cursor visible");
    assert_eq!(
        row_after_play,
        row_after_record + 3,
        "3@a must move cursor 3 more rows; was at {row_after_record}, now at {row_after_play}"
    );
}
