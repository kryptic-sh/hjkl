//! Multi-instance and multi-thread file locking.
//!
//! Two layers, both required:
//!
//! 1. **Cross-process** — `std::fs::File::lock` (stable since Rust 1.89):
//!    `flock(LOCK_EX)` on Unix, `LockFileEx` on Windows. This is what makes two
//!    concurrently-running hjkl instances safe against each other.
//! 2. **In-process** — a path-keyed wait set in this module. Needed because the
//!    std docs are explicit that taking a second lock through a handle that
//!    already holds one is *unspecified and may deadlock*, and because Unix
//!    `flock` is per-open-file-description: two threads in one process opening
//!    the same path would each get their own description and **not** block each
//!    other. The wait set closes both holes.
//!
//! Locks are taken on a **sidecar** `<path>.lock` file, never on the target
//! itself. Locking the target would mean creating it just to lock it, and the
//! mere existence of a swap or undofile is meaningful to the recovery paths —
//! an empty placeholder would read as "there is a swap here". Sidecars are
//! bounded by the same hashed namespace as the files they guard and are left in
//! place between runs (an empty 0-byte file is cheaper than the churn of
//! creating and deleting one per write).
//!
//! Unix locks are advisory: they coordinate *cooperating* processes and are no
//! defence against a writer that ignores them. Windows locks are mandatory.
//! Everything in hjkl goes through this module, so cooperation is the norm.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};

/// Who holds a path's in-process claim, and how many times over.
#[derive(Debug)]
struct Holder {
    thread: std::thread::ThreadId,
    depth: usize,
    /// True when the outermost acquisition was exclusive. Used to
    /// debug-assert that a shared→exclusive upgrade is never attempted.
    exclusive: bool,
}

/// In-process claims: paths currently locked by *this* process.
///
/// A `Condvar` rather than a per-path `Mutex` because `std::sync::Mutex` has no
/// owned-guard form — parking a `MutexGuard` inside [`FileLock`] would need an
/// unsafe lifetime transmute. Acquire/release around a map is safe and does the
/// same job, and the map is what makes re-entrancy possible.
fn wait_set() -> &'static (Mutex<HashMap<PathBuf, Holder>>, Condvar) {
    static SET: OnceLock<(Mutex<HashMap<PathBuf, Holder>>, Condvar)> = OnceLock::new();
    SET.get_or_init(|| (Mutex::new(HashMap::new()), Condvar::new()))
}

/// Claim `key` for this thread, blocking while another thread holds it.
///
/// Returns `true` when the claim is **re-entrant** — this thread already held
/// the path, so the caller must not take the OS lock a second time. That matters
/// beyond convenience: `flock` is per-open-file-description, so a second lock on
/// a fresh handle for the same file blocks against the first even within one
/// process. Without re-entrancy, a sequence that holds a lock across a
/// read-decide-write would deadlock the moment the inner read tried to lock too.
///
/// Re-entrant acquisition is satisfied by the outer lock and does not upgrade it:
/// taking a shared lock inside an exclusive one stays exclusive, which is safe.
/// Taking an *exclusive* lock inside a *shared* one would be an upgrade and is
/// not supported — hold the exclusive lock from the start.
fn acquire_in_process(key: &Path, exclusive: bool) -> bool {
    let (mutex, cv) = wait_set();
    // A poisoned wait set means some thread panicked mid-critical-section. The
    // map is still structurally sound, so recover rather than propagating the
    // panic into every future disk write.
    let mut held = mutex.lock().unwrap_or_else(|e| e.into_inner());
    let me = std::thread::current().id();
    loop {
        match held.get_mut(key) {
            Some(h) if h.thread == me => {
                // Shared→exclusive upgrade is not supported: the outer
                // shared lock carries no OS-level exclusion, so an
                // "exclusive" guard obtained here would provide false
                // cross-process safety.
                debug_assert!(
                    h.exclusive || !exclusive,
                    "shared→exclusive lock upgrade for {key:?} — hold the exclusive lock from the start"
                );
                h.depth += 1;
                return true;
            }
            Some(_) => {
                held = cv.wait(held).unwrap_or_else(|e| e.into_inner());
            }
            None => {
                held.insert(
                    key.to_path_buf(),
                    Holder {
                        thread: me,
                        depth: 1,
                        exclusive,
                    },
                );
                return false;
            }
        }
    }
}

/// Drop one level of this thread's claim on `key`.
///
/// Returns `true` when the outermost level was released, meaning the caller
/// should now drop the OS lock and wake waiters.
fn release_in_process(key: &Path) -> bool {
    let (mutex, cv) = wait_set();
    let mut held = mutex.lock().unwrap_or_else(|e| e.into_inner());
    let fully_released = match held.get_mut(key) {
        Some(h) => {
            h.depth -= 1;
            h.depth == 0
        }
        None => false,
    };
    if fully_released {
        held.remove(key);
    }
    drop(held);
    if fully_released {
        cv.notify_all();
    }
    fully_released
}

/// Normalize a lock path so two spellings of one file share a wait-set entry.
///
/// `std::path::absolute` rather than `canonicalize`: the lock file may not exist
/// yet, and canonicalize fails on missing paths. This is lexical only, so it
/// does not resolve symlinks — the OS lock is the authority on identity; this
/// key only needs to be stable within the process.
fn normalize(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The sidecar lock path guarding `target`: `<target>.lock`.
pub fn lock_path_for(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

/// Open (creating if absent) the sidecar lock file.
///
/// Opened `read(true).write(true)` because Windows refuses to lock a handle
/// opened append-only, and `truncate(false)` because the file's contents are
/// irrelevant — only the lock on it matters.
///
/// Lock files sit beside state that is already owner-only, so they are created
/// through [`crate::owner_only_options`] and match it.
fn open_lock_file(path: &Path) -> io::Result<File> {
    crate::open::owner_only_options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// An acquired lock. Releases the OS lock and the in-process claim on drop.
///
/// A re-entrant acquisition carries no `File`: the outer guard owns the OS lock
/// and releasing it early would drop the whole nest's protection.
#[derive(Debug)]
pub struct FileLock {
    file: Option<File>,
    key: PathBuf,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Release the in-process level first to learn whether this was the
        // outermost one; only then drop the OS lock. A waiter woken by the
        // release must not find the OS lock still held, so the notify inside
        // `release_in_process` happens before the file handle is unlocked —
        // waiters block on the wait set, not on the OS lock, so this ordering is
        // what they actually observe.
        let outermost = release_in_process(&self.key);
        if outermost && let Some(file) = self.file.take() {
            let _ = file.unlock();
        }
    }
}

/// Take an exclusive lock on `lock_path`, blocking until it is available.
///
/// `lock_path` is the sidecar itself — most callers want [`with_lock_exclusive`]
/// or [`lock_path_for`].
pub fn lock_exclusive(lock_path: &Path) -> io::Result<FileLock> {
    let key = normalize(lock_path);
    // Re-entrant: this thread already holds the path, so the outer guard's OS
    // lock still covers us and taking another would deadlock against it.
    if acquire_in_process(&key, true) {
        return Ok(FileLock { file: None, key });
    }
    // From here every early return must release the in-process claim.
    let file = match open_lock_file(lock_path) {
        Ok(f) => f,
        Err(e) => {
            release_in_process(&key);
            return Err(e);
        }
    };
    if let Err(e) = file.lock() {
        release_in_process(&key);
        return Err(e);
    }
    Ok(FileLock {
        file: Some(file),
        key,
    })
}

/// Take a shared (read) lock on `lock_path`, blocking until it is available.
///
/// Note the in-process layer is still exclusive: within one process, readers
/// serialize with each other. Cross-process readers do share. This is the
/// conservative choice — hjkl's readers are short and the alternative needs a
/// reader-count map that buys nothing at this scale.
pub fn lock_shared(lock_path: &Path) -> io::Result<FileLock> {
    let key = normalize(lock_path);
    // Re-entrant: satisfied by the outer guard, whatever its mode. A shared
    // request nested inside an exclusive lock stays exclusive, which is safe.
    if acquire_in_process(&key, false) {
        return Ok(FileLock { file: None, key });
    }
    let file = match open_lock_file(lock_path) {
        Ok(f) => f,
        Err(e) => {
            release_in_process(&key);
            return Err(e);
        }
    };
    if let Err(e) = file.lock_shared() {
        release_in_process(&key);
        return Err(e);
    }
    Ok(FileLock {
        file: Some(file),
        key,
    })
}

/// Run `f` holding an exclusive lock on `target`'s sidecar.
///
/// This is the wrapper for read-modify-write sequences: the lock must span the
/// **whole** load → mutate → store, not just the store, or concurrent instances
/// still lose each other's updates (an atomic rename makes each write
/// self-consistent, it does not make RMW atomic).
pub fn with_lock_exclusive<T, F>(target: &Path, f: F) -> io::Result<T>
where
    F: FnOnce() -> io::Result<T>,
{
    let _guard = lock_exclusive(&lock_path_for(target))?;
    f()
}

/// Run `f` holding a shared lock on `target`'s sidecar.
pub fn with_lock_shared<T, F>(target: &Path, f: F) -> io::Result<T>
where
    F: FnOnce() -> io::Result<T>,
{
    let _guard = lock_shared(&lock_path_for(target))?;
    f()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn lock_path_appends_suffix() {
        assert_eq!(
            lock_path_for(Path::new("/a/b/filestate.bin")),
            PathBuf::from("/a/b/filestate.bin.lock")
        );
    }

    #[test]
    fn exclusive_lock_is_released_on_drop() {
        let td = tempfile::tempdir().unwrap();
        let target = td.path().join("state.bin");
        // Two sequential acquisitions must both succeed; the first is released
        // by drop at the end of its statement.
        drop(lock_exclusive(&lock_path_for(&target)).unwrap());
        drop(lock_exclusive(&lock_path_for(&target)).unwrap());
    }

    #[test]
    fn with_lock_exclusive_runs_body_and_propagates_value() {
        let td = tempfile::tempdir().unwrap();
        let target = td.path().join("state.bin");
        let got = with_lock_exclusive(&target, || Ok(42usize)).unwrap();
        assert_eq!(got, 42);
    }

    #[test]
    fn with_lock_exclusive_releases_after_error() {
        let td = tempfile::tempdir().unwrap();
        let target = td.path().join("state.bin");
        let err = with_lock_exclusive(&target, || Err::<(), _>(io::Error::other("body failed")));
        assert!(err.is_err());
        // Lock must be free again despite the body erroring.
        drop(lock_exclusive(&lock_path_for(&target)).unwrap());
    }

    /// The in-process layer must serialize threads that would otherwise each
    /// get their own `flock` open-file-description and race.
    #[test]
    fn threads_in_one_process_serialize() {
        let td = tempfile::tempdir().unwrap();
        let target = Arc::new(td.path().join("counter.bin"));
        // `inside` is incremented on entry and decremented on exit while
        // holding the lock; if two threads are ever inside at once, `max`
        // records it.
        let inside = Arc::new(AtomicUsize::new(0));
        let max = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let target = Arc::clone(&target);
            let inside = Arc::clone(&inside);
            let max = Arc::clone(&max);
            handles.push(std::thread::spawn(move || {
                for _ in 0..25 {
                    with_lock_exclusive(&target, || {
                        let n = inside.fetch_add(1, Ordering::SeqCst) + 1;
                        max.fetch_max(n, Ordering::SeqCst);
                        std::thread::yield_now();
                        inside.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            max.load(Ordering::SeqCst),
            1,
            "two threads held the same lock at once"
        );
    }

    /// Nesting must not deadlock. This is what lets a caller hold a lock across
    /// a read-decide-write while the low-level read and write each lock too —
    /// the shape swap recovery needs (read swap, check the writer pid, remove).
    #[test]
    fn nested_locks_on_one_path_are_reentrant() {
        let td = tempfile::tempdir().unwrap();
        let target = td.path().join("swap.swp");
        let got = with_lock_exclusive(&target, || {
            // Inner exclusive (a write) inside the outer sequence lock.
            with_lock_exclusive(&target, || Ok(()))?;
            // Inner shared (a read) inside the outer exclusive lock.
            with_lock_shared(&target, || Ok(()))?;
            // Three deep, to prove depth counting rather than a boolean.
            with_lock_exclusive(&target, || with_lock_exclusive(&target, || Ok(7usize)))
        })
        .unwrap();
        assert_eq!(got, 7);
        // Fully released afterwards: a fresh acquisition must still succeed.
        drop(lock_exclusive(&lock_path_for(&target)).unwrap());
    }

    /// Re-entrancy is per thread: another thread must still be excluded while
    /// the first holds the path, however deeply nested it is.
    #[test]
    fn reentrancy_does_not_leak_across_threads() {
        let td = tempfile::tempdir().unwrap();
        let target = Arc::new(td.path().join("t.bin"));
        let inside = Arc::new(AtomicUsize::new(0));
        let max = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let target = Arc::clone(&target);
            let inside = Arc::clone(&inside);
            let max = Arc::clone(&max);
            handles.push(std::thread::spawn(move || {
                for _ in 0..20 {
                    with_lock_exclusive(&target, || {
                        let n = inside.fetch_add(1, Ordering::SeqCst) + 1;
                        max.fetch_max(n, Ordering::SeqCst);
                        // Nest while holding it — the other threads must still
                        // be excluded for the whole nested span.
                        with_lock_exclusive(&target, || Ok(()))?;
                        std::thread::yield_now();
                        inside.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            max.load(Ordering::SeqCst),
            1,
            "re-entrancy leaked to another thread"
        );
    }

    /// Distinct targets must not contend — a per-path wait set, not a global one.
    #[test]
    fn distinct_paths_do_not_block_each_other() {
        let td = tempfile::tempdir().unwrap();
        let a = td.path().join("a.bin");
        let b = td.path().join("b.bin");
        let _outer = lock_exclusive(&lock_path_for(&a)).unwrap();
        // Would deadlock if the wait set were keyed globally instead of by path.
        let _inner = lock_exclusive(&lock_path_for(&b)).unwrap();
    }

    /// A shared→exclusive lock upgrade must panic in debug builds.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "shared→exclusive lock upgrade")]
    fn shared_to_exclusive_upgrade_panics() {
        let td = tempfile::tempdir().unwrap();
        let p = lock_path_for(&td.path().join("f.bin"));
        let _shared = lock_shared(&p).unwrap();
        let _exclusive = lock_exclusive(&p).unwrap();
    }
}
