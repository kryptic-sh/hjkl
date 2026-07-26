//! Atomic path-publication with lost-race tolerance.
//!
//! Rename `staging` → `dest`. If `dest` already exists (another thread won
//! the race), staging is cleaned up and the call succeeds — the winner's
//! copy is assumed identical.
//!
//! Payloads are either directories (cloned grammar sources, query trees) or
//! plain files (compiled grammar `.so`), so cleanup dispatches on the staged
//! path's file type.

use std::io;
use std::path::Path;

/// Delete `staging`, whether it is a plain file or a directory.
///
/// The two removal calls are not interchangeable: `remove_dir_all` on a plain
/// file fails with `NotADirectory` and `remove_file` on a directory fails with
/// `IsADirectory`. So stat first and pick the right one. `symlink_metadata`
/// rather than `metadata` so a staged symlink is unlinked instead of followed
/// (and so a dangling one still gets removed).
///
/// Best-effort: cleanup failures are ignored, since the caller's real result is
/// the publish outcome, not the fate of the staging path.
fn remove_staging(staging: &Path) {
    match std::fs::symlink_metadata(staging) {
        Ok(md) if md.is_dir() => {
            let _ = std::fs::remove_dir_all(staging);
        }
        // Plain file or symlink: unlink.
        Ok(_) => {
            let _ = std::fs::remove_file(staging);
        }
        // Already gone (or unstattable) — nothing to do.
        Err(_) => {}
    }
}

/// Publish `staging` to `dest` via `rename`, tolerating `EEXIST`
/// (another thread finished first). Staging is always cleaned up before
/// returning, via [`remove_staging`], which handles both file and directory
/// payloads.
pub(super) fn publish_path(staging: &Path, dest: &Path) -> io::Result<()> {
    match std::fs::rename(staging, dest) {
        Ok(()) => Ok(()),
        Err(_) if dest.exists() => {
            remove_staging(staging);
            Ok(())
        }
        Err(e) => {
            remove_staging(staging);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: cleanup used to be `remove_dir_all`, which fails with
    /// `NotADirectory` on a plain file, leaking the staging file forever.
    ///
    /// Note the shape of the collision: on Unix `rename` over an existing plain
    /// *file* succeeds silently, so a file staging only reaches the
    /// `dest.exists()` arm when `dest` is of another type (and on Windows, when
    /// `dest` is any existing path).
    #[test]
    fn lost_race_removes_file_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("grammar.tmp.so");
        let dest = tmp.path().join("grammar.so");
        std::fs::write(&staging, b"staged").unwrap();
        std::fs::create_dir(&dest).unwrap();

        publish_path(&staging, &dest).unwrap();

        assert!(!staging.exists(), "staging file must be cleaned up");
        assert!(dest.exists(), "winner kept");
    }

    /// The platform-independent leak: `rename` fails for a reason other than a
    /// lost race (here a missing parent), so the error arm cleans up.
    #[test]
    fn error_path_removes_file_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("grammar.tmp.so");
        let dest = tmp.path().join("no-such-dir").join("grammar.so");
        std::fs::write(&staging, b"staged").unwrap();

        let err = publish_path(&staging, &dest).unwrap_err();

        assert!(!staging.exists(), "staging file must be cleaned up: {err}");
        assert!(!dest.exists());
    }

    #[test]
    fn lost_race_removes_dir_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("src.tmp");
        let dest = tmp.path().join("src");
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("parser.c"), b"staged").unwrap();
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("parser.c"), b"winner").unwrap();

        publish_path(&staging, &dest).unwrap();

        assert!(!staging.exists(), "staging dir must be cleaned up");
        assert_eq!(
            std::fs::read(dest.join("parser.c")).unwrap(),
            b"winner",
            "winner kept"
        );
    }

    #[test]
    fn clean_publish_renames_file_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("grammar.tmp.so");
        let dest = tmp.path().join("grammar.so");
        std::fs::write(&staging, b"staged").unwrap();

        publish_path(&staging, &dest).unwrap();

        assert!(!staging.exists(), "staging moved away");
        assert_eq!(std::fs::read(&dest).unwrap(), b"staged");
    }
}
