//! E2e regression for kryptic-sh/hjkl#279 (slice 1): vim's uppercase
//! (global) marks must be visible from every window, not just the window
//! that set them.
//!
//! Before the fix, each window's `Editor` had its own private
//! `global_marks` map (a plain `BTreeMap`), so `mA` set in one split was
//! invisible to `'A` typed in a sibling split onto the *same* buffer — a
//! silent divergence from vim, where uppercase marks are session-global.

use super::harness::TerminalSession;
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pty_harness/fixtures")
        .join(name)
}

/// Open a 120-line file, split it, set global mark `A` at line 60 in the new
/// (top) window, focus the sibling window with `<C-w>w`, move away from line
/// 60, then jump with `'A`. The cursor must land on a row showing "line60" —
/// proving the mark set in one window's `Editor` was visible from the
/// other's.
#[test]
fn global_mark_set_in_one_split_is_visible_in_sibling_split() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_120.txt"));

    // Open a horizontal split — new window (top) is created and focused,
    // duplicating the current (only) window's slot/buffer.
    s.keys(":split<Enter>");

    // In the new, focused (top) window: jump to line 60 and set global mark
    // 'A' there.
    s.keys(":60<Enter>");
    s.keys("mA");

    // Focus the sibling window (the original window, bottom half).
    s.keys("<C-w>w");

    // Move away from line 60 in the sibling window so the upcoming jump is
    // observable, then jump to global mark 'A'.
    s.keys("gg");
    s.keys("'A");

    // The cursor must land on a visible row showing "line60" — the sibling
    // window's Editor saw the mark set in the other window's Editor.
    let mut cursor_row = u16::MAX;
    let mut last_line = String::new();
    let mut ok = false;
    for _ in 0..200 {
        let (row, _) = s.cursor_cell_wait();
        cursor_row = row;
        last_line = s.line(row);
        if last_line.contains("line60") {
            ok = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        ok,
        "after 'A' in the sibling window, cursor row {cursor_row} shows \
         {last_line:?} — expected a visible row containing \"line60\" (global \
         mark 'A' set in the other window must be visible here)"
    );
}
