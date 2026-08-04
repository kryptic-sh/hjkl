//! Test-only helper serializing tests that mutate process-global state — the
//! current working directory.
//!
//! `std::env::set_current_dir` changes global process state. Under `cargo
//! test`'s single-binary thread pool, tests run as parallel threads, so two
//! such tests race — one test's `chdir` is observed by another, surfacing as
//! spurious `NotFound` errors and nondeterministic failures. (Under `cargo
//! nextest run`, each test is its own process with isolated globals, so the
//! lock is uncontended there.)
//!
//! The guard takes [`SERIAL_LOCK`]. Mirrors the in-process
//! `TEST_LOCK: Mutex<()>` that `hjkl-clipboard`'s display tests use.
//!
//! **The lock is not isolation.** It serializes the tests that take it and
//! nothing else: every other test thread in the binary keeps running and keeps
//! reading the mutated cwd. So a guard is the right tool only where the global
//! itself is under test (or unavoidable, as the cwd is for the explorer's
//! root). Where a path can simply be passed in — the trash root, the swap
//! root, the anvil store — pass it in; see [`hjkl_app::trash::TrashRoot`].

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// One lock for all process-global-state mutation in tests. A single guard must
/// never be taken twice on the same thread (would deadlock); each guard type
/// acquires it exactly once for its lifetime.
static SERIAL_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    // Recover from a poisoned lock: a panicking test poisons the mutex, but the
    // only invariant it guards is "one mutation at a time", which the
    // restore-on-drop below re-establishes regardless.
    SERIAL_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Serializes cwd-mutating tests and restores the prior working directory when
/// dropped. Hold it for the whole scope in which the cwd is changed.
///
/// It used to carry a `set_env` companion (and a separate `EnvVarGuard`) so a
/// test could override `XDG_CACHE_HOME` under the same lock. Both are gone:
/// the lock only holds back the tests that take it, while an environment
/// variable is read by the whole process, so the override still reached every
/// other test thread. The tests that needed it now take an explicit root
/// instead — `hjkl_app::trash::TrashRoot::At(..)`,
/// `hjkl_app::swap::SwapRoot::At(..)` — which no other test can observe.
/// Anything reaching for `set_env` should do the same: inject the path, do not
/// move the variable everyone shares.
pub struct CwdGuard {
    _lock: MutexGuard<'static, ()>,
    prev: PathBuf,
}

impl CwdGuard {
    /// Acquire the serialization lock, then `chdir` into `dir`. The previous
    /// working directory is restored (and the lock released) on drop.
    pub(crate) fn enter(dir: &Path) -> Self {
        let lock = lock();
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
