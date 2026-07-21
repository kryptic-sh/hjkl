//! Cross-session cursor-position memory end-to-end test (issue #295).
//!
//! Drives the real `hjkl` binary under a pty: open a file, move the cursor to
//! a known line/column, `:wq`, then respawn on the SAME file and assert the
//! cursor is restored to that spot. The two spawns share one
//! `XDG_STATE_HOME` (via `spawn_with_file_and_state_home`) so they resolve the
//! same `<state>/hjkl/filestate.bin` store — the redirect that lets the test
//! observe cross-session persistence without touching the real state dir.
//!
//! The assertion reads the statusline's `row:col` ruler (1-based, e.g.
//! `50:6`) rather than the software-cursor cell: the cell scan is unreliable
//! for a scrolled buffer, but the ruler is exactly the cursor position and is
//! always painted.

use super::harness::TerminalSession;

/// Build a 120-line file (`line1`..`line120`) in a fresh tempdir so the test
/// never writes into the tracked repo fixtures (a `:wq` rewrites the file).
fn make_lines_file(td: &std::path::Path) -> std::path::PathBuf {
    let mut content = String::new();
    for i in 1..=120 {
        content.push_str(&format!("line{i}\n"));
    }
    let p = td.join("cursor_restore_target.txt");
    std::fs::write(&p, content).unwrap();
    p
}

/// Poll up to ~2s for the state-store file to appear under `state_home`.
/// The first session writes it on `:w`/exit; ordering the respawn after it
/// exists makes the test deterministic (no fixed sleep).
fn wait_for_store(state_home: &std::path::Path) -> bool {
    let store = state_home.join("hjkl").join("filestate.bin");
    for _ in 0..200 {
        if store.exists() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    store.exists()
}

/// Move to line 50, end of line (`$`), `:wq`; respawn on the same file and the
/// cursor must land back on line 50 at the final column. Line 50 needs the
/// viewport to scroll, so this also proves the restore positions correctly on
/// a scrolled buffer, not just within the first screen.
#[test]
fn cursor_position_restored_across_sessions() {
    let file_dir = tempfile::tempdir().unwrap();
    let file = make_lines_file(file_dir.path());

    // Caller-owned XDG_STATE_HOME shared by BOTH spawns → same cursor store.
    let state_home = tempfile::tempdir().unwrap();

    // ── Session 1: position the cursor, save + quit. ──────────────────────
    {
        let mut s = TerminalSession::spawn_with_file_and_state_home(&file, state_home.path());
        // Jump to line 50, then `$` to the last column. "line50" is 6 chars,
        // so `$` lands on the 6th (1-based) column → ruler shows "50:6".
        s.keys(":50<Enter>");
        s.keys("$");
        assert!(
            s.wait_for_screen_contains("50:6", 2000),
            "session 1 setup: cursor should be at line 50, col 6 (end of \"line50\"); screen:\n{}",
            (0..24).map(|r| s.line(r)).collect::<Vec<_>>().join("\n")
        );
        // Persist happens on `:w` (save) and on exit; `:wq` exercises both.
        s.keys(":wq<Enter>");
        // Keep session 1 alive until the store is on disk, then drop it.
        assert!(
            wait_for_store(state_home.path()),
            "session 1 must write the cursor store on :wq"
        );
    }

    // ── Session 2: reopen the same file, cursor must be restored. ─────────
    let s = TerminalSession::spawn_with_file_and_state_home(&file, state_home.path());
    assert!(
        s.wait_for_screen_contains("50:6", 2000),
        "cursor must be restored to line 50, col 6 on reopen (ruler \"50:6\"); screen:\n{}",
        (0..24).map(|r| s.line(r)).collect::<Vec<_>>().join("\n")
    );
    // And the restored cursor's row must actually show line50's text (not a
    // stale ruler) — the viewport scrolled to bring it into view.
    assert!(
        (0..24).any(|r| s.line(r).contains("line50")),
        "reopened buffer must have scrolled line50 into view; screen:\n{}",
        (0..24).map(|r| s.line(r)).collect::<Vec<_>>().join("\n")
    );
}
