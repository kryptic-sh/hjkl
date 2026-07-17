//! `-n` (no swap file) end-to-end tests.
//!
//! `-n` mirrors `vim -n` / `nvim -n`: swap-file writes are disabled for the
//! whole session, so a crash leaves nothing to recover. Drives the real
//! `hjkl` binary under a pty with an inspectable, session-isolated
//! `XDG_CACHE_HOME` and asserts no `.swp` file ever lands under
//! `<cache_dir>/hjkl/swap/` — neither the immediate on-open arm (the
//! PID-lock swap `App::new` writes before `main` applies `-n`, then removes)
//! nor the idle-write sweep after an edit. A control run without `-n`
//! proves the opposite: the same edit DOES produce a `.swp` file, so the
//! `-n` assertion isn't vacuously true.

use super::harness::TerminalSession;
use std::path::Path;

/// `true` if `<cache_dir>/hjkl/swap/` contains any `*.swp` file. The
/// directory itself may exist (created, then emptied, by `-n`'s startup
/// cleanup) without containing anything — that still counts as "no swap".
fn swap_dir_has_swp_file(cache_dir: &Path) -> bool {
    let dir = cache_dir.join("hjkl").join("swap");
    match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("swp")),
        Err(_) => false, // Directory never created — definitely no swap.
    }
}

/// `-n` on the CLI: opening a file, editing it, and waiting past the (sped
/// up) idle-write deadline must leave no `.swp` file behind at all — not the
/// startup PID-lock arm, not the idle sweep.
#[test]
fn dash_n_writes_no_swap_file() {
    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("noswap.txt");
    std::fs::write(&file_path, "hello\n").unwrap();

    let cache_dir = tempfile::tempdir().unwrap();
    let cache_path = cache_dir.path().to_path_buf();

    let mut s = TerminalSession::spawn_with_file_cache_dir_and_args(
        &file_path,
        cache_dir,
        // Lower `updatetime` so the idle swap-write sweep (normally a 4s
        // deadline) would fire well within the test's wait below if it were
        // ever going to run.
        &["-n", "-c", "set updatetime=50"],
    );

    s.keys("ix<Esc>");
    // Give the (would-be) idle sweep time to fire past the lowered deadline.
    std::thread::sleep(std::time::Duration::from_millis(500));

    assert!(
        !swap_dir_has_swp_file(&cache_path),
        "-n must leave no .swp file under {}",
        cache_path.join("hjkl").join("swap").display()
    );
}

/// Control: the same file + edit sequence WITHOUT `-n` produces a `.swp`
/// file, proving the assertion above isn't vacuously true (e.g. from a
/// harness/env misconfiguration that suppresses swap writes for everyone).
#[test]
fn control_without_dash_n_writes_swap_file() {
    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("withswap.txt");
    std::fs::write(&file_path, "hello\n").unwrap();

    let cache_dir = tempfile::tempdir().unwrap();
    let cache_path = cache_dir.path().to_path_buf();

    let mut s = TerminalSession::spawn_with_file_cache_dir_and_args(
        &file_path,
        cache_dir,
        &["-c", "set updatetime=50"],
    );

    s.keys("ix<Esc>");
    std::thread::sleep(std::time::Duration::from_millis(500));

    assert!(
        swap_dir_has_swp_file(&cache_path),
        "control run without -n must produce a .swp file under {}",
        cache_path.join("hjkl").join("swap").display()
    );
}
