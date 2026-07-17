//! `--clean` end-to-end tests: drive the real `hjkl` binary under a pty and
//! prove that `--clean` starts from the bundled defaults, ignoring a user
//! `config.toml` present at the session's XDG path AND declining to write
//! runtime state back to it — the terminal twin of `main.rs`'s
//! `parse_argv_clean_flag` parse test.

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

/// A seeded `[explorer] open = true` config makes hjkl restore the left
/// explorer dock on startup (`restore_dock_state_from_config`), which lists
/// the cwd's files. The uniquely-named marker file appears in the tree rows
/// (1..22, excluding the top bar and the status line, which shows the open
/// filename regardless) ONLY when the dock is restored.
///
/// - The control session (no `--clean`) reads the seeded config → dock opens
///   → the marker shows in the tree.
/// - The `--clean` session ignores the seeded config → dock stays closed →
///   the marker never appears in the tree rows.
///
/// Same file, same seed, opposite outcomes: the only difference is the flag,
/// so this pins that `--clean` actually bypasses the on-disk user config.
#[test]
fn clean_ignores_seeded_explorer_open_config() {
    const SEED: &str = "[explorer]\nopen = true\n";
    // A name that cannot occur in the buffer body or chrome — so finding it in
    // the tree rows is unambiguous evidence the explorer listing is present.
    let marker = "zz_clean_marker.txt";

    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join(marker);
    std::fs::write(&file, "hi\n").unwrap();

    // Control: no --clean → seeded config is read → explorer restored.
    let control = TerminalSession::spawn_in_dir_with_file_config_args(tmp.path(), &file, SEED, &[]);
    let control_shows_tree = wait_until(|| (1..22).any(|r| control.line(r).contains(marker)));
    drop(control);
    assert!(
        control_shows_tree,
        "control (no --clean) must honor the seeded explorer.open=true and \
         list {marker} in the explorer tree — otherwise the seed itself is \
         ineffective and the --clean assertion below proves nothing"
    );

    // --clean: seeded config ignored → explorer stays closed.
    let clean =
        TerminalSession::spawn_in_dir_with_file_config_args(tmp.path(), &file, SEED, &["--clean"]);
    // Give the frame time to settle, then confirm the tree listing is absent
    // across the whole settle window (a wait_until for its ABSENCE).
    let clean_hides_tree = wait_until(|| !(1..22).any(|r| clean.line(r).contains(marker)));
    drop(clean);
    assert!(
        clean_hides_tree,
        "--clean must ignore the seeded explorer.open=true; the explorer \
         must NOT be restored and {marker} must not appear in the tree rows"
    );
}

/// Under `--clean`, opening + widening the explorer (which normally persists
/// `explorer.width` / `explorer.open` back to the config file) must NOT write
/// anything to disk: a clean session leaves the user's config untouched. The
/// seeded config file is the only file at the XDG path; after the resize it
/// must still contain exactly the seed, with no written-back keys.
#[test]
fn clean_does_not_persist_runtime_state_to_config() {
    const SEED: &str = "[explorer]\nopen = true\n";
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("hello.txt");
    std::fs::write(&file, "hi\n").unwrap();

    let mut session =
        TerminalSession::spawn_in_dir_with_file_config_args(tmp.path(), &file, SEED, &["--clean"]);
    let cfg_path = session.config_file_path();

    // Open the explorer and widen it twice — in a NON-clean session this
    // persists `width = 38` and `open = true` (see the explorer resize e2e
    // test). Under --clean the write-back path is disarmed (config_path unset
    // in main.rs), so nothing should change on disk.
    session.keys(" e");
    session.keys("<C-w>><C-w>>");
    // Let any (erroneous) write-back attempt have its chance to land.
    std::thread::sleep(Duration::from_millis(400));

    let text = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    drop(session);

    assert_eq!(
        text, SEED,
        "--clean must not write runtime state back to the config file; \
         it should still contain exactly the seed, got:\n{text}"
    );
    assert!(
        !text.contains("width = 38"),
        "--clean must not persist the resized explorer width"
    );
}
