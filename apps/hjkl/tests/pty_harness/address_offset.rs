//! E2e regression for audit finding A4: `:+N` / `:-N` ex-command addresses
//! must be cursor-relative (vim semantics), not absolute line numbers.
//!
//! Before the fix, `crates/hjkl-ex/src/range.rs::parse_address` had no case
//! for a leading `+`/`-`, so `:+3` fell through to the permissive bare
//! line-number fallback in `handle_bare_line_number` — and Rust's
//! `usize::from_str` accepts a leading `+`, so `:+3` silently became an
//! absolute `goto_line(3)` regardless of where the cursor was.

use super::harness::TerminalSession;
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pty_harness/fixtures")
        .join(name)
}

/// Open a 30-line file, jump to line 5, then `:+3<Enter>` must land on line
/// 8 (cursor + 3) — NOT absolute line 3, which is what the pre-fix bug
/// produced.
#[test]
fn plus_n_address_is_cursor_relative() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_30.txt"));

    // Put the cursor on a known, non-1 line first so an absolute-vs-relative
    // divergence is observable.
    s.keys(":5<Enter>");
    let (row5, _) = s.cursor_cell_wait();
    assert!(
        s.line(row5).contains("line5"),
        ":5 must land on line5, got {:?}",
        s.line(row5)
    );

    // `:+3` from line 5 → line 8 (cursor-relative). The pre-fix bug jumped
    // to absolute line 3 instead.
    s.keys(":+3<Enter>");
    let (row_after_plus, _) = s.cursor_cell_wait();
    let line_after_plus = s.line(row_after_plus);
    assert!(
        line_after_plus.contains("line8"),
        ":+3 from line5 must land on line8 (cursor + 3), got {line_after_plus:?} \
         at row {row_after_plus} — if this shows line3, the bug regressed: \
         `+3` was parsed as an absolute line number instead of a cursor-relative offset"
    );

    // `:-2` from line 8 → line 6 (cursor-relative backward).
    s.keys(":-2<Enter>");
    let (row_after_minus, _) = s.cursor_cell_wait();
    let line_after_minus = s.line(row_after_minus);
    assert!(
        line_after_minus.contains("line6"),
        ":-2 from line8 must land on line6 (cursor - 2), got {line_after_minus:?} \
         at row {row_after_minus}"
    );
}
