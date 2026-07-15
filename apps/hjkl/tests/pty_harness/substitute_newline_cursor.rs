//! E2e regression for audit finding A5: `:s` replacements that embed a
//! newline via `\r` across a MULTI-ROW range must land the cursor on the
//! correct final row.
//!
//! `hjkl-engine::substitute::apply_substitute` tracked the "last changed
//! line" in PRE-split row-index space: as earlier rows in the range also
//! split into extra rows (because their own replacement contained `\r`),
//! the recorded row index was never adjusted for the rows those earlier
//! splits inserted, so the cursor landed on the wrong physical line once
//! the buffer was re-split. See `crates/hjkl-engine/src/substitute.rs`
//! (`apply_substitute`) for the coordinate-space fix.

use super::harness::TerminalSession;
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pty_harness/fixtures")
        .join(name)
}

/// Fixture is two lines: `a,b` / `c,d`. Running `:%s/,/\r/` splits EACH line
/// on its comma, growing the buffer to four lines: `a` / `b` / `c` / `d`.
/// Vim leaves the cursor on the first non-blank of the LAST changed line
/// (`d`, real row 3) — not row 1 (`b`), which is where the pre-fix,
/// PRE-split row index would incorrectly land.
#[test]
fn multi_row_backslash_r_substitute_lands_cursor_on_last_split_line() {
    let mut s = TerminalSession::spawn_with_file(&fixture("substitute_newline_split.txt"));

    s.keys(":%s/,/\\r/<Enter>");

    // Buffer must have split into four lines in the right order. The gutter
    // renders a line-number column before the content (e.g. "  1 a"), so
    // check the trailing letter rather than an exact match.
    fn ends_with_letter(line: &str, letter: char) -> bool {
        line.trim_end().ends_with(letter)
    }

    let mut ok = false;
    let mut lines = [String::new(), String::new(), String::new(), String::new()];
    for _ in 0..200 {
        lines = [s.line(0), s.line(1), s.line(2), s.line(3)];
        if ends_with_letter(&lines[0], 'a')
            && ends_with_letter(&lines[1], 'b')
            && ends_with_letter(&lines[2], 'c')
            && ends_with_letter(&lines[3], 'd')
        {
            ok = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        ok,
        "expected the buffer to split into four lines a/b/c/d after \
         ':%s/,/\\r/', got {lines:?}"
    );

    // Cursor must land on real row 3 ("d"), the last changed line — not row
    // 1 ("b"), which is what the pre-fix PRE-split row index produced.
    let (cursor_row, _) = s.cursor_cell_wait();
    let cursor_line = s.line(cursor_row);
    assert_eq!(
        cursor_row, 3,
        "cursor landed on row {cursor_row} ({cursor_line:?}) — expected row \
         3 (\"d\"), the last changed line in POST-split coordinates"
    );
    assert!(
        ends_with_letter(&cursor_line, 'd'),
        "cursor row {cursor_row} shows {cursor_line:?} — expected it to end \
         with \"d\""
    );
}
