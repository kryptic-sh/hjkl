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
        // reclaim a lock whose owner is *provably* gone — `Some(false)`. A
        // platform with no probe answers `None`; keep the lock there and let
        // mtime govern, since treating "unknown" as "dead" would make every
        // live lock look stale.
        if let Some(pid_str) = contents.split_whitespace().next()
            && let Ok(pid) = pid_str.parse::<u32>()
        {
            return matches!(hjkl_fs::pid_liveness(pid), Some(false));
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

/// Write `contents` to `path` atomically: a same-directory temp file
/// (`.<filename>.hjkl-tmp.<pid>`), fsync, rename over the real path, fsync the
/// parent.  The temp file is cleaned up on every error path.
///
/// The whole mechanism is [`hjkl_fs::write_atomic`]; this function is now only
/// the `io::Error` → [`ConfigError::Write`] mapping.
///
/// Neither stock preset fits, so the options are spelled out:
///
/// - **No non-atomic fallback** (as `state()`, unlike `document()`). The previous
///   hand-rolled write was always atomic, and refusing to persist a dock width is
///   the right outcome when the atomic path is unavailable — an in-place truncate
///   that then failed would destroy a hand-written config, which is precisely
///   what this module exists to protect.
/// - **Preserve the existing mode** (as `document()`, unlike `state()`). This is
///   the *user's* config file, not hjkl's private state: they may have chosen
///   `0644` deliberately, or keep it group-readable in a shared dotfiles setup.
///   Silently tightening it to `0600` on the first `:set` hjkl persists would be
///   hjkl editing a property of the file it was never asked to change. A config
///   hjkl creates itself still lands at `0600` via `mode`, since there is no
///   prior mode to preserve.
fn atomic_write(path: &Path, contents: &str) -> Result<(), ConfigError> {
    let opts = hjkl_fs::WriteOptions {
        mode: Some(0o600),
        preserve_mode: true,
        nonatomic_fallback: false,
        ..hjkl_fs::WriteOptions::default()
    };
    hjkl_fs::write_atomic(path, contents.as_bytes(), &opts).map_err(|e| ConfigError::Write {
        path: path.to_path_buf(),
        source: e,
    })
}

/// The file a write to `path` really lands in: what a symlinked `path`
/// resolves to, or `path` itself canonicalized.
///
/// Renaming a temp file over the link would replace the link with a regular
/// file: a config that home-manager links into the Nix store would silently
/// stop being managed (and block the next `home-manager switch`), and one
/// that stow links into a dotfiles checkout would never get the edit. Writing
/// through the link is what vim's `:w` does. Canonicalizing also gives every
/// spelling of one file the same lock. A dangling link is an error rather than
/// a cue to replace it.
fn resolve_target(path: &Path) -> Result<PathBuf, ConfigError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            std::fs::canonicalize(path).map_err(|e| ConfigError::Write {
                path: path.to_path_buf(),
                source: e,
            })
        }
        _ => Ok(hjkl_fs::canonicalize_nearest(path)),
    }
}

/// Refuse a target whose mode marks it read-only, as vim refuses `:w` on one.
/// The directory's permissions are no guide: on a single-user Nix install the
/// store directory is writable by its owner, and only the file's mode says the
/// file must not change.
fn refuse_read_only(target: &Path) -> Result<(), ConfigError> {
    match std::fs::metadata(target) {
        Ok(meta) if meta.permissions().readonly() => Err(ConfigError::ReadOnly {
            path: target.to_path_buf(),
        }),
        _ => Ok(()),
    }
}

/// Re-point an error raised while working on the resolved file at the path
/// the caller passed in. That is the path the user configured, and the
/// resolved one is a `\\?\` path on Windows.
fn reported_at(err: ConfigError, path: &Path) -> ConfigError {
    let path = path.to_path_buf();
    match err {
        ConfigError::Io { source, .. } => ConfigError::Io { path, source },
        ConfigError::Write { source, .. } => ConfigError::Write { path, source },
        ConfigError::ReadOnly { .. } => ConfigError::ReadOnly { path },
        ConfigError::Invalid { message, .. } => ConfigError::Invalid { path, message },
        other => other,
    }
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
/// When `path` is a symlink, the write goes through it: the file the link
/// resolves to is rewritten and the link is kept. `path` is resolved once, up
/// front, and the lock, the read and the write all use that real file, so a
/// link re-pointed mid-write cannot receive another file's contents.
///
/// A file whose mode marks it read-only is never rewritten, symlinked or not:
/// that is how a config home-manager links into the Nix store looks, and how a
/// user marks a config they do not want hjkl to touch.
///
/// The read-modify-write sequence is guarded by a lock file beside the real
/// file (`<file>.lock`), so concurrent hjkl processes don't silently lose each
/// other's updates, whichever path they reached the file by.  The write itself
/// is atomic (temp-file + `fsync` + `rename`), so a crash or I/O error
/// mid-write never leaves a partial/truncated config behind.
///
/// # Errors
///
/// [`ConfigError::ReadOnly`] when the file's mode marks it read-only,
/// [`ConfigError::Io`] on read failure (other than "file does not exist"),
/// [`ConfigError::Invalid`] when the existing file isn't valid TOML or a
/// path segment collides with a non-table value, [`ConfigError::Write`] on
/// write failure (including a dangling symlink, and a lock that cannot be
/// created because the real file's directory is not writable). Every error
/// names `path` as passed in, not the file it resolved to.
pub fn write_key_at(
    path: &Path,
    dotted_path: &str,
    value: impl Into<toml_edit::Value>,
) -> Result<(), ConfigError> {
    resolve_target(path)
        .and_then(|target| write_key_to(&target, dotted_path, value.into()))
        .map_err(|e| reported_at(e, path))
}

/// [`write_key_at`] on an already-resolved `path`.
fn write_key_to(
    path: &Path,
    dotted_path: &str,
    value: toml_edit::Value,
) -> Result<(), ConfigError> {
    // Before the lock, so a refused file gets nothing created beside it.
    refuse_read_only(path)?;

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
    table.insert(last, toml_edit::Item::Value(value));

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
        // "Alive" (unix, Windows) and "cannot probe" (anywhere else) both mean
        // "keep the lock" while its mtime is fresh — so this holds everywhere.
        assert!(!lock_is_stale(&path));
    }

    /// A lock left behind by a process that is gone is reclaimed immediately,
    /// without waiting out [`LOCK_STALE_SECS`]. The probe is what supplies the
    /// `Some(false)`; while it was `/proc`-only, macOS and Windows answered
    /// "unknown" here and every crashed writer blocked config persistence for a
    /// full minute.
    #[cfg(any(unix, windows))]
    #[test]
    fn lock_is_stale_for_dead_owner_before_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml.lock");
        // Freshly written, so the mtime branch cannot be what reclaims it.
        std::fs::write(&path, "999999999 0").unwrap();
        assert!(
            lock_is_stale(&path),
            "a lock whose owner is provably gone must be reclaimable at once"
        );
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

    #[cfg(unix)]
    #[test]
    fn symlinked_config_keeps_the_link_and_updates_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let dotfiles = dir.path().join("dotfiles");
        std::fs::create_dir(&dotfiles).unwrap();
        let target = dotfiles.join("config.toml");
        std::fs::write(&target, "# mine\n[explorer]\nwidth = 20\n").unwrap();
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        write_key_at(&link, "explorer.width", 60i64).unwrap();

        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink(), "the link was replaced");
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
        let text = std::fs::read_to_string(&target).unwrap();
        assert!(text.contains("# mine"));
        assert!(text.contains("width = 60"));
    }

    /// The home-manager case on a single-user Nix install, where the store
    /// directory is writable by its owner and only the file's mode says it is
    /// immutable.
    #[cfg(unix)]
    #[test]
    fn symlink_to_read_only_file_is_refused_and_changes_nothing() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        std::fs::create_dir(&store).unwrap();
        let target = store.join("config.toml");
        let original = "[explorer]\nwidth = 20\n";
        std::fs::write(&target, original).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let result = write_key_at(&link, "explorer.width", 60i64);

        assert!(
            matches!(result, Err(ConfigError::ReadOnly { .. })),
            "expected a read-only error, got {result:?}"
        );
        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink(), "the link was replaced");
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o444, "the target's mode changed");
        let entries: Vec<_> = std::fs::read_dir(&store).unwrap().collect();
        assert_eq!(entries.len(), 1, "something was created beside the target");
    }

    /// Whether `dir`, just made 0555, really refuses a new file. Root ignores
    /// directory modes, so a test run as root cannot build a read-only
    /// directory this way, and the tests that need one skip that part rather
    /// than fail for a reason that has nothing to do with the code.
    #[cfg(unix)]
    fn dir_refuses_writes(dir: &Path) -> bool {
        let probe = dir.join(".write-probe");
        match std::fs::File::create(&probe) {
            Ok(_) => {
                std::fs::remove_file(&probe).unwrap();
                eprintln!(
                    "skipping the read-only-directory check: {} accepts writes \
                     despite mode 0555 (running as root?)",
                    dir.display()
                );
                false
            }
            Err(_) => true,
        }
    }

    /// A store directory mounted read-only fails cleanly, before anything is
    /// written, and leaves both the link and the file alone.
    #[cfg(unix)]
    #[test]
    fn symlink_into_read_only_dir_fails_cleanly_and_changes_nothing() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        std::fs::create_dir(&store).unwrap();
        let target = store.join("config.toml");
        let original = "[explorer]\nwidth = 20\n";
        std::fs::write(&target, original).unwrap();
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o555)).unwrap();
        if !dir_refuses_writes(&store) {
            std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let result = write_key_at(&link, "explorer.width", 60i64);
        // Restore writability so tempdir deletion works.
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            matches!(result, Err(ConfigError::Write { .. })),
            "expected a write error, got {result:?}"
        );
        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink(), "the link was replaced");
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
    }

    /// The lock belongs to the real file: one held beside it blocks a write
    /// through the link, and one held beside the link does not.
    #[cfg(unix)]
    #[test]
    fn lock_sits_beside_the_real_file_not_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let dotfiles = dir.path().join("dotfiles");
        std::fs::create_dir(&dotfiles).unwrap();
        let target = dotfiles.join("config.toml");
        std::fs::write(&target, "[explorer]\nwidth = 20\n").unwrap();
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        // Owned by this live process with a fresh mtime, so never stale.
        let held = format!("{} 0", std::process::id());

        let link_lock = dir.path().join("config.toml.lock");
        std::fs::write(&link_lock, &held).unwrap();
        write_key_at(&link, "explorer.width", 40i64).unwrap();
        assert!(
            std::fs::read_to_string(&target)
                .unwrap()
                .contains("width = 40")
        );
        std::fs::remove_file(&link_lock).unwrap();

        std::fs::write(dotfiles.join("config.toml.lock"), &held).unwrap();
        let result = write_key_at(&link, "explorer.width", 60i64);
        assert!(
            matches!(result, Err(ConfigError::Write { .. })),
            "a lock held beside the real file must block the write, got {result:?}"
        );
        assert!(
            std::fs::read_to_string(&target)
                .unwrap()
                .contains("width = 40")
        );
    }

    /// Errors name the path the caller configured, not the file it resolves to
    /// (on Windows that would be a `\\?\` path), whichever step raised them.
    #[cfg(unix)]
    #[test]
    fn errors_name_the_callers_path_when_it_is_a_symlink() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        std::fs::create_dir(&store).unwrap();
        let read_only = store.join("read-only.toml");
        std::fs::write(&read_only, "").unwrap();
        std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o444)).unwrap();
        let writable = store.join("writable.toml");
        std::fs::write(&writable, "").unwrap();
        let read_only_link = dir.path().join("read-only.toml");
        std::os::unix::fs::symlink(&read_only, &read_only_link).unwrap();
        let writable_link = dir.path().join("writable.toml");
        std::os::unix::fs::symlink(&writable, &writable_link).unwrap();

        let refused = write_key_at(&read_only_link, "explorer.width", 60i64);
        match refused {
            Err(ConfigError::ReadOnly { path }) => assert_eq!(path, read_only_link),
            other => panic!("expected a read-only error, got {other:?}"),
        }

        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o555)).unwrap();
        let enforced = dir_refuses_writes(&store);
        let unlockable = write_key_at(&writable_link, "explorer.width", 60i64);
        // Restore writability so tempdir deletion works.
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !enforced {
            return;
        }
        match unlockable {
            Err(ConfigError::Write { path, .. }) => assert_eq!(path, writable_link),
            other => panic!("expected a write error, got {other:?}"),
        }
    }

    #[test]
    fn read_only_config_is_refused_and_left_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "[explorer]\nwidth = 20\n";
        std::fs::write(&path, original).unwrap();
        let writable = std::fs::metadata(&path).unwrap().permissions();
        let mut read_only = writable.clone();
        read_only.set_readonly(true);
        std::fs::set_permissions(&path, read_only).unwrap();

        let result = write_key_at(&path, "explorer.width", 60i64);
        let text = std::fs::read_to_string(&path).unwrap();
        let still_read_only = std::fs::metadata(&path).unwrap().permissions().readonly();
        // Restore writability so tempdir deletion works on Windows.
        std::fs::set_permissions(&path, writable).unwrap();

        match result {
            // The path as given: canonicalizing it would show `\\?\` on Windows.
            Err(ConfigError::ReadOnly { path: reported }) => assert_eq!(reported, path),
            other => panic!("expected a read-only error, got {other:?}"),
        }
        assert_eq!(text, original);
        assert!(still_read_only, "the file's mode changed");
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
