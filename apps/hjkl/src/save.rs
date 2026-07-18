use std::path::PathBuf;

/// Save `body` (plus an optional trailing newline) to `path` atomically and
/// durably: write a temp file in the target's own directory, fsync it, then
/// `rename` it over the target (atomic on the same filesystem). A crash or
/// ENOSPC mid-write can no longer leave the target truncated the way the old
/// in-place `File::create` (truncate-then-write) could.
///
/// Behavior notes:
/// - Symlinks are preserved: the target is canonicalized first, so the temp
///   file is written next to — and renamed onto — the REAL file; the symlink
///   itself keeps pointing where it did.
/// - The existing file's permission mode is copied onto the new file.
/// - Hardlinks ARE broken: `rename` replaces the inode, so other links keep
///   the old content. Accepted tradeoff (same as vim's default
///   `backupcopy=auto` rename strategy).
/// - Where temp+rename can't work (unwritable parent dir, cross-device
///   rename, exotic filesystems) this falls back to the previous in-place
///   write so saving never regresses.
pub(crate) fn save_file_durable(
    path: &std::path::Path,
    body: &[u8],
    trailing_nl: bool,
) -> std::io::Result<()> {
    use std::io::Write;

    fn write_body(f: &mut std::fs::File, body: &[u8], trailing_nl: bool) -> std::io::Result<()> {
        // Two pieces so the trailing newline doesn't force a full-buffer
        // clone just to append a byte.
        f.write_all(body)?;
        if trailing_nl {
            f.write_all(b"\n")?;
        }
        Ok(())
    }

    // In a confined RPC filesystem policy, refuse writes that escape the
    // working directory. Check the caller-supplied `path` *before* canonicalize
    // (which would resolve `..` away). No-op when the policy is off (TUI).
    hjkl_engine::policy::check_fs_path(path)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e))?;

    // Resolve symlinks so we replace the real file, not the link itself.
    // `canonicalize` fails when the file doesn't exist yet (new file) —
    // write at the given path in that case.
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    // `:w` on a file we lack write permission for must still fail, exactly
    // like the old `File::create` did — the rename trick would otherwise
    // bypass the file's own permission bits. Probe with a single non-truncating
    // write-open (contents and mtime untouched). A single open (rather than a
    // separate `exists()` check followed by an open) closes the TOCTOU gap where
    // the target could be swapped between the two syscalls. `NotFound` means a
    // brand-new file, which is allowed.
    match std::fs::OpenOptions::new().write(true).open(&target) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    // Track whether the temp file was opened so we can gate the fallback:
    // once the temp exists a write/sync error must propagate, never fall
    // back to in-place truncation (which would compound the I/O error with
    // data loss).  Only pre-write failures (unwritable parent, EXDEV) may
    // fall through to the non-atomic path.
    let mut temp_opened = false;
    let atomic = (|| -> std::io::Result<()> {
        let parent = match target.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let file_name = target
            .file_name()
            .ok_or_else(|| std::io::Error::other("target has no file name"))?;
        let tmp_path = parent.join(format!(
            ".{}.hjkl-tmp.{}",
            file_name.to_string_lossy(),
            std::process::id()
        ));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true) // never clobber an existing file
            .open(&tmp_path)?;
        temp_opened = true;

        let res = (|| -> std::io::Result<()> {
            // Preserve the existing file's permission mode.
            if let Ok(meta) = std::fs::metadata(&target) {
                f.set_permissions(meta.permissions())?;
            }
            write_body(&mut f, body, trailing_nl)?;
            f.sync_all()?; // durable BEFORE the rename makes it visible
            std::fs::rename(&tmp_path, &target)?;
            // fsync the parent directory so the rename itself is durable.
            if let Some(parent) = target.parent()
                && let Ok(pdir) = std::fs::File::open(parent)
            {
                let _ = pdir.sync_all();
            }
            Ok(())
        })();
        if res.is_err() {
            let _ = std::fs::remove_file(&tmp_path);
        }
        res
    })();

    match atomic {
        Ok(()) => Ok(()),
        Err(e) if !temp_opened || e.kind() == std::io::ErrorKind::CrossesDevices => {
            // Fallback: the previous in-place truncate-and-write. Non-atomic,
            // but works where temp+rename can't (e.g. cross-device rename, or
            // unwritable parent dir that prevented temp-file creation).
            // Only entered when the temp file was *never* opened; a write or
            // sync failure after creation would propagate above.
            let mut f = std::fs::File::create(path)?;
            write_body(&mut f, body, trailing_nl)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod save_file_durable_tests {
    use super::save_file_durable;

    /// Overwriting an existing file replaces its content and leaves no
    /// stray temp file behind.
    #[test]
    fn overwrites_existing_file_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "old contents\n").unwrap();

        save_file_durable(&p, b"new contents", true).unwrap();

        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new contents\n");
        // No leftover `.a.txt.hjkl-tmp.*` files.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("hjkl-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file leaked: {leftovers:?}");
    }

    /// Creating a brand-new file (no existing target) works.
    #[test]
    fn creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("fresh.txt");
        save_file_durable(&p, b"hello", true).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello\n");
    }

    /// Saving through a symlink must update the file the link points at and
    /// leave the symlink itself intact (NOT replace it with a regular file).
    #[cfg(unix)]
    #[test]
    fn preserves_symlink_and_updates_target() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&real, "old\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        save_file_durable(&link, b"new", true).unwrap();

        // The link is still a symlink pointing at the same place…
        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink(), "symlink was replaced");
        assert_eq!(std::fs::read_link(&link).unwrap(), real);
        // …and the real file got the new content.
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new\n");
    }

    /// The existing file's permission mode survives the rename replace.
    #[cfg(unix)]
    #[test]
    fn preserves_permission_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("script.sh");
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();

        save_file_durable(&p, b"#!/bin/sh\necho hi", true).unwrap();

        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "permission mode not preserved");
    }

    /// A read-only target must still make the save fail (old `File::create`
    /// semantics) rather than being silently replaced via rename.
    #[cfg(unix)]
    #[test]
    fn readonly_target_still_errors() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ro.txt");
        std::fs::write(&p, "locked\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o444)).unwrap();

        assert!(save_file_durable(&p, b"nope", true).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "locked\n");
    }
}
