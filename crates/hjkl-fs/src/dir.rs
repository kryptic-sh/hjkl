//! Directory-level operations: staged recursive copy, swap-then-delete
//! replacement, type-correct removal, and a move that survives a filesystem
//! boundary.
//!
//! [`crate::atomic`] gives a *file* the property that a reader sees either the
//! old contents or the new ones, never a half-written mixture. A tree needs the
//! same property, and gets it the same way: the copy is built under a staging
//! name beside the destination and only becomes the destination once it is
//! complete. A copy that dies halfway leaves the destination exactly as it was —
//! the failure mode this crate exists to prevent is a partial result that *looks*
//! complete, and a half-populated directory is the worst version of that, because
//! nothing about it reads as damaged.
//!
//! Three shapes, previously hand-rolled at every call site that needed them
//! (anvil's package install, the explorer's trash):
//!
//! - [`copy_dir_atomic`] — stage, then swap.
//! - [`move_atomic`] — `rename`, falling back to copy-then-delete across devices.
//! - [`remove_path_all`] — remove whatever is at a path, without following a
//!   symlink through to someone else's data.
//!
//! # What `WriteOptions` means here
//!
//! - `temp_retries` — attempts at an unused staging name, as for a file write.
//! - `fsync_dir` — `fsync` the parent after the swap, so the *name* change is
//!   durable.
//! - `fsync` — `fsync` each copied file before it is swapped into place.
//! - `preserve_mode` — per-entry mode preservation (see [`copy_dir_atomic`]).
//! - `nonatomic_fallback` — **ignored**. For a file it means "an in-place write
//!   beats refusing to save"; for a tree the in-place version *is* the
//!   half-populated destination, so there is nothing to fall back to.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::atomic::{WriteOptions, copy_atomic, sync_parent, temp_path};

/// Find a staging/aside name beside `base` that nothing currently occupies.
///
/// `attempt` starts at 1 so [`temp_path`] always mixes in the process-unique
/// counter: every call returns a distinct name, and the pid in the name means no
/// other process can be staging under it. Two calls in one operation (the
/// staging tree and the aside slot for the old destination) therefore cannot
/// collide with each other.
///
/// This reserves nothing — the caller creates the directory (`create_dir` is an
/// atomic `mkdir`, so a lost race is reported as `AlreadyExists`) or renames onto
/// it. For the rename case the existence check is the guard: `rename` silently
/// clobbers its destination, so handing it a name that is already taken would
/// destroy whatever was there.
fn unused_temp_name(base: &Path, retries: u32) -> io::Result<PathBuf> {
    for attempt in 1..=retries.max(1) {
        let candidate = temp_path(base, attempt);
        if std::fs::symlink_metadata(&candidate).is_err() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no unused staging name after retries",
    ))
}

/// Apply an explicit mode to a copied **file**.
///
/// Directories deliberately never get [`WriteOptions::mode`]: it is a file mode,
/// and the obvious one to pass (`0600`, from [`WriteOptions::state`]) would make
/// the copied tree untraversable — a directory needs its execute bit to be
/// entered at all.
fn apply_file_mode(path: &Path, mode: Option<u32>) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

/// Copy `src`'s permission bits onto `dst`. No-op off Unix, where there is no
/// mode to carry over (see [`crate::dirs`] for where confidentiality comes from
/// there instead).
fn copy_mode(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let perms = std::fs::metadata(src)?.permissions();
        std::fs::set_permissions(dst, perms)?;
    }
    #[cfg(not(unix))]
    let _ = (src, dst);
    Ok(())
}

/// Recreate the symlink at `src` as a symlink at `dst`, pointing at the same
/// (unresolved) target.
///
/// Never followed: following would copy the target's *contents* into the tree,
/// and a subsequent [`move_atomic`] would then delete the original through the
/// link. The target is not required to exist — a dangling link is a legitimate
/// thing to reproduce, and resolving it would break a relative one.
fn copy_symlink(src: &Path, dst: &Path) -> io::Result<()> {
    let target = std::fs::read_link(src)?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, dst)
    }
    #[cfg(not(unix))]
    {
        let _ = (target, dst);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "cannot reproduce a symlink on this platform (Windows symlink creation requires \
             elevation or Developer Mode)",
        ))
    }
}

/// Copy everything under `from` into the already-created directory `into`.
///
/// Iterative, not recursive: a deep tree (or a symlink cycle, were links
/// followed) must not be able to overflow the stack.
///
/// Entries are classified with [`std::fs::DirEntry::file_type`], which does not
/// follow symlinks, so a link is reproduced as a link rather than as a copy of
/// whatever it points at.
///
/// # Modes
///
/// Regular files carry the **source's** mode, because [`std::fs::copy`] does and
/// [`copy_atomic`] preserves that behaviour — a `0755` binary in the source is a
/// `0755` binary in the copy. `opts.mode`, when set, overrides it per file.
/// Directories are created with the platform default (umask) unless
/// `opts.preserve_mode` is set, which is what "per-entry mode preservation"
/// means for a tree.
///
/// Directory modes are applied *after* the walk, deepest-first, so a source
/// directory that is read-only or non-writable (`0500`) does not stop its own
/// contents from being written first.
///
/// Timestamps and ownership are not reproduced; nothing in hjkl depends on them,
/// and preserving ownership needs privileges this crate never assumes.
fn copy_tree(from: &Path, into: &Path, opts: &WriteOptions) -> io::Result<()> {
    // (source, destination) pairs for every directory, in discovery order.
    let mut dirs: Vec<(PathBuf, PathBuf)> = vec![(from.to_path_buf(), into.to_path_buf())];
    let mut pending: Vec<(PathBuf, PathBuf)> = vec![(from.to_path_buf(), into.to_path_buf())];

    while let Some((src_dir, dst_dir)) = pending.pop() {
        for entry in std::fs::read_dir(&src_dir)? {
            let entry = entry?;
            let src = entry.path();
            let dst = dst_dir.join(entry.file_name());
            let file_type = entry.file_type()?;

            if file_type.is_symlink() {
                copy_symlink(&src, &dst)?;
            } else if file_type.is_dir() {
                std::fs::create_dir(&dst)?;
                dirs.push((src.clone(), dst.clone()));
                pending.push((src, dst));
            } else if file_type.is_file() {
                // One writable handle for the whole copy: `fs::copy` would
                // stamp the source's mode onto `dst` immediately, making a
                // read-only source impossible to fsync through a fresh
                // write-open. Copy the bytes through handles we own, flush
                // while the handle is still writable, and only then apply the
                // final mode.
                let mut reader = File::open(&src)?;
                let mut writer = File::create(&dst)?;
                std::io::copy(&mut reader, &mut writer)?;
                if opts.fsync {
                    writer.sync_all()?;
                }
                drop(writer);
                match opts.mode {
                    Some(_) => apply_file_mode(&dst, opts.mode)?,
                    None => copy_mode(&src, &dst)?,
                }
            } else {
                // A fifo, socket or device node. Refusing is the honest answer:
                // `fs::copy` on a fifo blocks forever waiting for a writer, and
                // silently skipping it would produce a copy that claims to be
                // complete and is not.
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("cannot copy {src:?}: not a file, directory or symlink"),
                ));
            }
        }
    }

    if opts.preserve_mode {
        // Reverse discovery order: a parent is always discovered before its
        // children, so this fixes every child before the parent that might
        // become unwritable.
        for (src, dst) in dirs.iter().rev() {
            copy_mode(src, dst)?;
        }
    }
    Ok(())
}

/// Make `staging` become `to`, replacing whatever is there now.
///
/// Three cases, in the order they are tried:
///
/// 1. **Nothing at `to`** — a single `rename`.
/// 2. **An empty directory at `to`** (the shape a caller that pre-reserved its
///    destination leaves, e.g. the explorer's trash slot) — POSIX lets `rename`
///    replace it in one step, so the reservation is never momentarily free for
///    another process to claim.
/// 3. **Anything else** — the swap-then-delete dance: move the old destination
///    aside, rename the new tree into place, then delete the old one. There is no
///    point at which `to` is missing for longer than a single `rename`, which is
///    what a `remove_dir_all` + `rename` pair would leave wide open. If the
///    second rename fails, the old destination is renamed back, so a partial swap
///    restores rather than losing both trees.
fn swap_into_place(staging: &Path, to: &Path, opts: &WriteOptions) -> io::Result<()> {
    if std::fs::symlink_metadata(to).is_ok() {
        // Case 2: cheap to attempt, and when the kernel takes it there is no
        // window at all. A non-empty directory, a file, or Windows (which will
        // not rename onto an existing directory) falls through to case 3.
        if std::fs::rename(staging, to).is_err() {
            let aside = unused_temp_name(to, opts.temp_retries)?;
            std::fs::rename(to, &aside)?;
            if let Err(e) = std::fs::rename(staging, to) {
                // Put the original back. The staging tree is cleaned up by the
                // caller, which owns it until this function returns `Ok`.
                let _ = std::fs::rename(&aside, to);
                return Err(e);
            }
            // The new tree is in place; the old one is now just garbage under a
            // staging name. Failing to delete it costs disk space, not
            // correctness, so it must not fail the operation.
            let _ = remove_path_all(&aside);
        }
    } else {
        std::fs::rename(staging, to)?;
    }

    if opts.fsync_dir {
        sync_parent(to);
    }
    Ok(())
}

/// Copy the directory tree `from` to `to`, atomically.
///
/// The tree is built under a staging name beside `to`
/// (`.<name>.hjkl-tmp.<pid>`, the same naming [`crate::atomic::write_atomic`]
/// uses, retried on collision rather than clobbered) and swapped into place only
/// once every entry has been copied. A failure at any point removes the staging
/// tree and leaves `to` exactly as it was — never a half-populated directory
/// that looks like a finished install.
///
/// When `to` already exists it is replaced by [`swap_into_place`]: the old tree
/// is moved aside, the new one renamed in, and only then is the old one deleted.
/// A reader is never left with no destination at all, and a swap that fails
/// partway restores the original.
///
/// # Contents
///
/// Symlinks are reproduced as symlinks, never followed. Files carry the source's
/// mode; `opts.preserve_mode` extends that to directories. Anything that is not a
/// file, directory or symlink (fifo, socket, device node) fails the copy rather
/// than being skipped — see [`copy_tree`].
///
/// # Errors
///
/// - `NotADirectory` if `from` is not a directory. A symlink *to* a directory is
///   rejected too: whether that means "copy the link" or "copy the tree" is the
///   caller's decision, not something to guess at.
/// - `InvalidInput` if `to` is inside `from` — copying a tree into itself would
///   otherwise walk into its own staging directory and never terminate.
pub fn copy_dir_atomic(from: &Path, to: &Path, opts: &WriteOptions) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if !meta.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{from:?} is not a directory"),
        ));
    }

    // Compare resolved paths: `to` can be spelled with a `..` that puts it back
    // inside `from`, and a plain `starts_with` would not see it.
    let from_resolved = crate::path::canonicalize_nearest(from);
    let to_resolved = crate::path::canonicalize_nearest(to);
    if to_resolved.starts_with(&from_resolved) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{to:?} is inside the source tree {from:?}"),
        ));
    }

    // `create_dir` is an atomic mkdir: a name taken by another process is
    // reported as `AlreadyExists` and retried, never assumed to be ours.
    let staging = {
        let mut last_err: Option<io::Error> = None;
        let mut created: Option<PathBuf> = None;
        for attempt in 0..opts.temp_retries.max(1) {
            let candidate = temp_path(to, attempt);
            match std::fs::create_dir(&candidate) {
                Ok(()) => {
                    created = Some(candidate);
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    last_err = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        match created {
            Some(p) => p,
            None => {
                return Err(last_err.unwrap_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "no unused staging name after retries",
                    )
                }));
            }
        }
    };

    if let Err(e) = copy_tree(from, &staging, opts) {
        let _ = remove_path_all(&staging);
        return Err(e);
    }
    if let Err(e) = swap_into_place(&staging, to, opts) {
        let _ = remove_path_all(&staging);
        return Err(e);
    }
    Ok(())
}

/// Remove whatever is at `path`: a file, a directory tree, or a symlink.
///
/// # Symlinks are removed, not followed
///
/// A symlink to a directory is unlinked — the directory it points at, and
/// everything in it, is untouched. This is the whole reason the function exists:
/// `remove_dir_all` on a path that turns out to be a symlink is an error, and
/// "check then choose" written at the call site is exactly where someone
/// eventually reaches through a link and deletes a user's real data. The type is
/// read with `symlink_metadata` (`lstat`), which does not resolve the final
/// component. `remove_dir_all` likewise does not follow links it finds *inside*
/// the tree.
///
/// # Idempotent
///
/// A path that is already gone is success: the postcondition is "nothing is at
/// this path", and callers cleaning up after a failure would otherwise all have
/// to special-case `NotFound` themselves.
pub fn remove_path_all(path: &Path) -> io::Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let file_type = meta.file_type();

    if file_type.is_symlink() {
        #[cfg(windows)]
        {
            // A Windows directory symlink (or junction) is a reparse point on a
            // directory: `remove_dir` unlinks it, `remove_file` refuses. Unix
            // `unlink` handles both, so this is the only place they differ.
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
            if meta.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
                return std::fs::remove_dir(path);
            }
        }
        return std::fs::remove_file(path);
    }
    if file_type.is_dir() {
        return std::fs::remove_dir_all(path);
    }
    std::fs::remove_file(path)
}

/// Copy `from` to `to` and only then remove `from` — the cross-device half of
/// [`move_atomic`].
///
/// Ordering is the whole point: the source is not touched until the destination
/// is complete and in place, so a failure anywhere in the copy leaves the source
/// intact and the caller having lost nothing. That is the opposite of the
/// `remove` + `copy` ordering, and of a copy that streams into the destination
/// directly, both of which can lose the only surviving version.
///
/// Each shape is delegated to the primitive that already stages it:
/// [`copy_dir_atomic`] for a tree, [`copy_atomic`] for a file,
/// [`crate::symlink_atomic`] for a link (whose *target string* is moved — the
/// data it points at is not copied and not deleted).
///
/// The verification before the delete is deliberately cheap: the destination
/// exists, and for a file its length matches. A short write is what a failing
/// disk produces, and it is the failure that a naive `copy` + `remove` turns
/// into silent data loss. Comparing contents would mean re-reading every byte of
/// something already `fsync`ed by the copy.
fn copy_then_delete(from: &Path, to: &Path, opts: &WriteOptions) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    let file_type = meta.file_type();

    if file_type.is_symlink() {
        let target = std::fs::read_link(from)?;
        crate::atomic::symlink_atomic(to, &target)?;
    } else if file_type.is_dir() {
        copy_dir_atomic(from, to, opts)?;
    } else if file_type.is_file() {
        copy_atomic(from, to, opts)?;
        let copied = std::fs::metadata(to)?;
        if copied.len() != meta.len() {
            // The source is still there, so this is a plain error rather than a
            // loss. Do not delete.
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "copy of {from:?} is {} bytes, source is {} — refusing to delete the source",
                    copied.len(),
                    meta.len()
                ),
            ));
        }
    } else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("cannot move {from:?}: not a file, directory or symlink"),
        ));
    }

    if std::fs::symlink_metadata(to).is_err() {
        return Err(io::Error::other(format!(
            "copy of {from:?} did not produce {to:?} — refusing to delete the source"
        )));
    }
    remove_path_all(from)
}

/// Move `from` to `to`, crossing a filesystem boundary if it has to.
///
/// `rename` is the fast path and the only genuinely atomic one: within a
/// filesystem the move either happened or it did not. Across filesystems the
/// kernel refuses (`EXDEV` → [`io::ErrorKind::CrossesDevices`]) and there is no
/// atomic answer at all, so the fallback picks the next best property — the
/// source survives until the destination is complete
/// ([`copy_then_delete`]). A trash directory under `$XDG_CACHE_HOME` and a file
/// on a mounted volume are the everyday version of this, and getting the
/// ordering wrong there loses the file the user asked to keep.
///
/// Works on a file, a directory tree or a symlink. On the fallback path a
/// symlink's target string is moved, not the data it points at.
///
/// `opts.fsync_dir` syncs both parents on the `rename` path: a move changes two
/// directory entries (the new name and the old one's disappearance), and only
/// syncing the destination would leave a crash able to resurrect the source
/// name.
pub fn move_atomic(from: &Path, to: &Path, opts: &WriteOptions) -> io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => {
            if opts.fsync_dir {
                sync_parent(to);
                if from.parent() != to.parent() {
                    sync_parent(from);
                }
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => copy_then_delete(from, to, opts),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a small nested tree with a file at every level.
    fn make_tree(root: &Path) {
        std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
        std::fs::write(root.join("top.txt"), b"top").unwrap();
        std::fs::write(root.join("sub/mid.txt"), b"mid").unwrap();
        std::fs::write(root.join("sub/deeper/leaf.txt"), b"leaf").unwrap();
    }

    /// Every `.hjkl-tmp` entry left in `dir` — none may survive a call, whether
    /// it succeeded or failed.
    fn temp_leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("hjkl-tmp"))
            .collect()
    }

    #[test]
    fn copies_nested_tree_faithfully() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src");
        let dst = td.path().join("dst");
        make_tree(&src);

        copy_dir_atomic(&src, &dst, &WriteOptions::default()).unwrap();

        assert_eq!(std::fs::read(dst.join("top.txt")).unwrap(), b"top");
        assert_eq!(std::fs::read(dst.join("sub/mid.txt")).unwrap(), b"mid");
        assert_eq!(
            std::fs::read(dst.join("sub/deeper/leaf.txt")).unwrap(),
            b"leaf"
        );
        assert!(dst.join("sub/deeper").is_dir(), "structure not reproduced");
        // The source is a copy source, not a move source.
        assert!(src.join("top.txt").is_file());
        assert!(temp_leftovers(td.path()).is_empty());
    }

    #[test]
    fn copy_dir_rejects_a_non_directory_source() {
        let td = tempfile::tempdir().unwrap();
        let file = td.path().join("f.txt");
        std::fs::write(&file, b"x").unwrap();
        let err = copy_dir_atomic(&file, &td.path().join("dst"), &WriteOptions::default())
            .expect_err("a file is not a tree");
        assert_eq!(err.kind(), io::ErrorKind::NotADirectory);
    }

    #[test]
    fn copy_dir_rejects_destination_inside_source() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src");
        make_tree(&src);
        // Spelled with a `..` so a lexical `starts_with` would not catch it.
        let inside = src.join("sub/../nested-copy");
        let err = copy_dir_atomic(&src, &inside, &WriteOptions::default())
            .expect_err("copying a tree into itself must be refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// A fifo cannot be copied, so it is a portable-on-Unix way to fail the copy
    /// *after* the staging tree exists — no permission games, and it works when
    /// the tests run as root.
    #[cfg(unix)]
    #[test]
    fn failure_mid_copy_leaves_no_destination_and_no_staging() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src");
        let dst = td.path().join("dst");
        make_tree(&src);
        let fifo = std::ffi::CString::new(src.join("pipe").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

        let err = copy_dir_atomic(&src, &dst, &WriteOptions::default())
            .expect_err("a fifo in the tree must fail the copy");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);

        assert!(!dst.exists(), "a failed copy must leave no destination");
        assert!(
            temp_leftovers(td.path()).is_empty(),
            "staging left behind: {:?}",
            temp_leftovers(td.path())
        );
    }

    #[test]
    fn failure_mid_copy_leaves_an_existing_destination_intact() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src");
        let dst = td.path().join("dst");
        make_tree(&src);
        // A destination that already holds something the copy must not damage.
        std::fs::create_dir(&dst).unwrap();
        std::fs::write(dst.join("precious.txt"), b"keep me").unwrap();

        // Unreadable subdirectory: `read_dir` fails partway through the walk.
        // Skipped as root, where mode bits do not deny anything.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(src.join("sub"), std::fs::Permissions::from_mode(0o000))
                .unwrap();
            let readable = std::fs::read_dir(src.join("sub")).is_ok();
            if !readable {
                assert!(copy_dir_atomic(&src, &dst, &WriteOptions::default()).is_err());
            }
            std::fs::set_permissions(src.join("sub"), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }

        assert_eq!(std::fs::read(dst.join("precious.txt")).unwrap(), b"keep me");
        assert!(temp_leftovers(td.path()).is_empty());
    }

    #[test]
    fn replaces_an_existing_destination_wholesale() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src");
        let dst = td.path().join("dst");
        make_tree(&src);
        std::fs::create_dir_all(dst.join("stale-dir")).unwrap();
        std::fs::write(dst.join("stale.txt"), b"stale").unwrap();

        copy_dir_atomic(&src, &dst, &WriteOptions::default()).unwrap();

        // Not a merge: the old contents are gone, not left alongside the new.
        assert!(!dst.join("stale.txt").exists(), "old entry survived");
        assert!(!dst.join("stale-dir").exists(), "old subdir survived");
        assert_eq!(std::fs::read(dst.join("top.txt")).unwrap(), b"top");
        assert!(temp_leftovers(td.path()).is_empty());
    }

    /// The shape a caller that pre-reserved its destination leaves behind (the
    /// explorer's trash slot): an empty directory must be replaced too.
    #[test]
    fn replaces_an_empty_reserved_destination() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src");
        let dst = td.path().join("reserved");
        make_tree(&src);
        std::fs::create_dir(&dst).unwrap();

        copy_dir_atomic(&src, &dst, &WriteOptions::default()).unwrap();
        assert_eq!(std::fs::read(dst.join("top.txt")).unwrap(), b"top");
        assert!(temp_leftovers(td.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn reproduces_symlinks_as_symlinks() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src");
        let dst = td.path().join("dst");
        make_tree(&src);
        std::os::unix::fs::symlink("top.txt", src.join("link")).unwrap();
        std::os::unix::fs::symlink("nowhere", src.join("dangling")).unwrap();

        copy_dir_atomic(&src, &dst, &WriteOptions::default()).unwrap();

        let meta = std::fs::symlink_metadata(dst.join("link")).unwrap();
        assert!(meta.file_type().is_symlink(), "link became a regular file");
        assert_eq!(
            std::fs::read_link(dst.join("link")).unwrap(),
            Path::new("top.txt")
        );
        // A dangling link is reproduced rather than failing the copy.
        assert!(
            std::fs::symlink_metadata(dst.join("dangling"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn files_carry_the_source_mode() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src");
        let dst = td.path().join("dst");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("tool"), b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(src.join("tool"), std::fs::Permissions::from_mode(0o755)).unwrap();

        copy_dir_atomic(&src, &dst, &WriteOptions::default()).unwrap();

        let mode = std::fs::metadata(dst.join("tool"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755, "executable bit lost, got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn preserve_mode_carries_directory_modes() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src");
        let dst = td.path().join("dst");
        std::fs::create_dir_all(src.join("locked")).unwrap();
        std::fs::write(src.join("locked/inside.txt"), b"x").unwrap();
        // 0500: readable and traversable, but not writable — the copy must still
        // manage to write `inside.txt` before the mode is applied.
        std::fs::set_permissions(src.join("locked"), std::fs::Permissions::from_mode(0o500))
            .unwrap();

        let opts = WriteOptions {
            preserve_mode: true,
            ..WriteOptions::default()
        };
        copy_dir_atomic(&src, &dst, &opts).unwrap();

        assert_eq!(std::fs::read(dst.join("locked/inside.txt")).unwrap(), b"x");
        let mode = std::fs::metadata(dst.join("locked"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o500, "directory mode not preserved, got {mode:o}");

        // Leave the tree removable by the TempDir drop.
        std::fs::set_permissions(src.join("locked"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::set_permissions(dst.join("locked"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }

    #[test]
    fn move_atomic_moves_a_file_on_one_filesystem() {
        let td = tempfile::tempdir().unwrap();
        let from = td.path().join("a.txt");
        let to = td.path().join("b.txt");
        std::fs::write(&from, b"payload").unwrap();

        move_atomic(&from, &to, &WriteOptions::default()).unwrap();

        assert_eq!(std::fs::read(&to).unwrap(), b"payload");
        assert!(!from.exists(), "source must be gone after a move");
    }

    #[test]
    fn move_atomic_moves_a_tree_on_one_filesystem() {
        let td = tempfile::tempdir().unwrap();
        let from = td.path().join("src");
        let to = td.path().join("moved");
        make_tree(&from);

        move_atomic(&from, &to, &WriteOptions::default()).unwrap();

        assert_eq!(
            std::fs::read(to.join("sub/deeper/leaf.txt")).unwrap(),
            b"leaf"
        );
        assert!(!from.exists(), "source tree must be gone after a move");
    }

    #[test]
    fn move_atomic_onto_an_empty_reserved_directory() {
        let td = tempfile::tempdir().unwrap();
        let from = td.path().join("src");
        let to = td.path().join("reserved");
        make_tree(&from);
        std::fs::create_dir(&to).unwrap();

        move_atomic(&from, &to, &WriteOptions::default()).unwrap();
        assert_eq!(std::fs::read(to.join("top.txt")).unwrap(), b"top");
        assert!(!from.exists());
    }

    #[test]
    fn move_atomic_reports_a_missing_source() {
        let td = tempfile::tempdir().unwrap();
        let err = move_atomic(
            &td.path().join("nope"),
            &td.path().join("dst"),
            &WriteOptions::default(),
        )
        .expect_err("missing source must error");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    // ── The cross-device fallback ────────────────────────────────────────────
    //
    // `EXDEV` cannot be produced portably: it needs two real filesystems, and a
    // unit test cannot mount one. So the fallback is tested by calling it
    // directly — it is the entire body of the `CrossesDevices` arm — and, on
    // Linux, additionally through `move_atomic` itself whenever `/dev/shm`
    // happens to be a different device from the temp directory.

    #[test]
    fn copy_then_delete_moves_a_file_and_removes_the_source() {
        let td = tempfile::tempdir().unwrap();
        let from = td.path().join("a.txt");
        let to = td.path().join("b.txt");
        std::fs::write(&from, b"payload").unwrap();

        copy_then_delete(&from, &to, &WriteOptions::default()).unwrap();

        assert_eq!(std::fs::read(&to).unwrap(), b"payload");
        assert!(!from.exists());
        assert!(temp_leftovers(td.path()).is_empty());
    }

    #[test]
    fn copy_then_delete_moves_a_tree_and_removes_the_source() {
        let td = tempfile::tempdir().unwrap();
        let from = td.path().join("src");
        let to = td.path().join("dst");
        make_tree(&from);

        copy_then_delete(&from, &to, &WriteOptions::default()).unwrap();

        assert_eq!(std::fs::read(to.join("sub/mid.txt")).unwrap(), b"mid");
        assert!(!from.exists(), "source tree must be gone");
        assert!(temp_leftovers(td.path()).is_empty());
    }

    /// The property that matters most on this path: a copy that cannot finish
    /// must leave the source completely intact.
    #[cfg(unix)]
    #[test]
    fn copy_then_delete_keeps_the_source_when_the_copy_fails() {
        let td = tempfile::tempdir().unwrap();
        let from = td.path().join("src");
        let to = td.path().join("dst");
        make_tree(&from);
        let fifo = std::ffi::CString::new(from.join("pipe").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

        assert!(copy_then_delete(&from, &to, &WriteOptions::default()).is_err());

        assert_eq!(std::fs::read(from.join("sub/mid.txt")).unwrap(), b"mid");
        assert!(from.join("top.txt").is_file(), "source damaged");
        assert!(!to.exists(), "no partial destination");
    }

    #[cfg(unix)]
    #[test]
    fn copy_then_delete_moves_a_symlink_without_touching_its_target() {
        let td = tempfile::tempdir().unwrap();
        let target = td.path().join("real.txt");
        std::fs::write(&target, b"real").unwrap();
        let from = td.path().join("link");
        let to = td.path().join("moved-link");
        std::os::unix::fs::symlink(&target, &from).unwrap();

        copy_then_delete(&from, &to, &WriteOptions::default()).unwrap();

        assert_eq!(std::fs::read_link(&to).unwrap(), target);
        assert!(
            !std::fs::symlink_metadata(&from).is_ok(),
            "source link left"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"real",
            "the link's target must be untouched"
        );
    }

    /// A genuine `EXDEV`, when the machine offers one: `/dev/shm` is a separate
    /// tmpfs on essentially every Linux box, but the test is a no-op rather than
    /// a failure where it is not (containers, `/dev/shm` absent or same-device).
    #[cfg(target_os = "linux")]
    #[test]
    fn move_atomic_across_a_real_device_boundary_when_available() {
        use std::os::unix::fs::MetadataExt;

        let shm = Path::new("/dev/shm");
        let Ok(shm_meta) = std::fs::metadata(shm) else {
            return;
        };
        let td = tempfile::tempdir().unwrap();
        if std::fs::metadata(td.path()).unwrap().dev() == shm_meta.dev() {
            return; // same filesystem — this would only test `rename` again
        }
        let Ok(shm_dir) = tempfile::tempdir_in(shm) else {
            return; // not writable here
        };

        let from = shm_dir.path().join("tree");
        let to = td.path().join("landed");
        make_tree(&from);

        // Proof that this run is testing the fallback and not silently taking
        // the `rename` path: the bare rename must fail with exactly the error
        // `move_atomic` catches. It changes nothing when it fails.
        assert_eq!(
            std::fs::rename(&from, &to).unwrap_err().kind(),
            io::ErrorKind::CrossesDevices,
        );

        move_atomic(&from, &to, &WriteOptions::default()).unwrap();

        assert_eq!(
            std::fs::read(to.join("sub/deeper/leaf.txt")).unwrap(),
            b"leaf"
        );
        assert!(
            !from.exists(),
            "source must be gone after a cross-device move"
        );
        assert!(temp_leftovers(td.path()).is_empty());
        assert!(temp_leftovers(shm_dir.path()).is_empty());
    }

    // ── remove_path_all ──────────────────────────────────────────────────────

    #[test]
    fn removes_a_file() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("f.txt");
        std::fs::write(&p, b"x").unwrap();
        remove_path_all(&p).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn removes_a_populated_directory() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("tree");
        make_tree(&p);
        remove_path_all(&p).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn removing_a_missing_path_is_success() {
        let td = tempfile::tempdir().unwrap();
        remove_path_all(&td.path().join("never-existed")).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn removes_a_symlink_to_a_file_and_keeps_the_file() {
        let td = tempfile::tempdir().unwrap();
        let real = td.path().join("real.txt");
        std::fs::write(&real, b"keep").unwrap();
        let link = td.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        remove_path_all(&link).unwrap();

        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "link not removed"
        );
        assert_eq!(std::fs::read(&real).unwrap(), b"keep");
    }

    /// The dangerous one. A symlink pointing at a populated directory somewhere
    /// else must be *unlinked*, not walked into: deleting the link's contents
    /// would delete a user's real data through a name that only claims to be a
    /// directory.
    #[cfg(unix)]
    #[test]
    fn removes_a_symlink_to_a_populated_directory_without_touching_it() {
        let outside = tempfile::tempdir().unwrap();
        make_tree(outside.path());
        let td = tempfile::tempdir().unwrap();
        let link = td.path().join("looks-like-a-dir");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();

        remove_path_all(&link).unwrap();

        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "the link itself must be removed"
        );
        assert_eq!(
            std::fs::read(outside.path().join("sub/deeper/leaf.txt")).unwrap(),
            b"leaf",
            "the linked-to tree must be untouched"
        );
        assert!(outside.path().join("top.txt").is_file());
    }

    /// Same guarantee one level in: removing a tree that *contains* such a link
    /// removes the link, not the tree it points at.
    #[cfg(unix)]
    #[test]
    fn removing_a_tree_does_not_delete_through_a_symlink_inside_it() {
        let outside = tempfile::tempdir().unwrap();
        make_tree(outside.path());
        let td = tempfile::tempdir().unwrap();
        let tree = td.path().join("tree");
        make_tree(&tree);
        std::os::unix::fs::symlink(outside.path(), tree.join("escape")).unwrap();

        remove_path_all(&tree).unwrap();

        assert!(!tree.exists());
        assert_eq!(
            std::fs::read(outside.path().join("sub/mid.txt")).unwrap(),
            b"mid",
            "deleted through the link"
        );
    }

    // ── read-only file in a copied tree (B3) ──────────────────────────────

    /// `copy_dir_atomic` must not fail when the source tree contains a
    /// read-only (0444) file. Before the fix, `std::fs::copy` stamped the
    /// read-only mode onto the destination immediately and the subsequent
    /// `.write(true).open(...)` for fsync failed `EACCES`.
    #[cfg(unix)]
    #[test]
    fn copy_dir_atomic_copies_tree_with_readonly_file() {
        use std::os::unix::fs::PermissionsExt;

        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src");
        let dst = td.path().join("dst");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("top.txt"), b"hello").unwrap();
        std::fs::write(src.join("sub/ro.txt"), b"world").unwrap();
        let mut perms = std::fs::metadata(src.join("sub/ro.txt"))
            .unwrap()
            .permissions();
        perms.set_mode(0o444);
        std::fs::set_permissions(src.join("sub/ro.txt"), perms).unwrap();

        // Default options: fsync on — this is the path that used to fail.
        copy_dir_atomic(&src, &dst, &WriteOptions::default()).unwrap();

        assert_eq!(std::fs::read(dst.join("top.txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(dst.join("sub/ro.txt")).unwrap(), b"world");
        let dst_mode = std::fs::metadata(dst.join("sub/ro.txt"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            dst_mode & 0o777,
            0o444,
            "destination's read-only file must carry the source's mode (0444); got {dst_mode:#o}"
        );
    }
}
