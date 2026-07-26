/// Save `body` (plus an optional trailing newline) to `path` atomically and
/// durably: write a temp file in the target's own directory, fsync it, then
/// `rename` it over the target (atomic on the same filesystem). A crash or
/// ENOSPC mid-write can no longer leave the target truncated the way the old
/// in-place `File::create` (truncate-then-write) could.
///
/// The mechanism itself lives in [`hjkl_fs`] — the single seam for hjkl's disk
/// I/O — so `:w`, swap, undo and config all share one temp→fsync→rename
/// implementation. What stays here is the policy this call site owns: the RPC
/// filesystem guard, symlink resolution, and the write-permission probe.
///
/// Behavior notes:
/// - Symlinks are preserved: the target is canonicalized first, so the temp
///   file is written next to — and renamed onto — the REAL file; the symlink
///   itself keeps pointing where it did.
/// - The existing file's permission mode is copied onto the new file
///   ([`hjkl_fs::WriteOptions::document`]'s `preserve_mode`).
/// - Hardlinks ARE broken: `rename` replaces the inode, so other links keep
///   the old content. Accepted tradeoff (same as vim's default
///   `backupcopy=auto` rename strategy).
/// - Where temp+rename can't work (unwritable parent dir, cross-device
///   rename, exotic filesystems) `document()` falls back to a non-atomic
///   in-place write so saving never regresses. That fallback uses
///   `File::create` (O_TRUNC), so a mid-write failure **can** leave the
///   target truncated — only the atomic path guarantees no data loss.
pub fn save_file_durable(
    path: &std::path::Path,
    body: &[u8],
    trailing_nl: bool,
    cwd: &std::path::Path,
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
    // When the FS policy is active, use `resolve_under` which also rejects
    // symlink escapes that a purely lexical check would miss.
    // `canonicalize` fails when the file doesn't exist yet (new file) —
    // write at the given path in that case.
    let target = if hjkl_engine::policy::fs_restricted() {
        match hjkl_fs::resolve_under(cwd, path) {
            Ok(resolved) => resolved,
            Err(e) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("{path}: {e}", path = path.display()),
                ));
            }
        }
    } else {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    };

    // `:w` on a file we lack write permission for must still fail, exactly
    // like the old `File::create` did — the rename trick would otherwise
    // bypass the file's own permission bits. `probe_writable_nofollow` is a
    // single non-truncating write-open (contents and mtime untouched). A single
    // open (rather than a separate `exists()` check followed by an open) closes
    // the TOCTOU gap where the target could be swapped between the two
    // syscalls. `O_NOFOLLOW` (Unix) hardens this further: if the target was
    // swapped with a symlink after canonicalize resolved it, the open fails
    // rather than following the link to an unintended file (security finding
    // M4). `NotFound` is allowed through by the probe — that's a brand-new file.
    hjkl_fs::probe_writable_nofollow(&target)?;

    // temp → fsync → rename → fsync-parent, with mode preservation and the
    // pre-write-only non-atomic fallback, all expressed by `document()`.
    // `write_atomic_with` streams the body so the trailing newline never forces
    // a full-buffer clone; the closure is `Fn` and may run twice (temp-name
    // collision, fallback), so it must stay a pure function of `body`.
    hjkl_fs::write_atomic_with(&target, &hjkl_fs::WriteOptions::document(), |f| {
        write_body(f, body, trailing_nl)
    })
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

        save_file_durable(&p, b"new contents", true, &std::env::current_dir().unwrap()).unwrap();

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
        save_file_durable(&p, b"hello", true, &std::env::current_dir().unwrap()).unwrap();
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

        save_file_durable(&link, b"new", true, &std::env::current_dir().unwrap()).unwrap();

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

        save_file_durable(
            &p,
            b"#!/bin/sh\necho hi",
            true,
            &std::env::current_dir().unwrap(),
        )
        .unwrap();

        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "permission mode not preserved");
    }

    /// The `O_NOFOLLOW` write probe (security finding M4) must still be in the
    /// path after the move onto the `hjkl-fs` seam.
    ///
    /// A dangling symlink is the observable version of the race the flag
    /// defends against: `canonicalize` can't resolve it, so the probe runs on
    /// the link itself. With `O_NOFOLLOW` the open fails (`ELOOP`) and the save
    /// errors; a plain `open` would instead report `NotFound`, be treated as a
    /// brand-new file, and write *through* the link to whatever it names.
    #[cfg(unix)]
    #[test]
    fn dangling_symlink_target_is_refused_by_nofollow_probe() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&victim, &link).unwrap();

        assert!(
            save_file_durable(&link, b"pwned", true, &std::env::current_dir().unwrap()).is_err(),
            "write followed a symlink — O_NOFOLLOW probe lost"
        );
        assert!(!victim.exists(), "wrote through the symlink to its target");
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

        assert!(save_file_durable(&p, b"nope", true, &std::env::current_dir().unwrap()).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "locked\n");
    }
}
