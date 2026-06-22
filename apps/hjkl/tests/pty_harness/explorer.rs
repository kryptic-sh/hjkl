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
#[ignore = "kryptic-sh/hjkl#252: editable-explorer cursor landing not yet routed \
to the per-window editor (#151 Phase D); unit coverage stays green"]
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
#[ignore = "kryptic-sh/hjkl#252: editable-explorer cursor landing not yet routed \
to the per-window editor (#151 Phase D); unit coverage stays green"]
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
