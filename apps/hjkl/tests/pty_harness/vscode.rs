//! e2e: VSCode keybinding mode (`--keybindings vscode`).
//!
//! Spawns hjkl with `--keybindings vscode` on a temp file, types text, saves
//! with Ctrl+S, and asserts both the on-disk result and the status badge.

use super::harness::TerminalSession;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Create a writable temp file seeded with `content`.
fn seed(content: &str) -> (tempfile::NamedTempFile, PathBuf) {
    let mut f = tempfile::Builder::new()
        .suffix(".txt")
        .tempfile()
        .expect("create temp file");
    f.write_all(content.as_bytes()).expect("seed temp file");
    f.flush().expect("flush temp file");
    let path = f.path().to_owned();
    (f, path)
}

/// Poll the file until its content equals `want` (or 2 s elapses).
fn wait_for_contents(path: &Path, want: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut last = String::new();
    while Instant::now() < deadline {
        last = std::fs::read_to_string(path).unwrap_or_default();
        if last == want {
            return last;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    last
}

/// Spawn hjkl on an empty temp file in VSCode mode, type text, Ctrl+S to save.
/// Assert on-disk content = "hello\nworld\n" and status badge = "EDITOR".
#[test]
fn vscode_type_and_save_with_ctrl_s() {
    let (_keep, path) = seed("");
    let mut s = TerminalSession::spawn_with_file_and_args(&path, &["--keybindings", "vscode"]);

    // Status badge should read "EDITOR" immediately (non-modal mode).
    // Row 23 is the status line on a 24-row terminal.
    assert!(
        s.wait_for_line(23, "EDITOR", 2000),
        "status badge should be EDITOR; got: {:?}",
        s.line(23)
    );

    // Type "hello", Enter, "world" — each character is typed directly
    // (no `i` needed; VSCode mode is always in insert mode).
    s.keys("hello");
    s.keys("<Enter>");
    s.keys("world");

    // Ctrl+S saves.
    s.keys("<C-s>");

    let got = wait_for_contents(&path, "hello\nworld\n");
    assert_eq!(
        got, "hello\nworld\n",
        "on-disk content after Ctrl+S in VSCode mode"
    );

    // Confirm status still shows "EDITOR" (not "INSERT", "NORMAL", etc.).
    assert!(
        s.line(23).contains("EDITOR"),
        "status badge should still be EDITOR after typing; got: {:?}",
        s.line(23)
    );
}

/// Undo (Ctrl+Z) in VSCode mode reverts a continuous insert session.
///
/// Typing "hello" enters a single insert session; Ctrl+Z undoes it in full
/// (mirrors vim's insert-mode undo granularity — one `u` undoes the whole
/// session). After undo the buffer is empty again; Ctrl+S saves that.
#[test]
fn vscode_ctrl_z_undo() {
    let (_keep, path) = seed("");
    let mut s = TerminalSession::spawn_with_file_and_args(&path, &["--keybindings", "vscode"]);

    // Wait for EDITOR badge.
    assert!(s.wait_for_line(23, "EDITOR", 2000), "status badge EDITOR");

    // Type "hello", then undo — the whole insert session reverts.
    s.keys("hello");
    s.keys("<C-z>"); // undo: reverts the whole "hello" insert session
    s.keys("<C-s>"); // save the empty buffer

    // The buffer should be empty again (undo reverted the full insert).
    let got = wait_for_contents(&path, "");
    assert!(
        got.is_empty(),
        "after Ctrl+Z undo of 'hello', disk should be empty; got: {got:?}"
    );
}
