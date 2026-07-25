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

use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};

/// In-process wait set: paths currently locked by *this* process.
///
/// A `Condvar` rather than a per-path `Mutex` because `std::sync::Mutex` has no
/// owned-guard form — parking a `MutexGuard` inside [`FileLock`] would need an
/// unsafe lifetime transmute. Acquire/release around a `HashSet` is safe and
/// does the same job.
fn wait_set() -> &'static (Mutex<HashSet<PathBuf>>, Condvar) {
    static SET: OnceLock<(Mutex<HashSet<PathBuf>>, Condvar)> = OnceLock::new();
    SET.get_or_init(|| (Mutex::new(HashSet::new()), Condvar::new()))
}

/// Block until no other thread in this process holds `key`, then claim it.
fn acquire_in_process(key: &Path) {
    let (mutex, cv) = wait_set();
    // A poisoned wait set means some thread panicked mid-critical-section. The
    // set itself is still structurally sound (a HashSet of paths), so recover
    // rather than propagating the panic into every future disk write.
    let mut held = mutex.lock().unwrap_or_else(|e| e.into_inner());
    while held.contains(key) {
        held = cv.wait(held).unwrap_or_else(|e| e.into_inner());
    }
    held.insert(key.to_path_buf());
}

/// Release `key` and wake anyone waiting on it.
fn release_in_process(key: &Path) {
    let (mutex, cv) = wait_set();
    let mut held = mutex.lock().unwrap_or_else(|e| e.into_inner());
    held.remove(key);
    drop(held);
    cv.notify_all();
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
fn open_lock_file(path: &Path) -> io::Result<File> {
    let mut opts = File::options();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Lock files sit beside state that is already owner-only; match it.
        opts.mode(0o600);
    }
    opts.open(path)
}

/// An acquired lock. Releases the OS lock and the in-process claim on drop.
#[derive(Debug)]
pub struct FileLock {
    file: File,
    key: PathBuf,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // OS lock first, then the in-process claim: a waiter woken by the
        // release must not find the OS lock still held.
        let _ = self.file.unlock();
        release_in_process(&self.key);
    }
}

/// Take an exclusive lock on `lock_path`, blocking until it is available.
///
/// `lock_path` is the sidecar itself — most callers want [`with_lock_exclusive`]
/// or [`lock_path_for`].
pub fn lock_exclusive(lock_path: &Path) -> io::Result<FileLock> {
    let key = normalize(lock_path);
    acquire_in_process(&key);
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
    Ok(FileLock { file, key })
}

/// Take a shared (read) lock on `lock_path`, blocking until it is available.
///
/// Note the in-process layer is still exclusive: within one process, readers
/// serialize with each other. Cross-process readers do share. This is the
/// conservative choice — hjkl's readers are short and the alternative needs a
/// reader-count map that buys nothing at this scale.
pub fn lock_shared(lock_path: &Path) -> io::Result<FileLock> {
    let key = normalize(lock_path);
    acquire_in_process(&key);
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
    Ok(FileLock { file, key })
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
}
