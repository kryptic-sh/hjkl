//! Cross-session persistent-undo end-to-end test (issue #299).
//!
//! Drives the real `hjkl` binary under a pty for the headline scenario: make 5
//! distinct edits, `u` twice (landing on state 3), `:wq`; then respawn on the
//! SAME file with the SAME `XDG_STATE_HOME` and `<C-r>` twice must walk the
//! retained forward branch back to state 5. A second test proves the restored
//! history also walks BACK: after reopen, `u` several times returns to the
//! original file content.
//!
//! The two spawns share one `XDG_STATE_HOME` (via
//! `spawn_with_file_and_state_home`) so they resolve the same
//! `<state>/hjkl/undo/<hash>.und` undofile — the redirect that lets the test
//! observe cross-session persistence without touching the real state dir.

use super::harness::TerminalSession;

/// Seed a small file whose sole line is `start`, in a fresh tempdir so the test
/// never rewrites tracked repo fixtures (a `:wq` overwrites it).
fn make_seed_file(td: &std::path::Path) -> std::path::PathBuf {
    let p = td.join("undo_restore_target.txt");
    std::fs::write(&p, "start\n").unwrap();
    p
}

/// Poll up to ~2s for an undofile to appear under `<state_home>/hjkl/undo/`.
/// The first session writes it on `:w`; ordering the respawn after it exists
/// makes the test deterministic (no fixed sleep).
fn wait_for_undofile(state_home: &std::path::Path) -> bool {
    let dir = state_home.join("hjkl").join("undo");
    for _ in 0..200 {
        if let Ok(rd) = std::fs::read_dir(&dir)
            && rd
                .filter_map(|e| e.ok())
                .any(|e| e.path().extension().is_some_and(|x| x == "und"))
        {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

/// Make five distinct edits (`o line1` … `o line5`), then `u` twice so the
/// buffer sits on state 3 (`start`, `line1`, `line2`, `line3`). Each `o…<Esc>`
/// is one undoable change, matching vim.
fn make_five_edits_then_undo_twice(s: &mut TerminalSession) {
    for i in 1..=5 {
        s.keys(&format!("oline{i}<Esc>"));
    }
    assert!(
        s.wait_for_screen_contains("line5", 2000),
        "setup: all five edits should be visible; screen:\n{}",
        (0..24).map(|r| s.line(r)).collect::<Vec<_>>().join("\n")
    );
    // Undo twice: drop line5 then line4 → land on state 3.
    s.keys("u");
    s.keys("u");
    assert!(
        s.wait_for_screen_contains("line3", 2000) && !(0..24).any(|r| s.line(r).contains("line5")),
        "setup: after u u the buffer should show state 3 (line3 present, line5 \
         gone); screen:\n{}",
        (0..24).map(|r| s.line(r)).collect::<Vec<_>>().join("\n")
    );
}

/// The acceptance test: 5 edits → `u` twice → `:wq` → reopen → `<C-r>` twice →
/// back on state 5. Reopening restores the exact node the file was saved on and
/// redo walks forward through the retained nodes.
#[test]
fn redo_walks_forward_across_sessions() {
    let file_dir = tempfile::tempdir().unwrap();
    let file = make_seed_file(file_dir.path());
    // Caller-owned XDG_STATE_HOME shared by BOTH spawns → same undofile.
    let state_home = tempfile::tempdir().unwrap();

    // ── Session 1: edit, undo twice, save+quit. ───────────────────────────
    {
        let mut s = TerminalSession::spawn_with_file_and_state_home(&file, state_home.path());
        make_five_edits_then_undo_twice(&mut s);
        s.keys(":wq<Enter>");
        assert!(
            wait_for_undofile(state_home.path()),
            "session 1 must write an undofile on :wq"
        );
    }

    // ── Session 2: reopen; <C-r> twice must reach state 5. ────────────────
    let mut s = TerminalSession::spawn_with_file_and_state_home(&file, state_home.path());
    // The reopened buffer shows the saved state 3.
    assert!(
        s.wait_for_screen_contains("line3", 2000),
        "reopen: buffer should show saved state 3; screen:\n{}",
        (0..24).map(|r| s.line(r)).collect::<Vec<_>>().join("\n")
    );
    assert!(
        !(0..24).any(|r| s.line(r).contains("line4")),
        "reopen: state 4/5 must NOT be visible yet; screen:\n{}",
        (0..24).map(|r| s.line(r)).collect::<Vec<_>>().join("\n")
    );
    // Redo twice: re-apply line4 then line5 from the retained forward branch.
    s.keys("<C-r>");
    s.keys("<C-r>");
    assert!(
        s.wait_for_screen_contains("line5", 2000),
        "redo across sessions must reconstruct state 5 (line5 present); \
         screen:\n{}",
        (0..24).map(|r| s.line(r)).collect::<Vec<_>>().join("\n")
    );
    assert!(
        (0..24).any(|r| s.line(r).contains("line4")),
        "state 4 must also be present after two redos; screen:\n{}",
        (0..24).map(|r| s.line(r)).collect::<Vec<_>>().join("\n")
    );
}

/// After reopening, the restored history also walks BACK: `u` several times
/// returns to the original file content (`start`, with every `lineN` gone).
#[test]
fn undo_walks_back_to_original_across_sessions() {
    let file_dir = tempfile::tempdir().unwrap();
    let file = make_seed_file(file_dir.path());
    let state_home = tempfile::tempdir().unwrap();

    // Session 1: same setup — 5 edits, u u, save.
    {
        let mut s = TerminalSession::spawn_with_file_and_state_home(&file, state_home.path());
        make_five_edits_then_undo_twice(&mut s);
        s.keys(":wq<Enter>");
        assert!(
            wait_for_undofile(state_home.path()),
            "session 1 must write an undofile on :wq"
        );
    }

    // Session 2: reopen on state 3, then `u` three times to reach state 0
    // (`start` only). State 3 has 3 edits above the root, so 3 undos suffice.
    let mut s = TerminalSession::spawn_with_file_and_state_home(&file, state_home.path());
    assert!(
        s.wait_for_screen_contains("line3", 2000),
        "reopen: buffer should show saved state 3; screen:\n{}",
        (0..24).map(|r| s.line(r)).collect::<Vec<_>>().join("\n")
    );
    s.keys("u");
    s.keys("u");
    s.keys("u");
    // Now back at the original single line `start`; no `lineN` remains.
    assert!(
        s.wait_for_screen_contains("start", 2000)
            && !(0..24).any(|r| {
                let l = s.line(r);
                l.contains("line1") || l.contains("line2") || l.contains("line3")
            }),
        "undo across sessions must walk the restored history back to the \
         original content; screen:\n{}",
        (0..24).map(|r| s.line(r)).collect::<Vec<_>>().join("\n")
    );
}
