//! E2e regression for kryptic-sh/hjkl#279 (slice 4): a whole-line yank (`yy`)
//! in one split must paste LINEWISE in a sibling split — landing on its own
//! new line below the cursor, not spliced into the middle of the current
//! line.
//!
//! Register bank (`hjkl_engine::Registers`, holding text + a per-slot
//! `linewise` flag) is already shared across windows via one `Arc<Mutex<_>>`
//! (see slices 1-3, which did the same for global_marks / last_substitute /
//! abbrevs). The standalone `Editor.yank_linewise` bool is a *different*,
//! deliberately per-window field — `do_paste` in `hjkl-vim/src/vim/command.rs`
//! reads `linewise` from the selected register *slot*, not from
//! `vim.yank_linewise`, specifically so cross-window (and cross-buffer)
//! paste carries the right layout. This test proves that wiring holds up
//! end-to-end across a real split.

use super::harness::TerminalSession;
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pty_harness/fixtures")
        .join(name)
}

/// True if `line` has `token` as one of its whitespace-separated tokens.
/// The rendered row includes a line-number gutter (and sometimes an inline
/// git-blame annotation after the content), so a substring check would give
/// false positives ("line2" is a substring of "line20") and a whole-line
/// equality check would false-negative on the gutter prefix. Token equality
/// sidesteps both: content words are always their own whitespace-delimited
/// token regardless of what surrounds them.
fn line_has_token(line: &str, token: &str) -> bool {
    line.split_whitespace().any(|w| w == token)
}

/// Open a 30-line file, split it, yank a whole line (`yy` on "line5") in the
/// new (top) window, focus the sibling window with `<C-w>w`, move to a
/// different line ("line20"), and paste (`p`). The paste must be LINEWISE:
/// "line20" stays intact on its own row and "line5" appears as a brand-new
/// row directly below it (pushing "line21" down), with the cursor landing on
/// the pasted line. A charwise paste would instead splice "line5" into the
/// middle of "line20" (e.g. "lline5ine20"), leaving no intact "line20" row.
#[test]
fn linewise_yank_in_one_split_pastes_linewise_in_sibling_split() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_30.txt"));

    // Open a horizontal split — new window (top) is created and focused,
    // duplicating the current (only) window's slot/buffer.
    s.keys(":split<Enter>");

    // In the new, focused (top) window: jump to line 5 and yank it whole
    // with `yy`. This writes the shared unnamed register slot with
    // linewise = true.
    s.keys(":5<Enter>");
    s.keys("yy");

    // Focus the sibling window (the original window, bottom half).
    s.keys("<C-w>w");

    // Move to line 20 in the sibling window, then paste after it.
    s.keys(":20<Enter>");
    s.keys("p");

    // Find "line20" on screen; the row directly below it must read exactly
    // "line5" (the freshly pasted line), and the row below THAT must read
    // "line21" — proving the paste inserted a new row rather than splicing
    // into "line20".
    let mut found_at: Option<u16> = None;
    for _ in 0..200 {
        for row in 0..23u16 {
            if line_has_token(&s.line(row), "line20") {
                found_at = Some(row);
                break;
            }
        }
        if found_at.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let row20 = found_at.expect(
        "expected an intact row containing the token \"line20\" after paste \
         — a charwise paste would have spliced \"line5\" into it, leaving no \
         intact \"line20\" row",
    );

    let row_below = s.line(row20 + 1);
    assert!(
        line_has_token(&row_below, "line5"),
        "row directly below \"line20\" should be the freshly pasted \
         \"line5\" as its OWN line (linewise paste) — got {row_below:?}"
    );

    let row_below_2 = s.line(row20 + 2);
    assert!(
        line_has_token(&row_below_2, "line21"),
        "row two below \"line20\" should be \"line21\", pushed down by the \
         linewise-inserted \"line5\" row — got {row_below_2:?}"
    );

    // The cursor should land on the pasted "line5" row (vim: linewise `p`
    // puts the cursor on the first non-blank of the newly pasted line).
    let (cursor_row, _) = s.cursor_cell_wait();
    assert_eq!(
        cursor_row,
        row20 + 1,
        "cursor should land on the pasted \"line5\" row after linewise `p`"
    );
}
