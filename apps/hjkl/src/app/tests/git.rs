
/// Capture `git diff --cached` for `name` (what's staged in the index).
fn staged_diff(dir: &Path, name: &str) -> String {
    let out = Command::new("git")
        .args(["diff", "--cached", "--", name])
        .current_dir(dir)
        .output()
        .expect("git diff --cached");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `:GitStage` stages the hunk under the cursor into the index.
#[test]
fn git_stage_command_stages_hunk_under_cursor() {
    let tmp = tempfile::TempDir::new().unwrap();
    let f = make_repo_with_committed_file(tmp.path(), "s.txt", "a\nb\nc\n");
    // Edit on disk so the loaded buffer differs from HEAD but is NOT dirty.
    std::fs::write(&f, "a\nB\nc\n").unwrap();
    let mut app = open_app_on(&f);
    app.active_mut().editor.jump_cursor(1, 0); // on the changed row

    app.dispatch_ex("GitStage");

    let staged = staged_diff(tmp.path(), "s.txt");
    assert!(
        staged.contains("-b") && staged.contains("+B"),
        "expected b→B staged; got: {staged}"
    );
}

/// `:GitRevert` discards the hunk under the cursor, restoring HEAD on disk and
/// in the reloaded buffer.
#[test]
fn git_revert_command_restores_hunk() {
    let tmp = tempfile::TempDir::new().unwrap();
    let f = make_repo_with_committed_file(tmp.path(), "r.txt", "a\nb\nc\n");
    std::fs::write(&f, "a\nB\nc\n").unwrap();
    let mut app = open_app_on(&f);
    app.active_mut().editor.jump_cursor(1, 0);

    app.dispatch_ex("GitRevert");

    // Worktree file is back to the committed content.
    let after = std::fs::read_to_string(&f).unwrap();
    assert_eq!(after, "a\nb\nc\n", "revert must restore HEAD content on disk");
}

/// `:GitStage` on a dirty buffer is refused (must save first).
#[test]
fn git_stage_refuses_dirty_buffer() {
    let tmp = tempfile::TempDir::new().unwrap();
    let f = make_repo_with_committed_file(tmp.path(), "d.txt", "a\nb\nc\n");
    let mut app = open_app_on(&f);
    // Make an in-memory edit → dirty, unsaved.
    app.active_mut().dirty = true;
    app.dispatch_ex("GitStage");

    // Nothing staged (buffer was dirty → refused).
    let staged = staged_diff(tmp.path(), "d.txt");
    assert!(staged.is_empty(), "dirty buffer must not stage; got: {staged}");
}

/// `:GitDiff` on a hunk row opens an info popup with the patch.
#[test]
fn git_diff_command_opens_popup() {
    let tmp = tempfile::TempDir::new().unwrap();
    let f = make_repo_with_committed_file(tmp.path(), "p.txt", "a\nb\nc\n");
    std::fs::write(&f, "a\nB\nc\n").unwrap();
    let mut app = open_app_on(&f);
    app.active_mut().editor.jump_cursor(1, 0);

    app.dispatch_ex("GitDiff");
    assert!(
        app.info_popup.is_some(),
        ":GitDiff on a hunk row must open an info popup"
    );
}
