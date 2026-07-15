//! E2e regression for kryptic-sh/hjkl#279 (slice 2): vim's last substitute
//! command — repeated by `:&` / `:&&` — must be visible from every window,
//! not just the window that ran the original `:s`.
//!
//! Before the fix, each window's `Editor` had its own private
//! `last_substitute` field, so `:s/foo/bar/` run in one split left `:&` in a
//! sibling split with nothing to repeat (or a stale substitute from that
//! sibling's own history) — a silent divergence from vim, where the last
//! substitute is session-global.

use super::harness::TerminalSession;
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pty_harness/fixtures")
        .join(name)
}

/// Open a file with "foo" repeated on several lines, split it, run
/// `:s/foo/bar/` on one line in the new (top) window, focus the sibling
/// window with `<C-w>w`, move to a *different* line that still has "foo",
/// and run `:&`. The sibling window must reuse the substitute set in the
/// other window — proving `last_substitute` is shared, not per-window.
#[test]
fn last_substitute_set_in_one_split_is_reused_in_sibling_split() {
    let mut s = TerminalSession::spawn_with_file(&fixture("substitute_words.txt"));

    // Open a horizontal split — new window (top) is created and focused,
    // duplicating the current (only) window's slot/buffer.
    s.keys(":split<Enter>");

    // In the new, focused (top) window: jump to line 2 ("foo bravo") and
    // run `:s/foo/bar/`, turning it into "bar bravo".
    s.keys(":2<Enter>");
    s.keys(":s/foo/bar/<Enter>");

    // Focus the sibling window (the original window, bottom half).
    s.keys("<C-w>w");

    // Move to a different line that still has "foo" ("foo delta" on line 4)
    // in the sibling window, then repeat the last substitute with `:&`.
    s.keys(":4<Enter>");
    s.keys(":&<Enter>");

    // The cursor must land on a visible row showing "bar delta" — the
    // sibling window's Editor reused the substitute set in the other
    // window's Editor (pattern "foo" / replacement "bar").
    let mut cursor_row = u16::MAX;
    let mut last_line = String::new();
    let mut ok = false;
    for _ in 0..200 {
        let (row, _) = s.cursor_cell_wait();
        cursor_row = row;
        last_line = s.line(row);
        if last_line.contains("bar delta") {
            ok = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        ok,
        "after ':&' in the sibling window, cursor row {cursor_row} shows \
         {last_line:?} — expected a visible row containing \"bar delta\" \
         (last substitute set in the other window must be reused here)"
    );

    // Sanity: the line the substitute originally ran on (line 2, now "bar
    // bravo") stayed untouched by the sibling's `:&`, and the *other* "foo"
    // lines are still "foo" — confirms `:&` in the sibling only touched the
    // current line, not the whole buffer.
    let mut found_bravo = false;
    let mut found_charlie_untouched = false;
    for row in 0..30 {
        let line = s.line(row);
        if line.contains("bar bravo") {
            found_bravo = true;
        }
        if line.contains("foo charlie") {
            found_charlie_untouched = true;
        }
    }
    assert!(
        found_bravo,
        "expected \"bar bravo\" (from the original :s in the split window) \
         to still be visible somewhere on screen"
    );
    assert!(
        found_charlie_untouched,
        "expected \"foo charlie\" to remain untouched — :& should only \
         affect the current line"
    );
}
