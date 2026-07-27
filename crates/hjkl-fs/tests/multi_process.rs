//! Cross-process locking, exercised with real processes.
//!
//! The in-process wait set is unit-tested next to the code; this file covers the
//! half that unit tests structurally cannot — two *separate* processes
//! contending, which is the case that matters when a user has two hjkl instances
//! open on one machine.
//!
//! The workload is deliberately a read-modify-write on a shared counter file,
//! because that is the exact shape of hjkl's cursor index (`load` → `upsert` →
//! `save`). An atomic rename alone does not protect it: every child would read
//! the same value, add one, and the last rename would win, so the total would
//! come out far below the number of increments. Holding the lock across the whole
//! sequence is what makes the count exact.
//!
//! Each child is this very test binary re-executed with `HJKL_FS_LOCK_CHILD` set
//! — the standard way to get a second process without shipping a helper binary.

use std::path::{Path, PathBuf};
use std::process::Command;

const CHILD_ENV: &str = "HJKL_FS_LOCK_CHILD";
const TARGET_ENV: &str = "HJKL_FS_LOCK_TARGET";

/// Increments performed by each child. Small enough to stay fast, large enough
/// that an unlocked implementation loses updates essentially every run.
const BUMPS_PER_CHILD: usize = 20;
const CHILDREN: usize = 4;

fn read_counter(path: &Path) -> usize {
    match hjkl_fs::read_to_string_capped(path, 1024) {
        Ok(s) => s.trim().parse().unwrap_or(0),
        // Missing file == 0; the first child to run creates it.
        Err(_) => 0,
    }
}

fn write_counter(path: &Path, value: usize) {
    hjkl_fs::write_atomic(
        path,
        value.to_string().as_bytes(),
        &hjkl_fs::WriteOptions::state(),
    )
    .expect("write counter");
}

/// One child's work: `BUMPS_PER_CHILD` locked read-modify-write cycles.
fn run_as_child(target: &Path) {
    for _ in 0..BUMPS_PER_CHILD {
        hjkl_fs::with_lock_exclusive(target, || {
            let current = read_counter(target);
            // Widen the window between read and write. Without the lock this
            // makes lost updates near-certain; with it, it changes nothing.
            std::thread::sleep(std::time::Duration::from_millis(1));
            write_counter(target, current + 1);
            Ok(())
        })
        .expect("locked increment");
    }
}

#[test]
fn concurrent_processes_do_not_lose_updates() {
    // Child mode: do the work and exit before any assertions run.
    if let Ok(target) = std::env::var(TARGET_ENV)
        && std::env::var(CHILD_ENV).is_ok()
    {
        run_as_child(Path::new(&target));
        return;
    }

    let td = tempfile::tempdir().expect("tempdir");
    let target: PathBuf = td.path().join("counter.bin");
    let exe = std::env::current_exe().expect("current_exe");

    let mut children = Vec::new();
    for _ in 0..CHILDREN {
        let child = Command::new(&exe)
            // Run only this test in the child, and let it print nothing.
            .arg("concurrent_processes_do_not_lose_updates")
            .arg("--exact")
            .arg("--quiet")
            .env(CHILD_ENV, "1")
            .env(TARGET_ENV, &target)
            .spawn()
            .expect("spawn child");
        children.push(child);
    }

    for mut child in children {
        let status = child.wait().expect("wait child");
        assert!(status.success(), "child failed: {status:?}");
    }

    let total = read_counter(&target);
    assert_eq!(
        total,
        CHILDREN * BUMPS_PER_CHILD,
        "lost updates across processes: expected {}, got {total}",
        CHILDREN * BUMPS_PER_CHILD
    );
}

/// A shared lock guard must release cleanly and be re-acquirable.
///
/// This deliberately does NOT test that several processes hold a shared lock
/// at once — the in-process layer serializes readers by design, so a
/// single-process test structurally cannot demonstrate it. Cross-process
/// sharing needs the spawned-children harness the other tests in this file
/// use; until then this covers the release/re-acquire path only.
#[test]
fn shared_lock_guard_releases_and_reacquires() {
    let td = tempfile::tempdir().expect("tempdir");
    let target = td.path().join("readable.bin");
    write_counter(&target, 7);

    let first = hjkl_fs::lock::lock_shared(&hjkl_fs::lock_path_for(&target)).expect("first shared");
    // Same process, so the in-process layer serializes: prove the guard is
    // released and re-acquirable rather than deadlocking.
    drop(first);
    let second =
        hjkl_fs::lock::lock_shared(&hjkl_fs::lock_path_for(&target)).expect("second shared");
    drop(second);

    assert_eq!(read_counter(&target), 7);
}
