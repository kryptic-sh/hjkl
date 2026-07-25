//! Swap files under real multi-instance contention.
//!
//! Two hjkl instances open on one machine share a swap directory, and — for the
//! same file — one swap path. The atomic temp+rename in [`hjkl_fs`] makes each
//! individual write self-consistent; it does nothing about two instances
//! interleaving a read-decide-write, which is the shape every dangerous swap
//! operation has (recovery reads a swap, decides its writer is dead or its
//! contents stale, and removes it).
//!
//! Threads cannot stand in for this: `flock` is per-open-file-description and
//! the in-process wait set is per-process, so only separate processes exercise
//! the cross-process half. Each child here is this very test binary re-executed
//! with `HJKL_APP_SWAP_CHILD` set — the same trick as
//! `crates/hjkl-fs/tests/multi_process.rs`.
//!
//! The two tests cover the two distinct protections, and each one fails if the
//! lock it covers is removed:
//!
//! 1. [`concurrent_swap_read_decide_write_keeps_every_update`] — the
//!    **sequence** lock a caller holds across read → decide → write (the shape
//!    `check_recovery_on_open` and the orphan-scratch cleanup use). Delete the
//!    `with_lock_exclusive` in `run_counter_child` and updates are lost.
//! 2. [`swap_cannot_change_under_a_held_sequence_lock`] — the **primitive**
//!    locks inside `write_swap_full` / `read_swap_full` / `remove_swap`. The
//!    writer children here take no outer lock at all (exactly what the idle
//!    `write_swap_for_slot` does), so the only thing keeping them out of a
//!    holder's critical section is the lock inside the primitive. Delete the
//!    `with_lock_exclusive` in `swap::write_swap_full` and the swap mutates
//!    under a held lock.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use hjkl_app::swap::{self, SwapHeader};
use ropey::Rope;

const CHILD_ENV: &str = "HJKL_APP_SWAP_CHILD";
const TARGET_ENV: &str = "HJKL_APP_SWAP_TARGET";

/// A header whose body is the only payload the tests care about. `writer_pid`
/// is this process so nothing here ever looks like a crashed session.
fn header_now() -> SwapHeader {
    SwapHeader {
        magic: SwapHeader::MAGIC,
        version: SwapHeader::VERSION,
        canonical_path: "/tmp/multi-instance.rs".to_string(),
        file_mtime_unix_ms: 0,
        write_time_unix_ms: swap::now_unix_ms(),
        cursor: (0, 0),
        writer_pid: std::process::id(),
    }
}

/// The swap's body parsed as a number; a missing or unreadable swap reads as
/// `None` (the first writer creates it).
fn read_value(path: &Path) -> Option<u64> {
    let (_header, body) = swap::read_swap(path).ok()?;
    body.trim().parse().ok()
}

fn write_value(path: &Path, value: u64) {
    swap::write_swap(path, &header_now(), &Rope::from_str(&value.to_string())).expect("write swap");
}

/// Spawn `count` copies of this binary running only `test_name`, in child mode.
fn spawn_children(
    test_name: &str,
    target: &Path,
    count: usize,
    role: &str,
) -> Vec<std::process::Child> {
    let exe = std::env::current_exe().expect("current_exe");
    (0..count)
        .map(|_| {
            Command::new(&exe)
                .arg(test_name)
                .arg("--exact")
                .arg("--quiet")
                .env(CHILD_ENV, role)
                .env(TARGET_ENV, target)
                .spawn()
                .expect("spawn child")
        })
        .collect()
}

/// `Some(target)` when this process is a child playing `role`.
fn child_target(role: &str) -> Option<PathBuf> {
    let target = std::env::var(TARGET_ENV).ok()?;
    (std::env::var(CHILD_ENV).ok()? == role).then(|| PathBuf::from(target))
}

// ── 1. read → decide → write must not lose updates ───────────────────────────

const BUMPS_PER_CHILD: u64 = 15;
const COUNTER_CHILDREN: usize = 4;

/// One child's work: `BUMPS_PER_CHILD` read-decide-write cycles on the shared
/// swap, each under one exclusive lock spanning the whole sequence — the shape
/// `check_recovery_on_open_inner` and `recover_orphan_scratch_buffers_from` now
/// use.
fn run_counter_child(target: &Path) {
    for _ in 0..BUMPS_PER_CHILD {
        hjkl_fs::with_lock_exclusive(target, || {
            let current = read_value(target).unwrap_or(0);
            // Widen the read→write window. Unlocked this makes lost updates
            // near-certain; locked it changes nothing but the runtime.
            std::thread::sleep(Duration::from_millis(1));
            write_value(target, current + 1);
            Ok(())
        })
        .expect("locked swap increment");
    }
}

#[test]
fn concurrent_swap_read_decide_write_keeps_every_update() {
    if let Some(target) = child_target("counter") {
        run_counter_child(&target);
        return;
    }

    let td = tempfile::tempdir().expect("tempdir");
    let target = td.path().join("shared.swp");

    let children = spawn_children(
        "concurrent_swap_read_decide_write_keeps_every_update",
        &target,
        COUNTER_CHILDREN,
        "counter",
    );
    for mut child in children {
        let status = child.wait().expect("wait child");
        assert!(status.success(), "child failed: {status:?}");
    }

    let total = read_value(&target).expect("swap must exist after the children ran");
    assert_eq!(
        total,
        COUNTER_CHILDREN as u64 * BUMPS_PER_CHILD,
        "lost swap updates across processes: expected {}, got {total}",
        COUNTER_CHILDREN as u64 * BUMPS_PER_CHILD
    );
}

// ── 2. a held lock must freeze the swap, even against unlocked writers ───────

const WRITER_CHILDREN: usize = 2;
const WRITES_PER_CHILD: u64 = 120;

/// One writer child: hammer the swap through `write_swap_full` with NO outer
/// lock, the way the idle swap writer does. The lock inside the primitive is the
/// only thing that can keep these writes out of another process's critical
/// section.
fn run_writer_child(target: &Path) {
    for n in 1..=WRITES_PER_CHILD {
        write_value(target, n);
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn swap_cannot_change_under_a_held_sequence_lock() {
    if let Some(target) = child_target("writer") {
        run_writer_child(&target);
        return;
    }

    let td = tempfile::tempdir().expect("tempdir");
    let target = td.path().join("contended.swp");

    let mut children = spawn_children(
        "swap_cannot_change_under_a_held_sequence_lock",
        &target,
        WRITER_CHILDREN,
        "writer",
    );

    // Keep cycling for as long as any writer is alive, so the two overlap for
    // the children's whole run rather than by luck of scheduling.
    let mut cycles = 0usize;
    loop {
        hjkl_fs::with_lock_exclusive(&target, || {
            // Read → (window) → re-read. Under a correctly held exclusive lock
            // these two reads MUST agree: no other process may write in
            // between. This is the invariant the recovery path depends on when
            // it decides from `header` and then removes the file.
            let first = read_value(&target);
            std::thread::sleep(Duration::from_millis(3));
            let second = read_value(&target);
            assert_eq!(
                first, second,
                "swap changed under a held exclusive lock — a concurrent \
                 write_swap_full entered the critical section"
            );

            // Act on the decision, the way recovery does. Removing a swap that
            // another process wrote after our read is exactly the data loss
            // this locking exists to prevent.
            if first.is_some() {
                swap::remove_swap(&target)?;
                assert!(
                    !target.exists(),
                    "swap reappeared inside the lock that just removed it"
                );
            }
            Ok(())
        })
        .expect("locked swap sequence");
        cycles += 1;

        let mut any_alive = false;
        for child in &mut children {
            match child.try_wait().expect("try_wait") {
                Some(status) => assert!(status.success(), "writer child failed: {status:?}"),
                None => any_alive = true,
            }
        }
        if !any_alive {
            break;
        }
    }

    // Guard against a vacuous pass: if the children had already exited before
    // the first cycle, nothing was ever contended.
    assert!(
        cycles >= 5,
        "too few contended cycles ({cycles}) to prove anything"
    );
}
