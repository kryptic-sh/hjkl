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
//!
//! # Why the cwd is under a lock
//!
//! Both tests here need the **process** working directory, and neither can be
//! rewritten to take a path instead: the confinement root is not a parameter,
//! it is whatever `std::env::current_dir()` says at the moment `:r` runs (see
//! the `fs_restricted()` arm in `hjkl_ex::builtins`), and `:cd`'s whole
//! contract is that it must not move that directory. Being separate `#[test]`
//! functions in one binary, they run as threads of one process under
//! `cargo test` and so trade working directories. Measured on 2026-09-16 before
//! this guard existed: 38 of 50 consecutive
//! `cargo test -p hjkl-ex --test fs_policy` runs failed, with `:r inside.txt`
//! answering `NotFound` because the other test had moved the cwd, or `:cd`'s
//! before/after compare failing for the same reason. (Under `cargo nextest`,
//! which is what CI runs, each test is its own process and the lock is
//! uncontended.)

#![cfg(unix)]

use hjkl_engine::{DefaultHost, Editor, Options};
use hjkl_ex::{ExEffect, default_registry, try_dispatch};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Serializes the cwd-dependent tests in this binary and restores the previous
/// working directory on drop. Hold it for the whole test body.
///
/// Mirrors `apps/hjkl`'s `CwdGuard` (`src/test_cwd.rs`), which cannot be reused
/// here: it is `pub(crate)` inside a binary crate.
struct CwdGuard {
    _lock: MutexGuard<'static, ()>,
    prev: PathBuf,
}

impl CwdGuard {
    fn enter(dir: &Path) -> Self {
        static SERIAL_LOCK: Mutex<()> = Mutex::new(());
        // A panicking test poisons the mutex, but the only invariant it guards
        // is "one cwd mutation at a time", which restore-on-drop re-establishes
        // either way.
        let lock = SERIAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::current_dir().expect("read current dir");
        std::env::set_current_dir(dir).expect("set current dir");
        Self { _lock: lock, prev }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.prev);
    }
}

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

    let _cwd = CwdGuard::enter(&project);
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

/// `:cd` is refused outright while the filesystem policy is active: it would
/// rewrite the process working directory, which is the confinement root the
/// policy resolves against — a client could `:cd /` and make every later
/// `:w`/`:e` escape the original subtree. The cwd must be left unchanged.
#[test]
fn cd_is_refused_under_policy() {
    let td = tempfile::tempdir().unwrap();
    let _cwd = CwdGuard::enter(td.path());
    // Idempotent — makes this test order-independent within the binary.
    hjkl_engine::policy::restrict_fs();
    assert!(hjkl_engine::policy::fs_restricted());
    let before = std::env::current_dir().unwrap();

    let reg = default_registry::<DefaultHost>();
    let mut ed = make_editor();
    let result = try_dispatch(&reg, &mut ed, "cd /");
    assert!(
        matches!(result, Some(ExEffect::Error(_))),
        ":cd under the fs policy must be refused, got: {result:?}"
    );
    let after = std::env::current_dir().unwrap();
    assert_eq!(
        before, after,
        ":cd under the fs policy must not change the working directory"
    );
}
