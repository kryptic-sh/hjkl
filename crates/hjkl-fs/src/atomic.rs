//! Atomic file writes: temp file → fsync → rename → fsync parent.
//!
//! One implementation for every on-disk write hjkl makes. Before this crate the
//! same sequence existed seven times over (swap, undofile, filestate, config,
//! buffer save, plus bonsai's `copy_atomic` and anvil's `atomic_symlink`), each
//! with slightly different guarantees — only the buffer-save path carried the
//! `O_NOFOLLOW` symlink guard, only two forced `0600`. Divergence like that is
//! how a security property gets fixed in one path and missed in another.
//!
//! The rename is what makes the write atomic: a reader either sees the old file
//! or the new one, never a partial write. `fsync` on the temp file before the
//! rename orders the data ahead of the name change, and `fsync` on the parent
//! directory makes the rename itself durable — without it a crash can leave the
//! directory entry unwritten even though the file's data reached disk.

use std::fs::File;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// How the temp file is created and what happens if the rename can't be done.
#[derive(Clone, Debug)]
pub struct WriteOptions {
    /// Unix mode for the temp file. `None` leaves it to the process umask.
    pub mode: Option<u32>,
    /// Copy the existing target's permission bits onto the temp file, so
    /// replacing a `0755` script keeps it executable. No-op when the target
    /// does not exist yet (umask, or [`Self::mode`], then applies).
    pub preserve_mode: bool,
    /// `fsync` the temp file before renaming.
    pub fsync: bool,
    /// `fsync` the parent directory after renaming, making the rename durable.
    pub fsync_dir: bool,
    /// Fall back to a **non-atomic** in-place truncate-and-write when the
    /// atomic path cannot run at all — a cross-device rename, or a parent
    /// directory that permits writing the file but not creating a temp
    /// alongside it. This path uses `File::create` (O_TRUNC), so a mid-write
    /// failure (e.g. ENOSPC) **can** leave the target truncated — the atomic
    /// path's guarantee does not extend here.
    pub nonatomic_fallback: bool,
    /// Attempts to find an unused temp filename before giving up.
    pub temp_retries: u32,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            mode: None,
            preserve_mode: false,
            fsync: true,
            fsync_dir: true,
            nonatomic_fallback: false,
            temp_retries: 5,
        }
    }
}

impl WriteOptions {
    /// hjkl's own state (swap, undofile, filestate, config): owner-only, fully
    /// durable, never falls back to a non-atomic write.
    ///
    /// `0600` because swap bodies and undo history contain whatever the user was
    /// editing — credentials and private keys included.
    pub fn state() -> Self {
        Self {
            mode: Some(0o600),
            ..Self::default()
        }
    }

    /// A user's document (`:w`): keep the file's existing mode, and accept a
    /// non-atomic write rather than refusing to save at all.
    pub fn document() -> Self {
        Self {
            preserve_mode: true,
            nonatomic_fallback: true,
            ..Self::default()
        }
    }

    /// Set [`Self::mode`].
    pub fn with_mode(mut self, mode: u32) -> Self {
        self.mode = Some(mode);
        self
    }

    /// Set [`Self::fsync`] and [`Self::fsync_dir`] together.
    pub fn with_fsync(mut self, fsync: bool) -> Self {
        self.fsync = fsync;
        self.fsync_dir = fsync;
        self
    }
}

/// Process-unique counter so concurrent writers in one process never collide on
/// a temp name even within the same nanosecond.
fn next_temp_seq() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Temp path beside `target`: `.<name>.hjkl-tmp.<pid>` (plus a counter on
/// retries).
///
/// Beside, not in a temp dir, because `rename` must stay on one filesystem. The
/// leading dot keeps it out of directory listings and of hjkl's own explorer.
///
/// Shared with [`crate::dir`] so a staged *tree* is named the same way a staged
/// file is: one naming scheme means one thing to recognize (and one thing to
/// clean up) when a process dies mid-write.
pub(crate) fn temp_path(target: &Path, attempt: u32) -> PathBuf {
    let name = target
        .file_name()
        .map_or_else(|| "hjkl".to_string(), |n| n.to_string_lossy().into_owned());
    let pid = std::process::id();
    let leaf = if attempt == 0 {
        format!(".{name}.hjkl-tmp.{pid}")
    } else {
        format!(".{name}.hjkl-tmp.{pid}.{}", next_temp_seq())
    };
    target.with_file_name(leaf)
}

/// Open the temp file with `create_new` (`O_EXCL`) so an existing file is never
/// clobbered and the requested mode is genuinely applied at creation.
///
/// The mode is applied by [`crate::open::options_with_mode`], the crate's one
/// cfg-gated `mode` call; `None` still means "leave it to the umask".
fn open_temp(path: &Path, mode: Option<u32>) -> io::Result<File> {
    crate::open::options_with_mode(mode)
        .write(true)
        .create_new(true)
        .open(path)
}

/// `fsync` the directory holding `path`, making a rename into it durable.
///
/// Best-effort: some filesystems reject `fsync` on a directory handle, and a
/// failure costs durability of the *name*, not integrity of the data.
pub(crate) fn sync_parent(path: &Path) {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let dir = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
}

/// Write the whole payload in place, without a temp file. Not atomic.
fn write_in_place<F>(target: &Path, opts: &WriteOptions, fill: &F) -> io::Result<()>
where
    F: Fn(&mut File) -> io::Result<()>,
{
    let mut file = File::create(target)?;
    fill(&mut file)?;
    if opts.fsync {
        file.sync_all()?;
    }
    Ok(())
}

/// Write `target` atomically, streaming the contents through `fill`.
///
/// `fill` receives the temp file and must write the complete contents. Prefer
/// this over [`write_atomic`] when the payload is not already one contiguous
/// buffer — a rope's chunks, or a length-prefixed record stream — so it is never
/// materialized just to be handed over.
///
/// `fill` is `Fn`, not `FnOnce`, because it may need to run again: on a temp-name
/// collision, and on the non-atomic fallback path. It must therefore write the
/// same bytes each time it is called.
///
/// On any failure the temp file is removed, so a failed write leaves neither a
/// damaged target nor a stray temp behind.
pub fn write_atomic_with<F>(target: &Path, opts: &WriteOptions, fill: F) -> io::Result<()>
where
    F: Fn(&mut File) -> io::Result<()>,
{
    let mut last_err: Option<io::Error> = None;

    for attempt in 0..opts.temp_retries.max(1) {
        let tmp = temp_path(target, attempt);
        let mut file = match open_temp(&tmp, opts.mode) {
            Ok(f) => f,
            // Someone holds that exact temp name; try the next one.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                last_err = Some(e);
                continue;
            }
            // No temp file could be created at all (read-only parent, missing
            // directory). Nothing has been written, so falling back is safe.
            Err(e) => {
                return if opts.nonatomic_fallback {
                    write_in_place(target, opts, &fill)
                } else {
                    Err(e)
                };
            }
        };

        let written = (|| -> io::Result<()> {
            if opts.preserve_mode
                && let Ok(meta) = std::fs::metadata(target)
            {
                file.set_permissions(meta.permissions())?;
            }
            fill(&mut file)?;
            if opts.fsync {
                file.sync_all()?;
            }
            Ok(())
        })();

        // The temp now exists. A write or sync failure here must NOT fall back
        // to an in-place write — that would turn an I/O error into data loss.
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        drop(file);

        match std::fs::rename(&tmp, target) {
            Ok(()) => {
                if opts.fsync_dir {
                    sync_parent(target);
                }
                return Ok(());
            }
            // Cross-device rename: the temp landed on another filesystem. The
            // target is still untouched, so an in-place write is safe here.
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices && opts.nonatomic_fallback => {
                let _ = std::fs::remove_file(&tmp);
                return write_in_place(target, opts, &fill);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "no unused temp filename after retries",
        )
    }))
}

/// Write `bytes` to `target` atomically.
pub fn write_atomic(target: &Path, bytes: &[u8], opts: &WriteOptions) -> io::Result<()> {
    write_atomic_with(target, opts, |f| f.write_all(bytes))
}

/// Copy `from` to `to` atomically: temp → fsync → rename → fsync parent.
///
/// The bytes are streamed with [`std::io::copy`], so a large artifact (a
/// compiled grammar `.so`) is never materialized in memory just to be handed to
/// [`write_atomic`]. `from` is opened *inside* the fill closure because that
/// closure may run more than once — a temp-name collision retries it, and the
/// non-atomic fallback re-runs it — and a reader consumed by the first attempt
/// would hand the second attempt an empty file.
///
/// # Mode
///
/// This replaces [`std::fs::copy`], which copies the **source's** permission
/// bits onto the destination. That behaviour is preserved: when `opts.mode` is
/// `None` and `opts.preserve_mode` is `false`, the source file's mode is read
/// and applied to the temp file — at creation (so the temp is never briefly
/// more permissive than the source) and again explicitly afterwards (so the
/// result is exact rather than umask-trimmed). An explicit `opts.mode` wins,
/// and `opts.preserve_mode` keeps the *destination's* existing mode instead —
/// both mean the caller has asked for something other than the source's bits.
/// Non-Unix platforms have no mode to copy; the destination is created with
/// whatever the platform's defaults are.
///
/// On any failure the temp file is removed, so a failed copy leaves neither a
/// damaged destination nor a stray temp behind.
pub fn copy_atomic(from: &Path, to: &Path, opts: &WriteOptions) -> io::Result<()> {
    // Only consulted on Unix; elsewhere there is no mode to carry over.
    #[cfg(unix)]
    let source_mode: Option<u32> = if opts.mode.is_none() && !opts.preserve_mode {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(from)
            .ok()
            .map(|m| m.permissions().mode() & 0o7777)
    } else {
        None
    };
    #[cfg(not(unix))]
    let source_mode: Option<u32> = None;

    let effective = if source_mode.is_some() {
        WriteOptions {
            mode: source_mode,
            ..opts.clone()
        }
    } else {
        opts.clone()
    };

    write_atomic_with(to, &effective, |f| {
        let mut src = File::open(from)?;
        io::copy(&mut src, f)?;
        // `open_temp` applies the mode through the umask, which can only clear
        // bits; set it exactly so a 0755 source lands as 0755 the way
        // `fs::copy` would. Also covers the non-atomic fallback path, where the
        // file is created by `File::create` with no mode at all.
        #[cfg(unix)]
        if let Some(mode) = source_mode {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        Ok(())
    })
}

/// Attempts to find an unused temp name for a symlink before giving up.
///
/// Matches [`WriteOptions::temp_retries`]'s default; there is no options struct
/// here because a symlink has no contents, no mode and nothing to fsync.
///
/// Unix-gated to match its only use: the non-unix arm of [`symlink_atomic`]
/// returns `Unsupported` without staging anything, and an unconditional
/// constant would be dead code there — which `-D warnings` rejects.
#[cfg(unix)]
const SYMLINK_TEMP_RETRIES: u32 = 5;

/// Point `link_path` at `target`, replacing any existing link atomically.
///
/// The symlink is created at a sibling temp path and then renamed over
/// `link_path`, so a reader either sees the old link or the new one — never a
/// window with no link at all, which a `remove_file` + `symlink` pair would
/// leave. The temp shares [`write_atomic_with`]'s naming
/// (`.<name>.hjkl-tmp.<pid>`): appended to the *whole* file name, so `foo.sh`
/// and `foo.py` get distinct staging paths, and carrying the pid so two
/// processes linking the same path cannot pick the same temp. A name already
/// taken is retried rather than removed — the temp is never assumed to be ours.
///
/// `target` is not resolved or required to exist: a dangling symlink is a
/// legitimate thing to create, and resolving it here would break relative
/// targets.
///
/// The rename is within one directory, so it can never cross a filesystem
/// boundary and needs no `EXDEV` fallback.
///
/// # Platforms
///
/// Unix only. On Windows, symlink creation needs elevation or Developer Mode,
/// so this returns [`io::ErrorKind::Unsupported`] rather than failing somewhere
/// less legible.
pub fn symlink_atomic(link_path: &Path, target: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut last_err: Option<io::Error> = None;
        for attempt in 0..SYMLINK_TEMP_RETRIES {
            let tmp = temp_path(link_path, attempt);
            match std::os::unix::fs::symlink(target, &tmp) {
                Ok(()) => {}
                // Someone holds that exact temp name; try the next one rather
                // than deleting a link we do not own.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    last_err = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            }
            return match std::fs::rename(&tmp, link_path) {
                Ok(()) => {
                    // Durability of the *name* is the whole point of a symlink,
                    // so the parent sync is unconditional here.
                    sync_parent(link_path);
                    Ok(())
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    Err(e)
                }
            };
        }
        Err(last_err.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "no unused temp filename after retries",
            )
        }))
    }
    #[cfg(not(unix))]
    {
        let _ = (link_path, target);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symlinks are not supported on this platform (Windows symlink creation requires \
             elevation or Developer Mode)",
        ))
    }
}

/// Fail unless `target` is writable, without following a symlink to get there.
///
/// `:w` must still fail on a file the user cannot write, which temp+rename would
/// otherwise bypass entirely — the rename replaces the directory entry and never
/// consults the old file's mode. A single non-truncating write-open answers the
/// question, and `O_NOFOLLOW` additionally means that if the target was swapped
/// for a symlink after being resolved, the open fails rather than following the
/// link to an unintended file.
///
/// `NotFound` is success: a brand-new file has no permissions to violate.
///
/// The options come from [`crate::owner_only_options_no_follow`] — the probe
/// never passes `create`, so the owner-only mode it carries is inert here (the OS
/// only consults a mode when the open creates the file) and `O_NOFOLLOW` is the
/// part that matters.
pub fn probe_writable_nofollow(target: &Path) -> io::Result<()> {
    match crate::open::owner_only_options_no_follow()
        .write(true)
        .open(target)
    {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_replaces_contents() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("f.bin");
        write_atomic(&p, b"first", &WriteOptions::state()).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"first");
        write_atomic(&p, b"second", &WriteOptions::state()).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"second");
    }

    #[test]
    fn leaves_no_temp_file_behind() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("f.bin");
        write_atomic(&p, b"x", &WriteOptions::state()).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(td.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("hjkl-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }

    #[test]
    fn streaming_writer_composes_payload() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("f.bin");
        write_atomic_with(&p, &WriteOptions::state(), |f| {
            f.write_all(b"a")?;
            f.write_all(b"b")?;
            f.write_all(b"c")
        })
        .unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"abc");
    }

    #[test]
    fn failed_fill_leaves_target_untouched_and_no_temp() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("f.bin");
        write_atomic(&p, b"original", &WriteOptions::state()).unwrap();
        let err = write_atomic_with(&p, &WriteOptions::state(), |_| {
            Err(io::Error::other("fill failed"))
        });
        assert!(err.is_err());
        // Original survives — the rename never happened.
        assert_eq!(std::fs::read(&p).unwrap(), b"original");
        let leftover = std::fs::read_dir(td.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("hjkl-tmp"));
        assert!(!leftover, "temp file left after failed fill");
    }

    #[test]
    fn missing_parent_errors_without_fallback() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("no-such-dir").join("f.bin");
        assert!(write_atomic(&p, b"x", &WriteOptions::state()).is_err());
    }

    #[test]
    fn document_write_succeeds_on_normal_file() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("doc.txt");
        write_atomic(&p, b"hello", &WriteOptions::document()).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"hello");
    }

    /// Genuinely reach `write_in_place`: a directory that permits writing
    /// the existing target file but NOT creating a temp file beside it
    /// (parent chmod 0555). The atomic path fails to create the temp, the
    /// fallback opens the existing target for write, and the write lands.
    #[cfg(unix)]
    #[test]
    fn nonatomic_fallback_writes_into_unwritable_parent() {
        use std::os::unix::fs::PermissionsExt;

        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("inside.txt");
        std::fs::write(&p, b"old").unwrap();

        // Make the parent non-writable so temp-file creation fails.
        let mut perms = std::fs::metadata(td.path()).unwrap().permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(td.path(), perms).unwrap();

        let result = write_atomic(&p, b"new", &WriteOptions::document());
        // Restore writability so tempdir deletion works.
        let mut perms = std::fs::metadata(td.path()).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(td.path(), perms).unwrap();

        result.unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn state_mode_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("secret.bin");
        write_atomic(&p, b"x", &WriteOptions::state()).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn preserve_mode_keeps_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("script.sh");
        std::fs::write(&p, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        write_atomic(&p, b"#!/bin/sh\necho hi\n", &WriteOptions::document()).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "executable bit lost, got {mode:o}");
    }

    /// No `.hjkl-tmp` entry may survive a call, successful or not.
    fn temp_leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("hjkl-tmp"))
            .collect()
    }

    #[test]
    fn copy_atomic_copies_contents_and_leaves_no_temp() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src.bin");
        let dst = td.path().join("dst.bin");
        std::fs::write(&src, b"payload").unwrap();
        copy_atomic(&src, &dst, &WriteOptions::default()).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"payload");
        assert!(temp_leftovers(td.path()).is_empty());
    }

    #[test]
    fn copy_atomic_replaces_existing_destination() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src.bin");
        let dst = td.path().join("dst.bin");
        std::fs::write(&dst, b"stale-and-longer").unwrap();
        std::fs::write(&src, b"fresh").unwrap();
        copy_atomic(&src, &dst, &WriteOptions::default()).unwrap();
        // Not a truncating overwrite: the whole file is the new contents.
        assert_eq!(std::fs::read(&dst).unwrap(), b"fresh");
    }

    #[cfg(unix)]
    #[test]
    fn copy_atomic_takes_source_mode_like_fs_copy() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("tool.so");
        let dst = td.path().join("installed.so");
        std::fs::write(&src, b"\0elf").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();
        copy_atomic(&src, &dst, &WriteOptions::default()).unwrap();
        let mode = std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "source mode not carried over, got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn copy_atomic_explicit_mode_beats_source_mode() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src.bin");
        let dst = td.path().join("dst.bin");
        std::fs::write(&src, b"secret").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o644)).unwrap();
        copy_atomic(&src, &dst, &WriteOptions::state()).unwrap();
        let mode = std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "explicit mode lost, got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn copy_atomic_preserve_mode_keeps_destination_mode() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src.sh");
        let dst = td.path().join("dst.sh");
        std::fs::write(&src, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&dst, b"old\n").unwrap();
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).unwrap();
        let opts = WriteOptions {
            preserve_mode: true,
            ..WriteOptions::default()
        };
        copy_atomic(&src, &dst, &opts).unwrap();
        let mode = std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "destination mode not preserved, got {mode:o}");
    }

    #[test]
    fn copy_atomic_missing_source_errors_without_temp_or_damage() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("does-not-exist");
        let dst = td.path().join("dst.bin");
        std::fs::write(&dst, b"original").unwrap();
        assert!(copy_atomic(&src, &dst, &WriteOptions::default()).is_err());
        assert_eq!(std::fs::read(&dst).unwrap(), b"original");
        assert!(
            temp_leftovers(td.path()).is_empty(),
            "temp left after failed copy"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_atomic_creates_link_and_leaves_no_temp() {
        let td = tempfile::tempdir().unwrap();
        let target = td.path().join("real-bin");
        std::fs::write(&target, b"#!/bin/sh\n").unwrap();
        let link = td.path().join("tool");
        symlink_atomic(&link, &target).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
        assert!(temp_leftovers(td.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_atomic_replaces_existing_symlink() {
        let td = tempfile::tempdir().unwrap();
        let old = td.path().join("old-bin");
        let new = td.path().join("new-bin");
        std::fs::write(&old, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        let link = td.path().join("tool");
        symlink_atomic(&link, &old).unwrap();
        symlink_atomic(&link, &new).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), new);
        assert!(temp_leftovers(td.path()).is_empty());
    }

    /// The staging name is appended to the whole file name, so a neighbour that
    /// happens to be named like a temp of a *different* link is untouched — and
    /// `foo.sh` / `foo.py` never share a staging path.
    #[cfg(unix)]
    #[test]
    fn symlink_atomic_staging_names_do_not_collide() {
        let td = tempfile::tempdir().unwrap();
        let target = td.path().join("bin");
        std::fs::write(&target, b"x").unwrap();
        let sh = td.path().join("foo.sh");
        let py = td.path().join("foo.py");
        symlink_atomic(&sh, &target).unwrap();
        symlink_atomic(&py, &target).unwrap();
        assert_eq!(std::fs::read_link(&sh).unwrap(), target);
        assert_eq!(std::fs::read_link(&py).unwrap(), target);
    }

    /// A rename that cannot succeed (destination is a non-empty directory) must
    /// still clean the staging link up.
    #[cfg(unix)]
    #[test]
    fn symlink_atomic_failed_rename_leaves_no_temp() {
        let td = tempfile::tempdir().unwrap();
        let target = td.path().join("real-bin");
        std::fs::write(&target, b"x").unwrap();
        let link = td.path().join("occupied");
        std::fs::create_dir(&link).unwrap();
        std::fs::write(link.join("child"), b"x").unwrap();
        assert!(symlink_atomic(&link, &target).is_err());
        assert!(link.is_dir(), "destination directory must survive");
        assert!(
            temp_leftovers(td.path()).is_empty(),
            "temp left after failed rename"
        );
    }

    #[test]
    fn probe_allows_missing_file() {
        let td = tempfile::tempdir().unwrap();
        probe_writable_nofollow(&td.path().join("does-not-exist")).unwrap();
    }

    #[test]
    fn probe_allows_writable_file() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("f.txt");
        std::fs::write(&p, b"x").unwrap();
        probe_writable_nofollow(&p).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn probe_rejects_symlinked_target() {
        let td = tempfile::tempdir().unwrap();
        let real = td.path().join("real.txt");
        std::fs::write(&real, b"x").unwrap();
        let link = td.path().join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // O_NOFOLLOW: opening the link itself must fail rather than write
        // through to `real`. This is the M4 security property.
        assert!(probe_writable_nofollow(&link).is_err());
    }
}
