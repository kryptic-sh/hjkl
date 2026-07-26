//! Atomic directory-publication with lost-race tolerance.
//!
//! Rename `staging` → `dest`. If `dest` already exists (another thread won
//! the race), staging is cleaned up and the call succeeds — the winner's
//! copy is assumed identical.

use std::io;
use std::path::Path;

/// Publish `staging` to `dest` via `rename`, tolerating `EEXIST`
/// (another thread finished first). Staging is always cleaned up before
/// returning. Uses [`std::fs::remove_dir_all`] which handles both files
/// and directories.
pub(super) fn publish_dir(staging: &Path, dest: &Path) -> io::Result<()> {
    match std::fs::rename(staging, dest) {
        Ok(()) => Ok(()),
        Err(_) if dest.exists() => {
            let _ = std::fs::remove_dir_all(staging);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(staging);
            Err(e)
        }
    }
}
