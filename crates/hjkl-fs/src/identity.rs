//! Is this open handle still the object that path names?
//!
//! [`crate::atomic::probe_writable_nofollow`] answers the other half of the
//! question — "can I write here, without following a link to get there". It says
//! nothing once a handle is already open, and that is where the remaining window
//! is: any *open the file* then *validate the path* sequence can be split by a
//! swap. The handle stays pinned to whatever the path resolved to at open time,
//! while the validation inspects whatever the path resolves to *now*. A guard
//! that clears the new target while the I/O proceeds through the old handle has
//! passed on the wrong object.
//!
//! `O_NOFOLLOW` narrows this and does not close it: it covers the final component
//! at open time only, so it cannot see a rename, a directory substituted higher
//! up the path, or a swap performed after the open succeeded.
//!
//! # The primitive
//!
//! Every OS names a file object by a pair that is stable under renaming and
//! distinct between objects:
//!
//! - unix — `(st_dev, st_ino)`, reachable through
//!   [`std::os::unix::fs::MetadataExt`] with no FFI.
//! - Windows — `(dwVolumeSerialNumber, nFileIndexHigh:nFileIndexLow)` from
//!   `GetFileInformationByHandle`. `std` exposes those fields only behind the
//!   unstable `windows_by_handle` feature, so this module makes the raw call
//!   itself; `windows-sys` is a Windows-target-only dependency for exactly that.
//!
//! [`guard_not_swapped`] compares the pair behind the handle against the pair the
//! path resolves to now, and errors when they differ.
//!
//! [`hardlink_count`] rides along because on Windows it is `nNumberOfLinks` in
//! the *same* `BY_HANDLE_FILE_INFORMATION` — one raw call serves both — and it
//! answers a question a replace-by-rename writer already has: whether renaming
//! over this name would detach it from its siblings.
//!
//! # Order matters
//!
//! Open first, validate second, then call [`guard_not_swapped`]. The handle is
//! the fixed point; re-deriving it from the path afterwards would re-open the
//! very race being checked for.
//!
//! ```no_run
//! use hjkl_fs::guard_not_swapped;
//! use std::fs::File;
//! use std::path::Path;
//!
//! let path = Path::new("/srv/data/notes.md");
//! let file = File::open(path)?;
//! // ... whatever policy check the caller runs on `path` ...
//! guard_not_swapped(&file, path)?; // the check and the handle agree
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! Which files deserve the check, and what a failure means — refuse, retry, warn
//! — stay with the caller. This module owns the mechanism, not the policy.
//!
//! # Directories
//!
//! Both functions accept a directory: the identity pair is defined for one, and a
//! directory swapped for another directory is precisely the substitution
//! `O_NOFOLLOW` cannot see. [`hardlink_count`] reports the OS's own count, which
//! on unix is 2 plus the number of subdirectories (`.` and each child's `..`)
//! rather than 1. On Windows a directory handle requires
//! `FILE_FLAG_BACKUP_SEMANTICS`; [`hardlink_count`] opens with it, but a
//! directory handle a *caller* passes to [`guard_not_swapped`] must have been
//! opened with it too, or the caller never obtained the handle in the first
//! place.
//!
//! # What identity does not tell you
//!
//! Inode numbers are reused after deletion, and a `(dev, ino)` pair is only
//! unique per mounted filesystem at a point in time. The check is a guard against
//! a swap between two of *your own* operations, not a durable identifier to
//! record and compare against tomorrow.

use std::fs::File;
use std::io;
use std::path::Path;

/// The OS's name for a file object: which volume, and which object on it.
///
/// Widened to `u128` for the object half so the Windows 64-bit file index and
/// the unix inode both fit without a platform-dependent type in the comparison.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FileId {
    volume: u64,
    object: u128,
}

/// The error a mismatch produces.
///
/// [`io::ErrorKind::Other`] because no standard kind describes it, and the
/// message names the path so a log line is actionable without the call site
/// having to add it back.
fn swapped(path: &Path) -> io::Error {
    io::Error::other(format!(
        "{} no longer names the object the open handle refers to \
         (replaced, renamed, or the path now resolves elsewhere)",
        path.display()
    ))
}

// ---------------------------------------------------------------------------
// unix
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn id_of_metadata(meta: &std::fs::Metadata) -> FileId {
    use std::os::unix::fs::MetadataExt;
    FileId {
        volume: meta.dev(),
        object: u128::from(meta.ino()),
    }
}

#[cfg(unix)]
fn id_of_handle(file: &File) -> io::Result<FileId> {
    Ok(id_of_metadata(&file.metadata()?))
}

/// The identity the path resolves to *now*.
///
/// Symlinks are followed, because that is what the path names — the same
/// resolution an ordinary open would perform, which is the thing being compared
/// against.
#[cfg(unix)]
fn id_of_path(path: &Path) -> io::Result<FileId> {
    Ok(id_of_metadata(&std::fs::metadata(path)?))
}

#[cfg(unix)]
fn link_count(path: &Path) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::metadata(path)?.nlink())
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use super::FileId;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::mem::MaybeUninit;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
    };

    /// One `GetFileInformationByHandle` call — the volume serial, the file index
    /// and the link count all come from this single struct.
    pub(super) fn info(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
        let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: `file` owns a live handle for the whole borrow, so the raw
        // handle passed here is valid and is not closed under the call; the out
        // pointer addresses writable storage of exactly the right size and
        // alignment for the struct the API writes. Nothing is read from it
        // before the returned BOOL is checked below.
        let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call returned non-zero, and on success the API's contract
        // is that it has written every field of the struct, so it is fully
        // initialised.
        Ok(unsafe { info.assume_init() })
    }

    pub(super) fn id_from_info(info: &BY_HANDLE_FILE_INFORMATION) -> FileId {
        FileId {
            volume: u64::from(info.dwVolumeSerialNumber),
            object: u128::from(
                (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            ),
        }
    }

    /// A query-only handle on `path`.
    ///
    /// `FILE_READ_ATTRIBUTES` alone, because nothing here reads or writes
    /// contents; the full share mode so opening a file to inspect it never blocks
    /// another process's access to it; and `FILE_FLAG_BACKUP_SEMANTICS` so a
    /// directory can be opened at all. Reparse points are followed deliberately —
    /// the identity wanted is the one the path resolves to, matching the unix
    /// side's `fs::metadata`.
    pub(super) fn open_for_query(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    }
}

#[cfg(windows)]
fn id_of_handle(file: &File) -> io::Result<FileId> {
    Ok(win::id_from_info(&win::info(file)?))
}

#[cfg(windows)]
fn id_of_path(path: &Path) -> io::Result<FileId> {
    let probe = win::open_for_query(path)?;
    Ok(win::id_from_info(&win::info(&probe)?))
}

#[cfg(windows)]
fn link_count(path: &Path) -> io::Result<u64> {
    let probe = win::open_for_query(path)?;
    Ok(u64::from(win::info(&probe)?.nNumberOfLinks))
}

// ---------------------------------------------------------------------------
// Everywhere else
// ---------------------------------------------------------------------------

/// No identity pair is reachable off unix and Windows, and inventing one would
/// make a security check silently pass. Report the gap instead — the same
/// reasoning that put both platforms behind one helper in the first place.
#[cfg(not(any(unix, windows)))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "file identity is not available on this platform",
    )
}

#[cfg(not(any(unix, windows)))]
fn id_of_handle(_file: &File) -> io::Result<FileId> {
    Err(unsupported())
}

#[cfg(not(any(unix, windows)))]
fn id_of_path(_path: &Path) -> io::Result<FileId> {
    Err(unsupported())
}

#[cfg(not(any(unix, windows)))]
fn link_count(_path: &Path) -> io::Result<u64> {
    Err(unsupported())
}

// ---------------------------------------------------------------------------
// API
// ---------------------------------------------------------------------------

/// Prove `file` is still the object `path` names.
///
/// Callers open first, validate second, then call this — the order is what makes
/// the handle the fixed point. A mismatch means the path was made to point
/// somewhere else after the open, and any conclusion drawn about `path` no longer
/// applies to `file`.
///
/// The comparison is on the OS identity pair (`(st_dev, st_ino)` on unix, volume
/// serial plus file index on Windows), so it is not fooled by a rename, and it
/// *passes* for a hard link: two names for one object are one object, which is
/// the question being asked.
///
/// # Errors
///
/// - [`io::ErrorKind::NotFound`] (or another OS error) when `path` cannot be
///   resolved at all — a target deleted after the open reaches here.
/// - [`io::ErrorKind::Other`] when the path resolves to a *different* object,
///   with a message naming the path.
///
/// ```no_run
/// use hjkl_fs::guard_not_swapped;
/// use std::fs::File;
/// use std::path::Path;
///
/// let path = Path::new("/srv/data/notes.md");
/// let file = File::open(path)?;
/// guard_not_swapped(&file, path)?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn guard_not_swapped(file: &File, path: &Path) -> io::Result<()> {
    let opened = id_of_handle(file)?;
    let named = id_of_path(path)?;
    if opened == named {
        Ok(())
    } else {
        Err(swapped(path))
    }
}

/// How many directory entries name the object at `path`.
///
/// `1` for an ordinary file with no hard links. More than one means other names
/// share the object, so replacing this name by rename detaches it from them:
/// writers through the other names keep the old content, and readers through them
/// never see the new. That is vim's `backupcopy=auto` behaviour, and it is a
/// defensible default — this exists so a caller can make it a decision instead of
/// an unnoticed consequence.
///
/// Symlinks are followed; the count belongs to the object, not the link. A
/// directory reports the OS's own count — 2 plus its subdirectories on unix,
/// never `1`. Filesystems without hard links (FAT, some network mounts) report
/// `1` unconditionally, so a count of `1` is weaker evidence than a count above
/// it.
///
/// # Errors
///
/// Propagates the OS error when `path` cannot be resolved or opened.
///
/// ```no_run
/// use hjkl_fs::hardlink_count;
/// use std::path::Path;
///
/// if hardlink_count(Path::new("/srv/data/notes.md"))? > 1 {
///     // other names share this object; a rename here would detach them
/// }
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn hardlink_count(path: &Path) -> io::Result<u64> {
    link_count(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The baseline: nothing moved, so the handle and the name agree.
    #[test]
    fn same_file_and_path_pass() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("doc.txt");
        std::fs::write(&p, b"original").unwrap();
        let f = File::open(&p).unwrap();
        guard_not_swapped(&f, &p).unwrap();
    }

    /// The core case. A different file is renamed over the path while the handle
    /// stays open: the handle still refers to the original object, the name now
    /// refers to the impostor, and only an identity comparison can tell.
    #[test]
    fn file_replaced_by_rename_is_detected() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("doc.txt");
        std::fs::write(&p, b"original").unwrap();
        let f = File::open(&p).unwrap();
        guard_not_swapped(&f, &p).unwrap();

        // A second, distinct file swapped in under the same name.
        let impostor = td.path().join("impostor.txt");
        std::fs::write(&impostor, b"attacker").unwrap();
        std::fs::rename(&impostor, &p).unwrap();

        let err = guard_not_swapped(&f, &p).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other, "{err}");
        assert!(
            err.to_string().contains("doc.txt"),
            "message should name the path: {err}"
        );
    }

    /// The same swap spelled the other way round: the original is renamed away
    /// and a fresh file takes the old name.
    #[test]
    fn file_renamed_away_and_recreated_is_detected() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("doc.txt");
        std::fs::write(&p, b"original").unwrap();
        let f = File::open(&p).unwrap();

        std::fs::rename(&p, td.path().join("moved.txt")).unwrap();
        std::fs::write(&p, b"fresh").unwrap();

        assert!(guard_not_swapped(&f, &p).is_err());
    }

    /// A handle that was never the path's object at all.
    #[test]
    fn unrelated_file_at_path_is_detected() {
        let td = tempfile::tempdir().unwrap();
        let a = td.path().join("a.txt");
        let b = td.path().join("b.txt");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let f = File::open(&a).unwrap();
        assert!(guard_not_swapped(&f, &b).is_err());
    }

    /// A missing path is an ordinary `NotFound`, not a panic and not a silent
    /// pass — deleting the target is one of the ways it stops being the object.
    #[test]
    fn missing_path_is_not_found() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("doc.txt");
        std::fs::write(&p, b"original").unwrap();
        let f = File::open(&p).unwrap();
        let err = guard_not_swapped(&f, &td.path().join("never-existed")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
        assert_eq!(
            hardlink_count(&td.path().join("never-existed"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    /// Deleting the open file leaves the handle usable and the name gone; the
    /// guard must report that rather than pass on a handle nothing names.
    #[cfg(unix)]
    #[test]
    fn deleted_target_is_reported() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("doc.txt");
        std::fs::write(&p, b"original").unwrap();
        let f = File::open(&p).unwrap();
        std::fs::remove_file(&p).unwrap();
        assert_eq!(
            guard_not_swapped(&f, &p).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    /// Identity is the object, not the name: a second hard link is the same file,
    /// so the guard passes through either name. This is deliberate — a caller
    /// that cares about the *name* wants [`hardlink_count`], not this.
    #[cfg(unix)]
    #[test]
    fn hard_link_sibling_is_the_same_object() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("doc.txt");
        let link = td.path().join("also-doc.txt");
        std::fs::write(&p, b"original").unwrap();
        std::fs::hard_link(&p, &link).unwrap();
        let f = File::open(&p).unwrap();
        guard_not_swapped(&f, &link).unwrap();
    }

    /// A symlink resolves to its target, so pointing the guard at the link name
    /// still identifies the object behind it — and repointing the link is a swap.
    #[cfg(unix)]
    #[test]
    fn symlink_resolves_to_its_target_and_repointing_is_a_swap() {
        let td = tempfile::tempdir().unwrap();
        let real = td.path().join("real.txt");
        let other = td.path().join("other.txt");
        let link = td.path().join("link.txt");
        std::fs::write(&real, b"real").unwrap();
        std::fs::write(&other, b"other").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let f = File::open(&real).unwrap();
        guard_not_swapped(&f, &link).unwrap();

        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&other, &link).unwrap();
        assert!(guard_not_swapped(&f, &link).is_err());
    }

    #[test]
    fn plain_file_has_one_link() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("doc.txt");
        std::fs::write(&p, b"x").unwrap();
        assert_eq!(hardlink_count(&p).unwrap(), 1);
    }

    /// The whole point of the count: a rename over `p` would leave `link` holding
    /// the old content, and the caller can now see that before deciding.
    #[test]
    fn link_count_tracks_added_and_removed_links() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("doc.txt");
        let link = td.path().join("also-doc.txt");
        std::fs::write(&p, b"x").unwrap();
        assert_eq!(hardlink_count(&p).unwrap(), 1);

        std::fs::hard_link(&p, &link).unwrap();
        assert_eq!(hardlink_count(&p).unwrap(), 2);
        assert_eq!(
            hardlink_count(&link).unwrap(),
            2,
            "counted from either name"
        );

        std::fs::remove_file(&link).unwrap();
        assert_eq!(hardlink_count(&p).unwrap(), 1);
    }

    /// A directory is a legitimate subject: on unix its count is 2 plus its
    /// subdirectories, never the 1 a naive "is it linked?" test would expect.
    #[test]
    fn directory_link_count_is_the_os_count() {
        let td = tempfile::tempdir().unwrap();
        let n = hardlink_count(td.path()).unwrap();
        assert!(n >= 1, "got {n}");
        #[cfg(unix)]
        {
            assert!(
                n >= 2,
                "unix directories link `.` and each child's `..`: {n}"
            );
            std::fs::create_dir(td.path().join("child")).unwrap();
            assert_eq!(hardlink_count(td.path()).unwrap(), n + 1);
        }
    }

    /// A directory swapped for another directory is the substitution
    /// `O_NOFOLLOW` cannot see, and the guard does see it. Unix-only because a
    /// Windows directory handle needs `FILE_FLAG_BACKUP_SEMANTICS`, which
    /// `File::open` does not set — a caller there arrives with a handle it
    /// already opened that way.
    #[cfg(unix)]
    #[test]
    fn directory_swap_is_detected() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().join("data");
        let decoy = td.path().join("decoy");
        std::fs::create_dir(&dir).unwrap();
        std::fs::create_dir(&decoy).unwrap();

        let handle = File::open(&dir).unwrap();
        guard_not_swapped(&handle, &dir).unwrap();

        std::fs::remove_dir(&dir).unwrap();
        std::fs::rename(&decoy, &dir).unwrap();
        assert!(guard_not_swapped(&handle, &dir).is_err());
    }
}
