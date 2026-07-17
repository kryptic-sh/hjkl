//! Explorer end-to-end tests: drive the real `hjkl` binary under a pty,
//! operate on the file tree, and assert the resulting filesystem state.

use super::harness::TerminalSession;
use std::time::{Duration, Instant};

/// Poll `pred` until it returns true or ~3s elapses.
fn wait_until(mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    pred()
}

/// `dd` a directory, then `p` on another directory → the directory moves INTO
/// the target, contents preserved. Drives the real binary: `<leader>e` opens
/// the explorer; navigation uses `j`/`G` (in the lazy explorer `/` opens the
/// fuzzy finder, not a buffer search). `dd` cuts, `p` puts.
#[test]
fn dd_dir_then_p_on_dir_moves_into_target() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("mover")).unwrap();
    std::fs::write(tmp.path().join("mover").join("inner.txt"), b"CONTENT").unwrap();
    std::fs::create_dir(tmp.path().join("target")).unwrap();
    std::fs::write(tmp.path().join("target").join("keep.txt"), b"k").unwrap();

    let mut session = TerminalSession::spawn_in_dir(tmp.path());

    // Open the explorer (leader = space). Top level (dirs-first, by name):
    // row 0 = root, row 1 = mover/, row 2 = target/.
    session.keys(" e");
    // Land on `mover/` (row 1) and cut it.
    session.keys("j");
    session.keys("dd");
    // After the move-out, the tree is root + target/; `G` lands on target/.
    session.keys("G");
    session.keys("p");

    let moved = tmp.path().join("target").join("mover").join("inner.txt");
    let root_orig = tmp.path().join("mover");
    let ok = wait_until(|| moved.exists() && !root_orig.exists());

    // Read content before the session drops (kills the process).
    let content = std::fs::read(&moved).ok();
    drop(session);

    assert!(
        ok,
        "expected target/mover/inner.txt to exist and root mover/ gone"
    );
    assert_eq!(
        content.as_deref(),
        Some(b"CONTENT".as_slice()),
        "moved file must preserve its contents"
    );
}

/// `o` then typing a multi-level name (`somedir/test.txt`) then `<Esc>` creates
/// the nested file AND expands the new directory so the leaf is visible.
#[test]
fn o_create_multilevel_expands_new_dir() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("keep.txt"), b"k").unwrap();

    let mut session = TerminalSession::spawn_in_dir(tmp.path());
    session.keys(" e");
    // Open a new line and type a nested path, then leave insert mode.
    session.keys("o");
    session.keys("somedir/test.txt");
    session.keys("<Esc>");

    let created = tmp.path().join("somedir").join("test.txt");
    let ok = wait_until(|| created.exists());

    // The explorer must show the expanded new dir with its child on some row.
    let shows_child = wait_until(|| (0..24).any(|r| session.line(r).contains("test.txt")));
    drop(session);

    assert!(ok, "somedir/test.txt must be created on disk");
    assert!(
        shows_child,
        "the explorer must expand somedir/ and show test.txt"
    );
}

// ── Dock window navigation (#63 Phase C mouse/window audit) ────────────────

/// `<leader>e` opens the explorer as a real left dock; `<C-w>l`/`<C-w>h` must
/// cross between it and the main area exactly like `<C-w>` navigation between
/// two ordinary split windows, and `<leader>e` again closes it — end-to-end
/// twin of the in-process `dock_neighbor_*`/`ctrl_w_*` unit tests in
/// `app/tests/splits_windows.rs`.
#[test]
fn explorer_dock_open_navigate_and_close() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("hello.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();

    let mut session = TerminalSession::spawn_in_dir_with_file(tmp.path(), &file);

    // Baseline: no dock, so the software cursor sits in the main window at
    // its usual gutter-relative column.
    let (_, base_col) = session.cursor_cell_wait();

    session.keys(" e");
    // The explorer opens focused — some row must list hello.txt as a tree
    // entry, and the cursor must land well left of the main window's
    // original column (inside the dock, which starts at col 0).
    let shows_file = wait_until(|| (0..24).any(|r| session.line(r).contains("hello.txt")));
    assert!(shows_file, "explorer dock must list hello.txt");
    let (_, dock_col) = session.cursor_cell_wait();
    assert!(
        dock_col < base_col,
        "cursor must land inside the dock (col {dock_col}), left of the \
         main window's original cursor col {base_col}"
    );

    // `<C-w>l`: leave the dock, re-enter the main area (the last regular
    // window the user focused, or the tree's first leaf — dock.rs's
    // `dock_neighbor_right`). The main window is narrower while the dock is
    // open (its content now starts to the right of the dock's width), so the
    // cursor lands well right of `dock_col` — NOT back at `base_col`, which
    // only applies once the dock is gone and the main window is full-width
    // again (checked below, after close).
    session.keys("<C-w>l");
    let (_, main_col_dock_open) = session.cursor_cell_wait();
    assert!(
        main_col_dock_open > dock_col,
        "C-w l must move the cursor out of the dock into the main area \
         (dock col {dock_col}, main col {main_col_dock_open})"
    );

    // `<C-w>h`: back into the dock, at the same column as before.
    session.keys("<C-w>h");
    let (_, back_col) = session.cursor_cell_wait();
    assert_eq!(
        back_col, dock_col,
        "C-w h must return focus to the dock at the same column"
    );

    // `<C-w>l` once more so the dock is unfocused (but still open) before
    // closing it — closing an unfocused dock must not disturb the main
    // window's focus, only remove the dock pane itself.
    session.keys("<C-w>l");

    // `<leader>e` toggles the dock closed regardless of which window has
    // focus (`toggle_explorer` dispatches on `self.explorer`, not on
    // current focus). The main window reclaims the full frame width, so the
    // cursor returns to exactly its pre-dock column — and the tree listing
    // (rows 1..22, excluding the top bar and the status line, which
    // legitimately shows "hello.txt" as the open filename throughout)
    // disappears from the screen.
    session.keys(" e");
    let closed = wait_until(|| !(1..22).any(|r| session.line(r).contains("hello.txt")));
    assert!(closed, "closing the dock must remove the tree listing");
    let (_, final_col) = session.cursor_cell_wait();
    assert_eq!(
        final_col, base_col,
        "closing the dock must restore the main window to its original \
         (full-width) column"
    );
}

// ── Dock resize persistence (#63 Phase C) ──────────────────────────────────

/// `<C-w>>` widens the explorer dock and writes the new width back to the
/// session's own (isolated, per-spawn — see `TerminalSession::config_dir`)
/// config file. End-to-end twin of the in-process
/// `dock_resize_ctrl_w_gt_persists_width_to_config_file` unit test in
/// `app/tests/splits_windows.rs`, driven through the real key parser instead
/// of `dispatch_action` directly.
#[test]
fn dock_resize_ctrl_w_gt_persists_width_to_real_config_file() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("hello.txt");
    std::fs::write(&file, "hi\n").unwrap();

    let mut session = TerminalSession::spawn_in_dir_with_file(tmp.path(), &file);
    let cfg_path = session.config_file_path();
    assert!(
        !cfg_path.exists(),
        "no config file should exist before any resize/toggle happens"
    );

    // Open the explorer (focuses it) and widen it 2x — bundled default
    // `explorer.width` is 36, so this must land on 38. (The default 80-col
    // terminal's dynamic clamp caps the dock at terminal_width/2 = 40 —
    // `dock::clamp_dock_width` — so this stays well clear of that ceiling
    // rather than exercising it; the clamp itself is covered by
    // `dock_resize_clamps_to_minimum_width` and friends at the unit level.)
    session.keys(" e");
    session.keys("<C-w>><C-w>>");

    let ok = wait_until(|| {
        std::fs::read_to_string(&cfg_path).is_ok_and(|t| t.contains("width = 38"))
    });
    let text = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    assert!(
        ok,
        "2x <C-w>> from width 36 must persist width = 38 to \
         {cfg_path:?}; got:\n{text}"
    );
    assert!(text.contains("[explorer]"));
    // The explorer was opened during this same session (see `toggle_explorer`
    // → `persist_explorer_open`, #63 Phase C), so `open = true` must have
    // landed in the same file alongside the width.
    assert!(
        text.contains("open = true"),
        "opening the explorer this session must also have persisted \
         explorer.open = true; got:\n{text}"
    );
}
