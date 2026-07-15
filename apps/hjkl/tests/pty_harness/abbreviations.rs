//! E2e regression for kryptic-sh/hjkl#279 (slice 3): vim's `:iabbrev` /
//! `:abbreviate` tables must be visible from every window, not just the
//! window that defined them.
//!
//! Before the fix, each window's `Editor` had its own private `abbrevs`
//! table (a plain `Vec`), so `:iabbrev foo bar` defined in one split was
//! invisible to insert-mode expansion in a sibling split — a silent
//! divergence from vim, where abbreviations are session-global.

use super::harness::TerminalSession;
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pty_harness/fixtures")
        .join(name)
}

/// Open a file, split it, define `:iabbrev foo bar` in the new (top)
/// window, focus the sibling window with `<C-w>w`, enter insert mode there
/// and type "foo " (space is a non-keyword trigger char). The sibling
/// window must expand "foo" to "bar" — proving the abbreviation table
/// defined in the other window's `Editor` is visible from here.
#[test]
fn iabbrev_defined_in_one_split_expands_in_sibling_split() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_30.txt"));

    // Open a horizontal split — new window (top) is created and focused,
    // duplicating the current (only) window's slot/buffer.
    s.keys(":split<Enter>");

    // In the new, focused (top) window: define an insert-mode abbreviation.
    s.keys(":iabbrev foo bar<Enter>");

    // Focus the sibling window (the original window, bottom half).
    s.keys("<C-w>w");

    // In the sibling window: open a new line below and type "foo " —
    // the trailing space is a non-keyword trigger char that fires
    // abbreviation expansion.
    s.keys("o");
    s.keys("foo ");
    s.keys("<Esc>");

    // The cursor must land on (or near) a visible row showing "bar" — the
    // sibling window's Editor saw the abbreviation defined in the other
    // window's Editor. It must NOT show the literal, unexpanded "foo ".
    let mut cursor_row = u16::MAX;
    let mut last_line = String::new();
    let mut ok = false;
    for _ in 0..200 {
        let (row, _) = s.cursor_cell_wait();
        cursor_row = row;
        last_line = s.line(row);
        if last_line.contains("bar") {
            ok = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        ok,
        "after typing \"foo \" in the sibling window, cursor row \
         {cursor_row} shows {last_line:?} — expected a visible row \
         containing \"bar\" (iabbrev defined in the other window must \
         expand here)"
    );
    assert!(
        !last_line.contains("foo "),
        "expected \"foo \" to have been expanded to \"bar \", but the row \
         still shows the literal unexpanded text: {last_line:?}"
    );
}
