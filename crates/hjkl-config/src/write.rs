//! Surgical single-key TOML write-back, powered by `toml_edit`.
//!
//! Some settings (window/dock sizes, anything interactively resized) need to
//! persist to the user's config file so they survive across sessions. A
//! naive `write_default`-style "serialize the whole struct and overwrite the
//! file" would blow away the user's comments and formatting on every
//! resize — the config file is meant to stay human-owned. [`write_key_at`]
//! instead edits exactly one key in place via `toml_edit`'s format-preserving
//! document model: every other byte of the file (comments, blank lines,
//! key order, quoting style) is left untouched.
//!
//! Missing parent tables are created as needed; a missing file is created
//! fresh with just the one key. Never call [`write_default`](crate::write_default)
//! and this function on the same file expecting both to coexist gracefully —
//! `write_default` is a full-overwrite tool for scaffolding, this is a
//! targeted patch tool for runtime persistence.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::ConfigError;

// ---------------------------------------------------------------------------
// PID liveness check
// ---------------------------------------------------------------------------

/// Report whether a process with the given `pid` is still running.
///
/// - `Some(true)`  — the owner is alive.
/// - `Some(false)` — the owner is provably gone.
/// - `None`        — liveness could not be determined on this platform.
///
/// On Linux this probes `/proc/<pid>/`. Elsewhere there is no cheap probe
/// without a `libc` dependency `hjkl-config` does not carry, so it returns
/// `None` and the mtime check ([`LOCK_STALE_SECS`]) becomes the sole staleness
/// signal. Returning `None` (rather than `false`) is important: "cannot probe"
/// must not be mistaken for "dead", or every live lock would look stale and
/// mutual exclusion would break on non-Linux hosts.
fn pid_liveness(pid: u32) -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        Some(std::fs::metadata(format!("/proc/{pid}")).is_ok())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// Maximum age of a lock file before it is considered stale regardless of
/// PID liveness (guards against PID reuse and unreadable lock files).
///
/// **Known residual**: when two processes simultaneously reclaim the same
/// stale lock, B's `remove_file` can delete A's freshly-created replacement
/// before B's own `create_new`, letting both proceed.  The window is narrow
/// (post-crash recovery + concurrent `write_key_at`), config writes are
/// idempotent per-key, and the worst case is a lost dock-resize persistence
/// event — not worth the complexity of a rename-to-unique-name reclaim.
const LOCK_STALE_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// Atomic write helpers
// ---------------------------------------------------------------------------

/// Filesystem lock bound to the lifetime of a single read-modify-write
/// operation on `config_path`.  Creating the guard acquires the lock (retrying
/// if another writer holds it); dropping the guard removes the lock file.
///
/// The lock file records the owner PID and a write timestamp.  On acquisition,
/// if the lock already exists, the recorded PID is checked for liveness and
/// the mtime is checked against [`LOCK_STALE_SECS`]; a stale lock is reclaimed
/// rather than failing outright.  This prevents a crashed process (SIGKILL,
/// panic-abort, power loss) from permanently bricking runtime config
/// persistence.
struct LockGuard(PathBuf);

impl LockGuard {
    /// Try to create `<config_path>.lock`.  Up to 10 attempts, 10 ms apart,
    /// before bailing out with [`ConfigError::Write`].  Between attempts any
    /// existing lock is checked for staleness and reclaimed if the owner is
    /// dead or the lock has exceeded [`LOCK_STALE_SECS`].
    fn acquire(config_path: &Path) -> Result<Self, ConfigError> {
        let lock_path = PathBuf::from(format!("{}.lock", config_path.display()));
        // The lock file's parent dir may not exist yet for new configs
        // (e.g. nested dirs created by `create_dir_all` later in the
        // function). Ensure it exists before trying to create the lock.
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ConfigError::Write {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        const MAX_ATTEMPTS: u32 = 10;
        let mut attempts = 0u32;
        loop {
            match File::create_new(&lock_path) {
                Ok(mut f) => {
                    // Record owner PID + timestamp so a future acquire can
                    // detect staleness.
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let _ = write!(f, "{} {}", std::process::id(), now);
                    let _ = f.sync_all();
                    return Ok(Self(lock_path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&lock_path) {
                        // Reclaim the stale lock and retry immediately
                        // (don't count as an attempt — the lock wasn't
                        // genuinely contended).
                        let _ = std::fs::remove_file(&lock_path);
                        continue;
                    }
                    attempts += 1;
                    if attempts >= MAX_ATTEMPTS {
                        return Err(ConfigError::Write {
                            path: lock_path,
                            source: e,
                        });
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    return Err(ConfigError::Write {
                        path: lock_path,
                        source: e,
                    });
                }
            }
        }
    }
}

/// Check whether the lock file at `lock_path` is stale — the owner PID is
/// dead, the file is unreadable/corrupt, or its mtime exceeds
/// [`LOCK_STALE_SECS`].
fn lock_is_stale(lock_path: &Path) -> bool {
    // mtime check first: cheap and catches most cases.
    if let Ok(meta) = std::fs::metadata(lock_path)
        && let Ok(mod_time) = meta.modified()
        && let Ok(elapsed) = mod_time.elapsed()
        && elapsed.as_secs() > LOCK_STALE_SECS
    {
        return true;
    }

    // PID liveness check: read the recorded PID from the lock file body.
    let mut contents = String::new();
    if File::open(lock_path)
        .and_then(|mut f| f.read_to_string(&mut contents))
        .is_ok()
    {
        // Format: "<pid> <timestamp_secs>". Within the freshness window, only
        // reclaim a lock whose owner is *provably* gone. When liveness cannot
        // be probed (non-Linux), keep the lock and let mtime govern — treating
        // "unknown" as "dead" would make every live lock look stale.
        if let Some(pid_str) = contents.split_whitespace().next()
            && let Ok(pid) = pid_str.parse::<u32>()
        {
            return matches!(pid_liveness(pid), Some(false));
        }
        // Corrupt / unparseable body → stale (can't verify owner).
        return true;
    }

    // Can't read the lock file at all → stale.
    true
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Build the temp-file path: `.<filename>.hjkl-tmp.<pid>` in the same
/// directory as `config_path`.
fn temp_path_for(config_path: &Path) -> PathBuf {
    let dir = config_path.parent().unwrap_or(Path::new("."));
    let file_name = config_path
        .file_name()
        .unwrap_or(std::ffi::OsStr::new("config.toml"));
    let temp_name = format!(
        ".{}.hjkl-tmp.{}",
        file_name.to_string_lossy(),
        std::process::id()
    );
    dir.join(temp_name)
}

/// Write `contents` to `path` atomically: write to a same-directory temp file,
/// fsync it, then rename over the real path.  The temp file is cleaned up on
/// every error path.
fn atomic_write(path: &Path, contents: &str) -> Result<(), ConfigError> {
    let temp_path = temp_path_for(path);

    let mut file = File::create_new(&temp_path).map_err(|e| ConfigError::Write {
        path: temp_path.clone(),
        source: e,
    })?;
    file.write_all(contents.as_bytes()).map_err(|e| {
        let _ = std::fs::remove_file(&temp_path);
        ConfigError::Write {
            path: temp_path.clone(),
            source: e,
        }
    })?;
    file.sync_all().map_err(|e| {
        let _ = std::fs::remove_file(&temp_path);
        ConfigError::Write {
            path: temp_path.clone(),
            source: e,
        }
    })?;

    std::fs::rename(&temp_path, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp_path);
        ConfigError::Write {
            path: path.to_path_buf(),
            source: e,
        }
    })?;
    // fsync the parent directory so the rename is durable.
    if let Some(parent) = path.parent()
        && let Ok(pdir) = File::open(parent)
    {
        let _ = pdir.sync_all();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Set `dotted_path` (e.g. `"explorer.width"`) to `value` in the TOML file at
/// `path`, preserving every other byte of the file.
///
/// - `dotted_path` is split on `.`; all segments but the last are treated as
///   (and created as, if missing) tables. A dotted path with no `.` sets a
///   top-level key.
/// - If `path` does not exist, a fresh document is created containing only
///   the resulting key (plus its parent tables).
/// - If a path segment exists but is not a table (e.g. `explorer` is a
///   string), returns [`ConfigError::Invalid`] rather than clobbering it.
///
/// The read-modify-write sequence is guarded by a lock file
/// (`<path>.lock`) so concurrent hjkl processes don't silently lose each
/// other's updates.  The write itself is atomic (temp-file + `fsync` +
/// `rename`), so a crash or I/O error mid-write never leaves a
/// partial/truncated config behind.
///
/// # Errors
///
/// [`ConfigError::Io`] on read failure (other than "file does not exist"),
/// [`ConfigError::Invalid`] when the existing file isn't valid TOML or a
/// path segment collides with a non-table value, [`ConfigError::Write`] on
/// write failure.
pub fn write_key_at(
    path: &Path,
    dotted_path: &str,
    value: impl Into<toml_edit::Value>,
) -> Result<(), ConfigError> {
    // Acquire file lock *before* reading — without it two processes can
    // race: both read the same state, both write, last writer wins.
    let _lock = LockGuard::acquire(path)?;

    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };
    let mut doc = existing
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| ConfigError::Invalid {
            path: path.to_path_buf(),
            message: format!("existing config is not valid TOML: {e}"),
        })?;

    let segments: Vec<&str> = dotted_path.split('.').collect();
    let (last, parents) = segments
        .split_last()
        .expect("dotted_path must have at least one segment");

    let mut table: &mut toml_edit::Table = doc.as_table_mut();
    for seg in parents {
        let item = table
            .entry(seg)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        table = item.as_table_mut().ok_or_else(|| ConfigError::Invalid {
            path: path.to_path_buf(),
            message: format!("`{seg}` (in `{dotted_path}`) exists but is not a table"),
        })?;
    }
    table.insert(last, toml_edit::Item::Value(value.into()));

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ConfigError::Write {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    atomic_write(path, &doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_is_not_stale_for_live_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml.lock");
        std::fs::write(&path, format!("{} 0", std::process::id())).unwrap();
        // Live on Linux (probed) and "unknown" elsewhere both mean "keep the
        // lock" while its mtime is fresh — so this must hold on every platform.
        assert!(!lock_is_stale(&path));
    }

    #[test]
    fn lock_is_stale_for_corrupt_body() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml.lock");
        std::fs::write(&path, "not-a-pid").unwrap();
        // An unparseable owner can't be verified → reclaim it (all platforms).
        assert!(lock_is_stale(&path));
    }

    #[test]
    fn creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        write_key_at(&path, "explorer.width", 42i64).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[explorer]"));
        assert!(text.contains("width = 42"));
    }

    #[test]
    fn preserves_unrelated_content_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# leader comment\n[editor]\nleader = \" \"\n\n[explorer]\nwidth = 36\n",
        )
        .unwrap();
        write_key_at(&path, "explorer.width", 50i64).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# leader comment"));
        assert!(text.contains("leader = \" \""));
        assert!(text.contains("width = 50"));
        assert!(!text.contains("width = 36"));
    }

    #[test]
    fn creates_missing_parent_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[editor]\nleader = \" \"\n").unwrap();
        write_key_at(&path, "panel.height", 12i64).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[panel]"));
        assert!(text.contains("height = 12"));
        assert!(text.contains("[editor]"));
    }

    #[test]
    fn overwrites_existing_key_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[explorer]\nwidth = 20\n").unwrap();
        write_key_at(&path, "explorer.width", 60i64).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let width_lines: Vec<&str> = text.lines().filter(|l| l.contains("width")).collect();
        assert_eq!(width_lines.len(), 1, "must not duplicate the key");
        assert!(width_lines[0].contains("60"));
    }

    #[test]
    fn errors_when_segment_is_not_a_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "explorer = \"oops\"\n").unwrap();
        let err = write_key_at(&path, "explorer.width", 40i64).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn invalid_existing_toml_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "not [ valid toml").unwrap();
        let err = write_key_at(&path, "explorer.width", 40i64).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }
}
