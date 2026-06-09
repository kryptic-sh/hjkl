//! Event-driven autoreload (#242) end-to-end tests.
//!
//! Drive the real `hjkl` binary under a pty, change the open file on disk from
//! the *outside* with NO keypress and NO focus event, and assert the buffer
//! reloads on its own — proving the fs-watch path fires without the old
//! poll-only `:checktime` / focus-regain trigger.
//!
//! The watcher roots at the process cwd, so these spawn with cwd = the fixture
//! dir and open a file inside it (`spawn_in_dir_with_file`).

use super::harness::TerminalSession;

/// External write to the open file reloads the buffer with no input.
#[test]
fn external_write_reloads_without_keypress() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("watched.txt");
    std::fs::write(&file, "before-edit\n").unwrap();

    let s = TerminalSession::spawn_in_dir_with_file(dir.path(), &file);
    assert!(
        s.wait_for_line(0, "before-edit", 2000),
        "initial content should render; row0={:?}",
        s.line(0)
    );

    // Change the file from outside the editor — no keypress, no focus event.
    std::fs::write(&file, "after-external-edit\n").unwrap();

    // fs-watch debounce (~100 ms) + loop poll (≤120 ms) should pull it in.
    assert!(
        s.wait_for_line(0, "after-external-edit", 3000),
        "buffer must auto-reload from the external write without input; row0={:?}",
        s.line(0)
    );
}

/// A clean reload reflects multiple external edits in sequence (the watcher
/// keeps firing, not just once).
#[test]
fn repeated_external_writes_each_reload() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("watched.txt");
    std::fs::write(&file, "v0\n").unwrap();

    let s = TerminalSession::spawn_in_dir_with_file(dir.path(), &file);
    assert!(s.wait_for_line(0, "v0", 2000), "row0={:?}", s.line(0));

    std::fs::write(&file, "v1-external\n").unwrap();
    assert!(
        s.wait_for_line(0, "v1-external", 3000),
        "first reload; row0={:?}",
        s.line(0)
    );

    std::fs::write(&file, "v2-external\n").unwrap();
    assert!(
        s.wait_for_line(0, "v2-external", 3000),
        "second reload; row0={:?}",
        s.line(0)
    );
}

/// With unsaved edits in the buffer, an external write does NOT clobber them —
/// the dirty guard holds even on the event-driven path. The buffer keeps the
/// typed text; vim would require `:e!` to take the disk version.
#[test]
fn external_write_does_not_clobber_dirty_buffer() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("watched.txt");
    std::fs::write(&file, "disk-original\n").unwrap();

    let mut s = TerminalSession::spawn_in_dir_with_file(dir.path(), &file);
    assert!(
        s.wait_for_line(0, "disk-original", 2000),
        "row0={:?}",
        s.line(0)
    );

    // Make an unsaved edit: insert text at the start of line 0.
    s.keys("ITYPED-<Esc>");
    assert!(
        s.wait_for_line(0, "TYPED-", 2000),
        "typed text should show; row0={:?}",
        s.line(0)
    );

    // External write while the buffer is dirty.
    std::fs::write(&file, "disk-changed-underneath\n").unwrap();

    // Give the watch path time to (not) act, then assert the dirty buffer
    // still holds the typed text rather than the disk version.
    assert!(
        !s.wait_for_line(0, "disk-changed-underneath", 1500),
        "dirty buffer must not be auto-clobbered; row0={:?}",
        s.line(0)
    );
    assert!(
        s.line(0).contains("TYPED-"),
        "typed text must survive; row0={:?}",
        s.line(0)
    );
}
