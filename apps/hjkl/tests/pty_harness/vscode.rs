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

// ── V5 selection tests ────────────────────────────────────────────────────────

/// Shift+Left x2 selects "lo" in "hello"; typing "X" replaces the selection →
/// disk content = "helX\n".
#[test]
fn vscode_shift_select_then_type_replaces() {
    let (_keep, path) = seed("");
    let mut s = TerminalSession::spawn_with_file_and_args(&path, &["--keybindings", "vscode"]);
    assert!(s.wait_for_line(23, "EDITOR", 2000), "status badge EDITOR");

    // Type "hello" — caret is now after the 'o' (col 5).
    s.keys("hello");
    // Shift+Left twice: select "lo" (caret moves from col 5 to col 3).
    s.keys("<S-Left>");
    s.keys("<S-Left>");
    // Typing "X" replaces the selection.
    s.keys("X");
    // Save.
    s.keys("<C-s>");

    let got = wait_for_contents(&path, "helX\n");
    assert_eq!(got, "helX\n", "Shift+Left×2 + type 'X' should give 'helX'");
}

/// Shift+Left x2 selects "lo"; Backspace deletes the selection → "hel\n".
#[test]
fn vscode_shift_select_then_backspace_deletes() {
    let (_keep, path) = seed("");
    let mut s = TerminalSession::spawn_with_file_and_args(&path, &["--keybindings", "vscode"]);
    assert!(s.wait_for_line(23, "EDITOR", 2000), "status badge EDITOR");

    s.keys("hello");
    s.keys("<S-Left>");
    s.keys("<S-Left>");
    s.keys("<BS>"); // delete the "lo" selection
    s.keys("<C-s>");

    let got = wait_for_contents(&path, "hel\n");
    assert_eq!(got, "hel\n", "Shift+Left×2 + Backspace should give 'hel'");
}

/// Ctrl+A selects everything; typing "Z" replaces the whole buffer → "Z\n".
#[test]
fn vscode_ctrl_a_then_type_replaces_all() {
    let (_keep, path) = seed("");
    let mut s = TerminalSession::spawn_with_file_and_args(&path, &["--keybindings", "vscode"]);
    assert!(s.wait_for_line(23, "EDITOR", 2000), "status badge EDITOR");

    s.keys("abc");
    s.keys("<C-a>"); // select all ("abc")
    s.keys("Z"); // replace with "Z"
    s.keys("<C-s>");

    let got = wait_for_contents(&path, "Z\n");
    assert_eq!(got, "Z\n", "Ctrl+A + type 'Z' should give 'Z'");
}

// ── V6 clipboard tests ────────────────────────────────────────────────────────
//
// Clipboard testability strategy (CI-safe):
//
// The real `hjkl` binary uses `TuiHost` whose clipboard backend in CI / PTY
// (no X display) either has no READ capability (OSC 52) or fails construction.
// `read_clipboard()` therefore returns `None`, and paste falls back to the
// unnamed register — which the cut/copy step populated in the same process.
//
// All tests below rely on the **register fallback path only**:
//   cut / copy → unnamed register (in-process)
//   paste      → read_clipboard() == None → unnamed register
//
// If the host clipboard *is* readable (local dev box with wl-paste / pbpaste),
// the OS clipboard also holds the text, so `read_clipboard()` returns the same
// string — both paths produce identical results.
//
// Tests that depend on the register fallback are annotated with
// "NOTE: register fallback" below.

/// Ctrl+X removes the selected text from the buffer.
///
/// Type "hello", Shift+Left×2 selects "lo", Ctrl+X deletes → "hel".
#[test]
fn vscode_cut_removes_selection() {
    let (_keep, path) = seed("");
    let mut s = TerminalSession::spawn_with_file_and_args(&path, &["--keybindings", "vscode"]);
    assert!(s.wait_for_line(23, "EDITOR", 2000), "status badge EDITOR");

    s.keys("hello");
    s.keys("<S-Left>");
    s.keys("<S-Left>"); // selects "lo" (caret col 3, anchor col 5)
    s.keys("<C-x>"); // cut "lo" → "hel"
    s.keys("<C-s>"); // save

    let got = wait_for_contents(&path, "hel\n");
    assert_eq!(
        got, "hel\n",
        "Ctrl+X should cut selected 'lo', leaving 'hel'"
    );
}

/// Ctrl+X then Ctrl+V round-trips the cut text (register fallback).
///
/// Type "hello", select "lo", cut → "hel"; paste at caret → "hello".
/// NOTE: register fallback — OS clipboard read returns None in CI/PTY so
/// paste reads the unnamed register, which cut populated.
#[test]
fn vscode_cut_then_paste_roundtrip() {
    let (_keep, path) = seed("");
    let mut s = TerminalSession::spawn_with_file_and_args(&path, &["--keybindings", "vscode"]);
    assert!(s.wait_for_line(23, "EDITOR", 2000), "status badge EDITOR");

    s.keys("hello");
    s.keys("<S-Left>");
    s.keys("<S-Left>"); // selects "lo"
    s.keys("<C-x>"); // cut → buffer is "hel", register/clip = "lo"
    s.keys("<C-v>"); // paste "lo" back → "hello"
    s.keys("<C-s>");

    let got = wait_for_contents(&path, "hello\n");
    assert_eq!(
        got, "hello\n",
        "cut then paste should round-trip: 'hello' → cut 'lo' → paste 'lo' → 'hello'"
    );
}

/// Ctrl+C keeps the buffer intact and the selection active; a subsequent paste
/// appends the copied text.
///
/// Type "hello", select "lo" with Shift+Left×2, Ctrl+C (copies, keeps
/// selection), Right (collapses to end col 5), Ctrl+V inserts "lo" → "hellolo".
/// NOTE: register fallback (same reasoning as cut→paste test above).
#[test]
fn vscode_copy_keeps_buffer_then_paste_appends() {
    let (_keep, path) = seed("");
    let mut s = TerminalSession::spawn_with_file_and_args(&path, &["--keybindings", "vscode"]);
    assert!(s.wait_for_line(23, "EDITOR", 2000), "status badge EDITOR");

    s.keys("hello");
    s.keys("<S-Left>");
    s.keys("<S-Left>"); // selects "lo" (caret col 3, anchor col 5)
    s.keys("<C-c>"); // copy "lo"; buffer unchanged, selection stays
    s.keys("<Right>"); // collapse to end of selection (col 5 = after 'o')
    s.keys("<C-v>"); // paste "lo" at col 5 → "hellolo"
    s.keys("<C-s>");

    let got = wait_for_contents(&path, "hellolo\n");
    assert_eq!(
        got, "hellolo\n",
        "Ctrl+C copy + Right + Ctrl+V should give 'hellolo'"
    );
}

/// Ctrl+V with a selection pastes and replaces the selection.
///
/// Uses two steps to set up the register: type "XY", select "Y", cut → register="Y",
/// type "ab", select "b", paste → replaces "b" with "Y" → "aY".
/// NOTE: register fallback.
#[test]
fn vscode_paste_replaces_selection() {
    let (_keep, path) = seed("");
    let mut s = TerminalSession::spawn_with_file_and_args(&path, &["--keybindings", "vscode"]);
    assert!(s.wait_for_line(23, "EDITOR", 2000), "status badge EDITOR");

    // Phase 1: populate register with "Y" via cut.
    s.keys("XY");
    s.keys("<S-Left>"); // select "Y"
    s.keys("<C-x>"); // cut "Y" → buffer "X", register="Y"
    // Phase 2: type "ab", select "b", paste → "aY".
    s.keys("ab"); // buffer is now "Xab"
    s.keys("<S-Left>"); // select "b"
    s.keys("<C-v>"); // paste "Y", replacing "b" → "XaY"
    s.keys("<C-s>");

    let got = wait_for_contents(&path, "XaY\n");
    assert_eq!(
        got, "XaY\n",
        "Ctrl+V with selection should replace 'b' with 'Y'"
    );
}

/// Plain Left collapses the selection without replacing; typing after collapse
/// inserts at the collapsed position (selection start).
#[test]
fn vscode_plain_left_collapses_without_replace() {
    let (_keep, path) = seed("");
    let mut s = TerminalSession::spawn_with_file_and_args(&path, &["--keybindings", "vscode"]);
    assert!(s.wait_for_line(23, "EDITOR", 2000), "status badge EDITOR");

    s.keys("hello");
    // Select "lo" with Shift+Left twice.
    s.keys("<S-Left>");
    s.keys("<S-Left>");
    // Plain Left: collapse to selection start (col 3, between 'l' and 'l').
    s.keys("<Left>");
    // Typing 'X' now inserts at col 3 (between first 'l' and 'l').
    s.keys("X");
    s.keys("<C-s>");

    // "hello" with X inserted at col 3 (0-indexed) → "helXlo"
    let got = wait_for_contents(&path, "helXlo\n");
    assert_eq!(
        got, "helXlo\n",
        "plain Left after selection collapses then insert at start"
    );
}
