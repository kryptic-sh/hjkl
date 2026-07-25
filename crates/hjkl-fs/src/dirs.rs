//! XDG-rooted directories, threaded through [`hjkl_xdg`].
//!
//! `hjkl-xdg` stays the single resolver — this module re-exports it so callers
//! need one dependency for "where does this go and how do I write it", instead of
//! pairing a path crate with an I/O crate at every site. Nothing here
//! re-implements resolution; adding a second implementation is exactly the drift
//! this crate exists to remove.
//!
//! The layout is uniform on every platform, including macOS and Windows: `$XDG_*`
//! when set to an absolute path, else `~/.config`, `~/.local/share`, `~/.cache`,
//! `~/.local/state`. No `%APPDATA%`, no `~/Library/Application Support`.

use std::io;
use std::path::{Path, PathBuf};

pub use hjkl_xdg::{
    Error as XdgError, cache_dir, cache_home, config_dir, config_home, data_dir, data_home,
    state_dir, state_home,
};

/// Map an XDG resolution failure into an `io::Error`, so callers deal in one
/// error type across paths and I/O.
pub fn to_io(err: XdgError) -> io::Error {
    io::Error::other(format!("xdg: {err}"))
}

/// Create `dir` (and parents) and make it owner-only on Unix.
///
/// Every hjkl state directory wants this: swap bodies, undo history and the
/// cursor index all describe what the user was editing, so other local users must
/// not be able to enumerate or read them. Previously each of swap/undofile/trash/
/// filestate repeated the same `create_dir_all` + `set_permissions(0o700)` pair.
///
/// The mode is applied after creation rather than via `DirBuilder::mode` so that
/// an already-existing directory with looser permissions is tightened too.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Best-effort: a directory owned by another user cannot be chmod'ed, and
        // failing the whole operation over that is worse than proceeding.
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// `<XDG_CACHE_HOME>/<app>/<sub>`, created and owner-only.
///
/// For disposable state: crash-recovery swaps, trash, downloaded sources.
pub fn private_cache_subdir(app: &str, sub: &str) -> io::Result<PathBuf> {
    let dir = cache_dir(app).map_err(to_io)?.join(sub);
    ensure_private_dir(&dir)?;
    Ok(dir)
}

/// `<XDG_STATE_HOME>/<app>/<sub>`, created and owner-only.
///
/// For state the user would be annoyed to lose to a cache wipe: undo history,
/// cursor memory.
pub fn private_state_subdir(app: &str, sub: &str) -> io::Result<PathBuf> {
    let dir = state_dir(app).map_err(to_io)?.join(sub);
    ensure_private_dir(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_private_dir_creates_nested_path() {
        let td = tempfile::tempdir().unwrap();
        let nested = td.path().join("a").join("b").join("c");
        ensure_private_dir(&nested).unwrap();
        assert!(nested.is_dir());
    }

    #[test]
    fn ensure_private_dir_is_idempotent() {
        let td = tempfile::tempdir().unwrap();
        let d = td.path().join("x");
        ensure_private_dir(&d).unwrap();
        ensure_private_dir(&d).unwrap();
        assert!(d.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_dir_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let d = td.path().join("private");
        ensure_private_dir(&d).unwrap();
        let mode = std::fs::metadata(&d).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_dir_tightens_existing_loose_dir() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let d = td.path().join("loose");
        std::fs::create_dir(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&d).unwrap();
        let mode = std::fs::metadata(&d).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "existing dir not tightened, got {mode:o}");
    }
}
