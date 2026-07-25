//! Owner-only file opens.
//!
//! [`crate::atomic`] owns "replace this file atomically, owner-only". It does not
//! cover the other shape: a caller that holds a long-lived handle and *appends* to
//! it — a log, an event transcript, an incrementally written index. Rewriting the
//! whole file per event is the wrong complexity and loses the append-only
//! property, so those callers need the [`OpenOptions`] rather than the write.
//!
//! Before this module the `#[cfg(unix)] opts.mode(0o600)` dance existed three
//! times inside the crate (temp file creation, the write probe, the lock file) and
//! nowhere a caller could reach it. That is exactly the per-site re-derivation
//! this crate exists to remove: the permission policy is one decision, and the
//! platform caveat below is one sentence that should be read once, not
//! rediscovered at each call site.
//!
//! # Platforms
//!
//! On Unix the guarantee is explicit: the file is created `0600`, so no other
//! local user can read it even though the directory may be traversable.
//!
//! On Windows there is **no explicit ACL**. Nothing here sets one, and callers
//! should not assume the file object itself is protected. The guarantee comes
//! instead from the containing directory: hjkl's state lives under a per-user
//! XDG root created by [`crate::dirs::ensure_private_dir`], and a file created
//! there inherits that directory's ACL. This is the same reasoning
//! `ensure_private_dir` documents — it is stated here so an append-mode caller
//! that reaches for these options learns *where* its confidentiality actually
//! comes from, rather than assuming the mode call did it.

use std::fs::OpenOptions;

/// The one mode hjkl gives its own state: readable and writable by the owner
/// alone.
///
/// `0600` because swap bodies, undo history and any event log describe what the
/// user was editing — credentials and private keys included. Mirrors
/// [`crate::atomic::WriteOptions::state`], which applies the same bits to the
/// atomic-write path.
pub(crate) const OWNER_ONLY_MODE: u32 = 0o600;

/// [`OpenOptions`] carrying `mode` when one is asked for — the single place the
/// unix-only `mode` call is cfg-gated.
///
/// `None` leaves creation to the process umask. Nothing else is set: the caller
/// supplies its own read/write/create/append semantics, which is the whole reason
/// this hands back options instead of a `File`.
///
/// The mode is only consulted by the OS when the open actually creates the file
/// (`O_CREAT`); setting it on an open that cannot create is inert, not a change of
/// behaviour.
pub(crate) fn options_with_mode(mode: Option<u32>) -> OpenOptions {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut opts = OpenOptions::new();
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(mode);
    }
    // No mode concept off Unix; see the module docs for where the guarantee
    // comes from there instead.
    #[cfg(not(unix))]
    let _ = mode;
    opts
}

/// Add `O_NOFOLLOW` on unix — the single place that flag is cfg-gated.
///
/// A symlinked final component then fails the open instead of being followed, so
/// the open *is* the symlink check: there is no window between "checked" and
/// "opened" for the target to be swapped for a link.
#[cfg_attr(not(unix), allow(unused_mut))]
fn with_no_follow(mut opts: OpenOptions) -> OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    opts
}

/// [`OpenOptions`] that create a file only its owner can read.
///
/// No access mode, no `create`, no `append`: the caller adds its own, because the
/// point is to hand the *permission* decision over without also dictating the
/// open semantics. An append-mode caller writes
///
/// ```no_run
/// use hjkl_fs::owner_only_options;
/// use std::io::Write;
///
/// let mut log = owner_only_options()
///     .append(true)
///     .create(true)
///     .open("/tmp/events.log")?;
/// writeln!(log, "started")?;
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// rather than re-deriving `#[cfg(unix)] opts.mode(0o600)` — and, more
/// importantly, rather than silently skipping it.
///
/// The mode applies at creation only; an existing file keeps whatever
/// permissions it already has. Use [`crate::atomic::write_atomic`] with
/// [`crate::atomic::WriteOptions::state`] when the file is being replaced
/// wholesale and its mode must be forced.
///
/// See the [module docs](self#platforms) for the Windows position: no explicit
/// ACL is set, and confidentiality comes from the containing per-user directory.
pub fn owner_only_options() -> OpenOptions {
    options_with_mode(Some(OWNER_ONLY_MODE))
}

/// [`owner_only_options`] plus `O_NOFOLLOW` on unix.
///
/// A symlinked final component fails the open rather than being followed, so the
/// open itself is the symlink check — no TOCTOU window between resolving the path
/// and opening it. Use this whenever the path came from outside the process (a
/// user-supplied log destination, an RPC-named file) and following a link to an
/// unintended target would be a security problem rather than a convenience.
///
/// `O_NOFOLLOW` applies to the **final** component only; symlinked directories
/// along the way are still traversed. Confine the path first with
/// [`crate::resolve_under`] if the intermediate components are also untrusted.
///
/// Windows has no `O_NOFOLLOW` equivalent here and the returned options are those
/// of [`owner_only_options`]; a caller that depends on the symlink guard for
/// safety must not assume it holds there.
pub fn owner_only_options_no_follow() -> OpenOptions {
    with_no_follow(owner_only_options())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn owner_only_creates_file_with_0600() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("log");
        let _f = owner_only_options()
            .append(true)
            .create(true)
            .open(&p)
            .unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got {mode:o}");
    }

    /// The options carry no access mode of their own — the caller's semantics
    /// decide, which is the contract the append example relies on.
    #[test]
    fn owner_only_options_add_no_access_mode() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("nope");
        // No read/write/append requested and the file does not exist: the open
        // must fail rather than quietly create or truncate anything.
        assert!(owner_only_options().open(&p).is_err());
        assert!(!p.exists(), "options created a file on their own");
    }

    /// Appending must not truncate: the whole reason a streaming caller needs
    /// options instead of an atomic whole-file write.
    #[test]
    fn owner_only_append_preserves_existing_contents() {
        use std::io::Write;
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("events.log");
        std::fs::write(&p, b"first\n").unwrap();
        let mut f = owner_only_options()
            .append(true)
            .create(true)
            .open(&p)
            .unwrap();
        f.write_all(b"second\n").unwrap();
        drop(f);
        assert_eq!(std::fs::read(&p).unwrap(), b"first\nsecond\n");
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_options_refuse_a_symlinked_target() {
        let td = tempfile::tempdir().unwrap();
        let real = td.path().join("real.log");
        std::fs::write(&real, b"x").unwrap();
        let link = td.path().join("link.log");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // The link resolves to a writable file, so only O_NOFOLLOW can make this
        // fail — and it must, or an attacker-planted link redirects the stream.
        assert!(
            owner_only_options_no_follow()
                .append(true)
                .create(true)
                .open(&link)
                .is_err()
        );
        assert_eq!(
            std::fs::read(&real).unwrap(),
            b"x",
            "wrote through the link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_options_still_open_a_regular_file() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("plain.log");
        owner_only_options_no_follow()
            .append(true)
            .create(true)
            .open(&p)
            .unwrap();
        assert!(p.is_file());
    }

    #[test]
    fn options_with_mode_none_still_creates() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("umask.bin");
        options_with_mode(None)
            .write(true)
            .create(true)
            .open(&p)
            .unwrap();
        assert!(p.is_file());
    }
}
