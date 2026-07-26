//! `:r` under the confined filesystem policy.
//!
//! # Why this is its own test binary
//!
//! `hjkl_engine::policy::restrict_fs()` is a one-way process-global: once set it
//! can never be cleared. Calling it from a `#[cfg(test)]` module would poison
//! every other test in that binary, since `cargo test` runs a binary's tests as
//! threads of one process. So this file holds **only** policy-active tests, and
//! nothing here may assume the policy is off.
//!
//! What it pins: a lexically-innocent relative path (`escape/secret.txt`, all
//! `Normal` components) that traverses a symlink out of the working directory
//! must be refused. `hjkl_engine::policy::check_fs_path` alone passes it —
//! `hjkl_fs::resolve_under` is what catches it.

#![cfg(unix)]

use hjkl_engine::{DefaultHost, Editor, Options};
use hjkl_ex::{ExEffect, default_registry, try_dispatch};

fn make_editor() -> Editor<hjkl_buffer::View, DefaultHost> {
    let buf = hjkl_buffer::View::from_str("first");
    let host = DefaultHost::new();
    hjkl_vim::vim_editor(buf, host, Options::default())
}

fn buf_lines(editor: &Editor<hjkl_buffer::View, DefaultHost>) -> Vec<String> {
    let rope = editor.buffer().rope();
    (0..rope.len_lines())
        .map(|i| hjkl_buffer::rope_line_str(&rope, i))
        .collect()
}

/// `project/escape` → `../outside`, so `escape/secret.txt` resolves outside the
/// working directory while every component of the *spelling* is `Normal`.
#[test]
fn read_via_symlink_escape_is_refused_and_inside_read_still_works() {
    let td = tempfile::tempdir().unwrap();
    let project = td.path().join("project");
    let outside = td.path().join("outside");
    std::fs::create_dir(&project).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "TOP SECRET\n").unwrap();
    std::fs::write(project.join("inside.txt"), "inside line\n").unwrap();
    std::os::unix::fs::symlink("../outside", project.join("escape")).unwrap();

    std::env::set_current_dir(&project).unwrap();
    // One-way and process-global — see the module docs.
    hjkl_engine::policy::restrict_fs();
    assert!(hjkl_engine::policy::fs_restricted());

    let reg = default_registry::<DefaultHost>();

    // Negative: the symlink escape must produce an error, never file content.
    let mut ed = make_editor();
    let result = try_dispatch(&reg, &mut ed, "r escape/secret.txt");
    assert!(
        matches!(result, Some(ExEffect::Error(_))),
        ":r through a symlink out of cwd must be refused, got: {result:?}"
    );
    let lines = buf_lines(&ed);
    assert!(
        !lines.iter().any(|l| l.contains("TOP SECRET")),
        "confined-away content leaked into the buffer: {lines:?}"
    );

    // Positive: a normal file inside the working directory still reads with the
    // policy on — the fix must not turn confinement into a blanket refusal.
    let mut ed = make_editor();
    let result = try_dispatch(&reg, &mut ed, "r inside.txt");
    assert_eq!(
        result,
        Some(ExEffect::Ok),
        ":r of a file inside cwd must still succeed under the policy"
    );
    let lines = buf_lines(&ed);
    assert!(
        lines.contains(&"inside line".to_string()),
        "expected inserted content, got: {lines:?}"
    );
}
