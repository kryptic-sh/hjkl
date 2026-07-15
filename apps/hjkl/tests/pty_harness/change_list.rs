//! E2e regression for audit finding B3: vim's changelist (`g;` / `g,`) and
//! the `'.` / `` `. `` "last change" mark are session state scoped PER
//! BUFFER — every window viewing the same buffer shares one changelist;
//! windows on different buffers get independent ones.
//!
//! Before the fix, each window's `Editor` had its own private `change_list`
//! / `change_list_cursor` / `last_edit_pos` fields, so an edit made in one
//! split was invisible to `g;` typed in a sibling split onto the *same*
//! buffer — a silent divergence from vim, where the changelist is
//! per-buffer, not per-window.

use super::harness::TerminalSession;
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pty_harness/fixtures")
        .join(name)
}

/// Open a 120-line file, split it, make an edit at line 60 in the new (top)
/// window, focus the sibling window with `<C-w>w`, move away from line 60,
/// then press `g;`. The cursor must land back on line 60 — proving the
/// changelist entry recorded in one window's `Editor` was visible from the
/// other's.
#[test]
fn edit_made_in_one_split_is_visible_to_g_semicolon_in_sibling_split() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_120.txt"));

    // Open a horizontal split — new window (top) is created and focused,
    // duplicating the current (only) window's slot/buffer.
    s.keys(":split<Enter>");

    // In the new, focused (top) window: jump to line 60 and make an edit
    // there (insert "X" before the line's text) — this records a changelist
    // entry and sets the `'.` / `` `. `` mark at (row 59, col 0).
    s.keys(":60<Enter>");
    s.keys("iX<Esc>");

    // Focus the sibling window (the original window, bottom half). Its own
    // Editor never ran an edit, so a per-window (buggy) changelist would be
    // empty here.
    s.keys("<C-w>w");

    // Move away from line 60 in the sibling window so the upcoming jump is
    // observable.
    s.keys("gg");

    // `g;` walks to the most recent changelist entry. The sibling window
    // never made an edit itself — if the changelist bank is (bug) per
    // window, this is a no-op and the cursor stays on line 1.
    s.keys("g;");

    // The cursor must land on a visible row showing "line60" — the sibling
    // window's Editor saw the changelist entry recorded by the other
    // window's Editor on the same buffer.
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
        "after 'g;' in the sibling window, cursor row {cursor_row} shows \
         {last_line:?} — expected a visible row containing \"line60\" (the \
         changelist entry recorded by the edit in the other window must be \
         visible here)"
    );
}

/// Same setup, but checks the `` `. `` last-change mark instead of `g;` —
/// covers the second per-buffer field independently.
#[test]
fn edit_made_in_one_split_is_visible_to_backtick_dot_in_sibling_split() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_120.txt"));

    s.keys(":split<Enter>");
    s.keys(":60<Enter>");
    s.keys("iX<Esc>");
    s.keys("<C-w>w");
    s.keys("gg");

    // `` `. `` jumps to the exact last-change position (charwise), unlike
    // `g;` which participates in the walkable ring — exercises the other
    // per-buffer field (`last_edit_pos` / `ChangeBank::last_edit`).
    s.keys("`.");

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
        "after '`.' in the sibling window, cursor row {cursor_row} shows \
         {last_line:?} — expected a visible row containing \"line60\" (the \
         last-change mark set by the edit in the other window must be \
         visible here)"
    );
}
