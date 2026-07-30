//! Test-only helpers serializing tests that mutate process-global state — the
//! current working directory and environment variables.
//!
//! `std::env::set_current_dir` and `std::env::set_var` change global process
//! state. Under `cargo test`'s single-binary thread pool, tests run as parallel
//! threads, so two such tests race — one test's `chdir`/`set_var` is observed by
//! another, surfacing as spurious `NotFound` errors, wrong trash locations, and
//! nondeterministic failures. (Under `cargo nextest run`, each test is its own
//! process with isolated globals, so the lock is uncontended there.)
//!
//! Both guards below take the same [`SERIAL_LOCK`], so a cwd-mutating test and
//! an env-mutating test are also serialized against each other. Mirrors the
//! in-process `TEST_LOCK: Mutex<()>` that `hjkl-clipboard`'s display tests use.

use std::ffi::{OsStr, OsString};
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
pub struct CwdGuard {
    _lock: MutexGuard<'static, ()>,
    prev: PathBuf,
    env: Vec<(OsString, Option<OsString>)>,
}

impl CwdGuard {
    /// Acquire the serialization lock, then `chdir` into `dir`. The previous
    /// working directory is restored (and the lock released) on drop.
    pub(crate) fn enter(dir: &Path) -> Self {
        let lock = lock();
        let prev = std::env::current_dir().expect("read current dir");
        std::env::set_current_dir(dir).expect("set current dir");
        Self {
            _lock: lock,
            prev,
            env: Vec::new(),
        }
    }

    /// Set an environment variable while holding the same process-global lock
    /// as the cwd change. The previous value is restored with the cwd on drop.
    pub(crate) fn set_env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) {
        let key = key.as_ref().to_os_string();
        let prev = std::env::var_os(&key);
        // SAFETY: this guard holds SERIAL_LOCK for its entire lifetime.
        unsafe { std::env::set_var(&key, value) };
        self.env.push((key, prev));
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.prev);
        for (key, prev) in self.env.drain(..).rev() {
            // SAFETY: still holding SERIAL_LOCK.
            unsafe {
                match prev {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

/// Serializes env-var-mutating tests and restores the variable's prior value
/// when dropped. Hold it for the whole scope in which the variable is set.
pub struct EnvVarGuard {
    _lock: MutexGuard<'static, ()>,
    key: OsString,
    prev: Option<OsString>,
}

impl EnvVarGuard {
    /// Acquire the serialization lock, then set `key=value`. The previous value
    /// (or its absence) is restored on drop.
    pub(crate) fn set(key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        let lock = lock();
        let key = key.as_ref().to_os_string();
        let prev = std::env::var_os(&key);
        // SAFETY: the SERIAL_LOCK guarantees no other test thread is reading or
        // writing the environment for the guard's lifetime.
        unsafe { std::env::set_var(&key, value) };
        Self {
            _lock: lock,
            key,
            prev,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: still holding SERIAL_LOCK.
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var(&self.key, v),
                None => std::env::remove_var(&self.key),
            }
        }
    }
}
