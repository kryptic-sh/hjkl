//! E2e regression for audit finding A3: a backward ex-command range
//! (`:8,3d`, where the start address is greater than the end address) must
//! be rejected with vim's `E493: Backwards range given`, not silently
//! swapped into a forward range and executed.
//!
//! Before the fix, `crates/hjkl-ex/src/range.rs::parse_range` unconditionally
//! swapped `start > end` into `(end, start)`, so `:8,3d` silently deleted
//! lines 3-8 instead of erroring — a silent divergence from vim, which
//! errors outright for a backward range given non-interactively (vim only
//! offers to swap via an interactive "OK to swap" prompt, which a
//! headless/keystroke-driven editor like hjkl has no equivalent of).

use super::harness::TerminalSession;
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pty_harness/fixtures")
        .join(name)
}

/// Errors render as a toast (top-right floating popup, see
/// `hjkl-holler-tui::HollerLayout`), not on the bottom status line — so
/// scan the whole 24-row screen for `needle` rather than assuming a fixed
/// row.
fn screen_contains(s: &TerminalSession, needle: &str) -> bool {
    (0..24).any(|row| s.line(row).contains(needle))
}

/// `:8,3d<Enter>` on a 30-line file must show `E493` (as a toast) and leave
/// the buffer completely unchanged — no lines deleted.
#[test]
fn backward_range_delete_errors_and_leaves_buffer_unchanged() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_30.txt"));

    // Sanity: before running anything, row 2 (0-based) is "line3" and row 7
    // is "line8" — the file's first 23 lines fit on the 24-row terminal (row
    // 23 is the status line), so screen rows map 1:1 to buffer lines here.
    assert!(
        s.line(2).contains("line3"),
        "sanity: row 2 should show line3 before any edit, got {:?}",
        s.line(2)
    );
    assert!(
        s.line(7).contains("line8"),
        "sanity: row 7 should show line8 before any edit, got {:?}",
        s.line(7)
    );

    // Backward range: start (8) > end (3).
    s.keys(":8,3d<Enter>");

    // Errors render as a top-right toast, not the status line — scan the
    // whole screen for the E493 message.
    assert!(
        screen_contains(&s, "E493"),
        ":8,3d must report E493 (Backwards range given) somewhere on screen"
    );

    // Buffer must be UNCHANGED: line3 and line8 must still be on their
    // original rows. Before the fix, `:8,3d` silently swapped to `:3,8d` and
    // deleted lines 3-8, which would shift line9 up onto row 2.
    assert!(
        s.line(2).contains("line3"),
        "buffer must be unchanged after a rejected backward range — expected \
         line3 still on row 2, got {:?} (a shifted-up line like line9 here \
         would mean the backward range was silently executed instead of \
         erroring)",
        s.line(2)
    );
    assert!(
        s.line(7).contains("line8"),
        "buffer must be unchanged after a rejected backward range — expected \
         line8 still on row 7, got {:?}",
        s.line(7)
    );
}

/// Contrast case: the equivalent FORWARD range, `:3,8d<Enter>`, must still
/// delete lines 3-8 exactly as before — the E493 rejection only applies to
/// genuinely backward ranges.
#[test]
fn forward_range_delete_still_deletes() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_30.txt"));

    s.keys(":3,8d<Enter>");

    // No error should be shown anywhere on screen.
    assert!(
        !screen_contains(&s, "E493"),
        ":3,8d is a forward range and must not error"
    );

    // Lines 3-8 are gone: row 2 (previously "line3") now shows "line9",
    // which shifted up by 6 lines.
    assert!(
        s.line(2).contains("line9"),
        ":3,8d must delete lines 3-8, shifting line9 up onto row 2, got {:?}",
        s.line(2)
    );
}
