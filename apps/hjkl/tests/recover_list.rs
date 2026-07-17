//! Integration tests for `hjkl -r` (bare) — list swap files and exit.
//!
//! `-r` (bare) is a terminal action (prints to stdout, exits 0) that never
//! touches the TUI, so a plain `std::process::Command` subprocess is enough
//! — no pty required (contrast with `tests/pty_harness/recovery.rs`, which
//! covers the `-r <FILE>` recovery-prompt path and does need a pty).
//!
//! `hjkl_app` is a regular (non-dev) dependency of the `hjkl` package, so
//! its `swap` module is reachable directly from this integration test target
//! for seeding — same pattern `tests/pty_harness/recovery.rs` already uses.

use std::process::Command;

fn hjkl() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hjkl"))
}

/// No swap directory / no swap files at all: `-r` must print the vim-style
/// "no swaps" line and still exit 0.
#[test]
fn dash_r_bare_prints_no_swaps_when_none_exist() {
    let cache_dir = tempfile::tempdir().unwrap();

    let output = hjkl()
        .env("XDG_CACHE_HOME", cache_dir.path())
        .arg("-r")
        .output()
        .expect("hjkl binary");

    assert!(output.status.success(), "exit code: {}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No swap files found"),
        "expected the no-swaps line; got: {stdout:?}"
    );
}

/// A seeded swap file must show up in the `-r` listing: its swap path, the
/// edited file's canonical path, and exit 0. No TUI is spawned (this is a
/// plain `output()` call, not a pty session) — proof the flag exits before
/// entering alternate screen.
#[test]
fn dash_r_bare_lists_seeded_swap() {
    let edited_dir = tempfile::tempdir().unwrap();
    let edited_path = edited_dir.path().join("crashed.txt");
    std::fs::write(&edited_path, "on-disk content\n").unwrap();
    let canonical = std::fs::canonicalize(&edited_path).unwrap();

    let cache_dir = tempfile::tempdir().unwrap();
    let swap_dir = cache_dir.path().join("hjkl").join("swap");
    std::fs::create_dir_all(&swap_dir).unwrap();
    let swap_path = swap_dir.join("deadbeefdeadbeef.swp");

    let header = hjkl_app::swap::SwapHeader {
        magic: hjkl_app::swap::SwapHeader::MAGIC,
        version: hjkl_app::swap::SwapHeader::VERSION,
        canonical_path: canonical.to_string_lossy().into_owned(),
        file_mtime_unix_ms: 1_700_000_000_000,
        write_time_unix_ms: 1_700_000_001_000,
        cursor: (0, 0),
        // Almost certainly dead — asserts the "process gone" branch below.
        writer_pid: 999_999_999,
    };
    let rope = ropey::Rope::from_str("unsaved swap body\n");
    hjkl_app::swap::write_swap(&swap_path, &header, &rope).unwrap();

    let output = hjkl()
        .env("XDG_CACHE_HOME", cache_dir.path())
        .arg("-r")
        .output()
        .expect("hjkl binary");

    assert!(output.status.success(), "exit code: {}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&canonical.to_string_lossy().into_owned()),
        "expected the edited file's canonical path in the listing; got: {stdout:?}"
    );
    assert!(
        stdout.contains("deadbeefdeadbeef.swp"),
        "expected the swap file path in the listing; got: {stdout:?}"
    );
    assert!(
        stdout.contains("process gone"),
        "writer_pid 999_999_999 must report as gone; got: {stdout:?}"
    );
    assert!(
        !stdout.contains("No swap files found"),
        "must not print the no-swaps line when a swap exists; got: {stdout:?}"
    );
}
