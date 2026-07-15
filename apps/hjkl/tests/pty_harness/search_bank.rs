//! E2e regression for audit finding B2: vim's last-search pattern (the `"/`
//! register) is session-global — `n`/`N` in ANY window repeat whatever
//! pattern was most recently committed anywhere. Before the fix, each
//! window's `Editor` had its own private `last_search` / `last_search_forward`
//! / `search_history` fields (only copied once, at window-creation time, by
//! `make_view_editor`), so `/foo<CR>` committed in one split was invisible to
//! `n` typed in a sibling split — a silent divergence from vim.

use super::harness::TerminalSession;
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pty_harness/fixtures")
        .join(name)
}

/// Open a file with "foo" on several lines, split it, commit `/foo<CR>` in
/// the new (top) window, focus the sibling window with `<C-w>w`, reset its
/// cursor to line 1, then press `n`. The sibling window must reuse the
/// pattern committed in the other window — proving the last-search bank is
/// shared, not per-window.
#[test]
fn last_search_committed_in_one_split_is_reused_by_n_in_sibling_split() {
    let mut s = TerminalSession::spawn_with_file(&fixture("substitute_words.txt"));

    // Open a horizontal split — new window (top) is created and focused,
    // duplicating the current (only) window's slot/buffer.
    s.keys(":split<Enter>");

    // In the new, focused (top) window: commit a forward search for "foo".
    // Cursor starts at row 0 col 0 (on "foo" itself), so the forward search
    // lands on the NEXT match (row 1, "foo bravo") — this is just to commit
    // the pattern + direction, not the row we check below.
    s.keys("/foo<Enter>");

    // Focus the sibling window (the original window, bottom half).
    s.keys("<C-w>w");

    // Reset the sibling's cursor to line 1 ("foo alpha") so the upcoming
    // `n` press is observable regardless of where the split left it.
    s.keys(":1<Enter>");

    // The sibling window never ran its own search — its own (buggy,
    // per-window) `last_search` would be `None`. Press `n` and see whether
    // it repeats the pattern committed in the OTHER window.
    s.keys("n");

    // The cursor must land on a visible row showing "foo bravo" (the next
    // "foo" after row 0) — proving the sibling window's Editor saw the
    // pattern committed in the other window's Editor.
    let mut cursor_row = u16::MAX;
    let mut last_line = String::new();
    let mut ok = false;
    for _ in 0..200 {
        let (row, _) = s.cursor_cell_wait();
        cursor_row = row;
        last_line = s.line(row);
        if last_line.contains("foo bravo") {
            ok = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        ok,
        "after 'n' in the sibling window, cursor row {cursor_row} shows \
         {last_line:?} — expected a visible row containing \"foo bravo\" \
         (last search pattern \"foo\" committed in the other window must be \
         reused here)"
    );
}
