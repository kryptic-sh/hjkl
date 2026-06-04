//! Render-sync regression tests.
//!
//! These tests catch the bug class where the hjkl engine's internal state
//! (cursor position, viewport top_row) moves correctly but the rendered
//! terminal output visible to the user shows stale data.
//!
//! Known fixed regressions targeted here:
//!   23cb46b — `:100<Enter>` cursor stuck (window cursor cache not synced
//!              after `ex::run`)
//!   4414170 — pending-state Outcome arms missing `sync_after_engine_mutation`
//!   1cead4e — keymap-dispatched motion: engine cursor moves but viewport
//!              doesn't scroll
//!   0694b42 — non-Normal mode keymap Match missing sync
//!   219de02 — keymap-Match dispatch missing viewport sync

use super::harness::TerminalSession;
use std::path::Path;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pty_harness/fixtures")
        .join(name)
}

/// Return true when any screen line (0-based row index) contains `needle`.
fn any_line_contains(session: &TerminalSession, needle: &str) -> bool {
    for row in 0..24 {
        if session.line(row).contains(needle) {
            return true;
        }
    }
    false
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Regression for 23cb46b.
///
/// Open a 120-line file, send `:100<Enter>`, and assert that the cursor lands
/// on a *visible* screen row showing "line100". Before the fix, the engine
/// moved the cursor to line 100 but the window cursor cache wasn't flushed
/// after `ex::run`, so the displayed cursor stayed at row 0 of the screen
/// even though line 100 was scrolled into view.
///
/// IGNORED in CI: a real-pty render-sync test — it spawns the binary under a
/// pseudo-terminal and depends on the editor emitting the scroll frame within
/// a timing budget, which a loaded GitHub runner can't guarantee (it has flaked
/// across several unrelated commits even after a 10s poll). Run locally with
/// `cargo test -- --ignored`. Tracked for a deterministic, non-timing rewrite.
#[test]
#[ignore = "flaky under CI load — real-pty timing; run locally with --ignored"]
fn goto_line_100_scrolls_viewport() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_120.txt"));

    s.keys(":100<Enter>");

    // Poll until the cursor lands on a visible row whose text contains
    // "line100". A fixed settle delay races the repaint on a loaded CI runner:
    // both the scroll frame and the reported cursor position can arrive after
    // keys()'s 200ms settle window, so a one-shot read (or a two-read scan that
    // polls "any row" then re-reads the cursor) can still see stale state. Poll
    // the *cursor row* directly with a generous budget so the assertion can't
    // race the render.
    let mut cursor_row = u16::MAX;
    let mut last_line = String::new();
    let mut ok = false;
    for _ in 0..1000 {
        if let Some((row, _)) = s.cursor_cell() {
            cursor_row = row;
            last_line = s.line(row);
            if row < 24 && last_line.contains("line100") {
                ok = true;
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        ok,
        "after :100<Enter>, cursor row {cursor_row} shows {last_line:?} — \
         expected a visible row containing \"line100\""
    );
}

/// Regression for 1cead4e / 219de02.
///
/// Open a 120-line file, jump to the bottom with `G`, then go to the top with
/// `gg`. The cursor must land on a row showing "line1". Before fixes in the
/// keymap-dispatch chain, `gg` moved the engine cursor but didn't sync the
/// viewport, so the screen still showed lines from near the bottom.
#[test]
fn gg_from_normal_lands_top() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_120.txt"));

    s.keys("G");
    s.keys("gg");

    let (cursor_row, _) = s.cursor_cell().expect("software cursor visible after gg");
    let line_text = s.line(cursor_row);

    assert!(
        line_text.contains("line1"),
        "after gg: cursor row {cursor_row} shows {line_text:?} — expected \"line1\""
    );
    // "line1" must be at or near the top of the screen.
    assert!(
        cursor_row < 5,
        "after gg: cursor row {cursor_row} is too far down — viewport didn't scroll to top"
    );
}

/// Regression for 0694b42 / 4414170.
///
/// Open a 30-line file, move down a few lines, enter Visual with `v`, then
/// jump to the top with `gg`. The cursor must land on a row showing "line1".
/// This exercises the sync path in non-Normal modes where the keymap Match
/// arm was previously missing `sync_after_engine_mutation`.
#[test]
fn gg_in_visual_extends_selection_to_top() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_30.txt"));

    // Move down 3 lines, enter Visual, then jump to top.
    s.keys("jjjvgg");

    let (cursor_row, _) = s.cursor_cell().expect("software cursor visible after v+gg");

    // Cursor must be on a visible row (viewport sync works).
    assert!(cursor_row < 24, "cursor row {cursor_row} is off-screen");

    // The screen row at the cursor should show "line1" (top of file).
    let line_text = s.line(cursor_row);
    assert!(
        line_text.contains("line1"),
        "after v+gg: cursor row {cursor_row} shows {line_text:?} — expected \"line1\""
    );
}

/// Regression for 1cead4e.
///
/// Open a 120-line file and move down 30 lines with `30j`. The viewport must
/// scroll so that "line1" is no longer visible on screen row 0. Before the
/// fix, the engine cursor moved but the viewport's top_row was never updated,
/// so line1 remained pinned at the top even when the cursor was on line 31.
#[test]
fn j_past_viewport_bottom_scrolls() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_120.txt"));

    s.keys("30j");

    // Row 0 must NOT show "line1" (the file's first line) anymore.
    // The gutter format is " NNN linecontent". After 30j the top row must
    // show a line number greater than 1. We parse the leading line number
    // out of the trimmed text.
    let row0_text = s.line(0);
    // The trimmed text starts with the line number (e.g. "14 line14").
    let top_line_num: u32 = row0_text
        .split_whitespace()
        .next()
        .and_then(|tok| tok.parse().ok())
        .unwrap_or(1);
    assert!(
        top_line_num > 1,
        "screen row 0 still shows line 1 ({row0_text:?}) after 30j — viewport didn't scroll"
    );

    // The cursor must be within the visible screen (not off-screen).
    let (cursor_row, _) = s.cursor_cell().expect("software cursor visible after 30j");
    assert!(
        cursor_row < 24,
        "cursor row {cursor_row} is off-screen after 30j"
    );
}

/// Regression for 1cead4e / 23cb46b (viewport bottom scroll variant).
///
/// Open a 120-line file and send `G` (jump to last line). "line120" must be
/// visible somewhere on screen.
#[test]
fn g_to_bottom_scrolls_viewport() {
    let mut s = TerminalSession::spawn_with_file(&fixture("lines_120.txt"));

    s.keys("G");

    // "line120" must be visible somewhere on screen.
    let visible = any_line_contains(&s, "line120");
    assert!(
        visible,
        "after G: \"line120\" not visible on any screen row — viewport didn't scroll to bottom"
    );

    // Cursor must be on a visible row.
    let (cursor_row, _) = s.cursor_cell().expect("software cursor visible after G");
    assert!(
        cursor_row < 24,
        "cursor row {cursor_row} is off-screen after G"
    );
}
