//! Stale-swap crash-recovery prompt end-to-end tests (audit A6).
//!
//! Drives the real `hjkl` binary under a pty against a single-file launch
//! (`hjkl foo.txt`) with a newer-than-disk swap file waiting, so the
//! crash-recovery prompt appears on startup. Presses `q` (abort the open) and
//! asserts the abort is visible and the buffer actually resets — the bug was
//! that on the common `slots.len() == 1` launch, `q` silently dismissed the
//! prompt with no feedback and no effect, leaving the on-disk content
//! displayed as if nothing had happened.

use super::harness::TerminalSession;

/// FNV-1a-64 over `bytes`, reimplemented here (rather than depending on the
/// private `hjkl_app::swap::fnv1a64`) to compute the exact swap filename the
/// spawned process will look up: `swap_dir()/<hash16>.swp`. Must stay in sync
/// with the algorithm documented on `hjkl_app::swap` (build-stable, no
/// randomisation) — see `crates/hjkl-app/src/swap.rs`.
fn fnv1a64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;
    let mut h = FNV_OFFSET;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Poll the whole screen (not just row 0) for `needle`, up to `timeout_ms`.
/// Toast messages float top-right rather than pinning to a fixed row, so
/// scanning every row is the robust way to check one appeared.
fn wait_for_text_anywhere(s: &TerminalSession, needle: &str, timeout_ms: u64) -> bool {
    let steps = (timeout_ms / 20).max(1);
    for _ in 0..steps {
        for row in 0..24u16 {
            if s.line(row).contains(needle) {
                return true;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

/// Pressing `q` at the recovery prompt on a single-file launch (the sole
/// slot) must show an abort message and reset the buffer to an empty
/// scratch — not silently dismiss the prompt while leaving the on-disk
/// content displayed untouched.
#[test]
fn recovery_q_on_sole_slot_aborts_and_resets_buffer() {
    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("crashed.txt");
    std::fs::write(&file_path, "on-disk-content-line\n").unwrap();
    let canonical = std::fs::canonicalize(&file_path).unwrap();

    let file_mtime_ms = std::fs::metadata(&file_path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // Build the XDG_CACHE_HOME the spawned process will use, and pre-seed a
    // swap file at the exact path it'll look up: <cache>/hjkl/swap/<hash>.swp
    // (see hjkl_app::swap::swap_dir / swap_path_for).
    let cache_dir = tempfile::tempdir().unwrap();
    let swap_dir = cache_dir.path().join("hjkl").join("swap");
    std::fs::create_dir_all(&swap_dir).unwrap();
    let hash = fnv1a64(canonical.to_string_lossy().as_bytes());
    let swap_path = swap_dir.join(format!("{hash:016x}.swp"));

    let header = hjkl_app::swap::SwapHeader {
        magic: hjkl_app::swap::SwapHeader::MAGIC,
        version: hjkl_app::swap::SwapHeader::VERSION,
        canonical_path: canonical.to_string_lossy().into_owned(),
        file_mtime_unix_ms: file_mtime_ms,
        // Newer than the on-disk mtime → triggers the recovery prompt.
        write_time_unix_ms: file_mtime_ms + 10_000,
        cursor: (0, 0),
        // Almost certainly dead — must NOT trip the live-writer-pid lock
        // (which would open the file read-only via E325 instead of prompting).
        writer_pid: 999_999_999,
    };
    let rope = ropey::Rope::from_str("unsaved-swap-body-must-not-survive-abort\n");
    hjkl_app::swap::write_swap(&swap_path, &header, &rope).unwrap();

    let mut s = TerminalSession::spawn_with_file_and_cache_dir(&file_path, cache_dir);

    // The recovery prompt (see render.rs's "E325: swap file found ...
    // Recover? [y/N/q]") must appear on startup.
    assert!(
        wait_for_text_anywhere(&s, "Recover? [y/N/q]", 2000),
        "recovery prompt must appear on startup; screen:\n{}",
        (0..24).map(|r| s.line(r)).collect::<Vec<_>>().join("\n")
    );

    // Press 'q' to abort.
    s.keys("q");

    // (a) An abort message must be visible.
    assert!(
        wait_for_text_anywhere(&s, "Aborted file open", 2000),
        "'q' must show an abort message on the sole-slot path; screen:\n{}",
        (0..24).map(|r| s.line(r)).collect::<Vec<_>>().join("\n")
    );

    // (b) The buffer must be the aborted empty-scratch state, NOT the
    // silently-opened on-disk content and NOT the swap body.
    let screen_text = (0..24).map(|r| s.line(r)).collect::<Vec<_>>().join("\n");
    assert!(
        !screen_text.contains("on-disk-content-line"),
        "aborted buffer must not silently show the on-disk content; screen:\n{screen_text}"
    );
    assert!(
        !screen_text.contains("unsaved-swap-body-must-not-survive-abort"),
        "aborted buffer must not show the swap body either; screen:\n{screen_text}"
    );
    // The status line reports the fallback scratch buffer's name.
    assert!(
        wait_for_text_anywhere(&s, "[No Name]", 1000),
        "aborted slot must fall back to [No Name]; screen:\n{screen_text}"
    );
}
