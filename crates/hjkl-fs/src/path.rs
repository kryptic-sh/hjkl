//! Path normalization and confinement.
//!
//! [`crate::dirs`] answers "where does this go". This module answers the other
//! half of deciding whether a write is allowed: **"is this path inside the
//! boundary I was given"**.
//!
//! # The trap
//!
//! A confinement check is easy to write and easy to write wrongly. The specific
//! failure is a `starts_with` test against a path that still contains unresolved
//! `..`:
//!
//! ```text
//! root/nonexistent/../../etc/passwd    // starts_with(root) == true, escapes root
//! ```
//!
//! The prefix test passes on the spelling while the path resolves somewhere else
//! entirely. [`std::fs::canonicalize`] fixes that — and then fails outright when
//! the path does not exist yet, which is precisely the case for a file about to
//! be created. So neither tool alone is enough, and the composition of the two
//! ("canonicalize the nearest existing ancestor, resolve the remainder lexically
//! against it") is fiddly enough to be worth exactly one implementation.
//!
//! # Mechanism, not policy
//!
//! Which roots are allowed, and what to do about a violation, belong to the
//! consumer — `hjkl_engine::policy::check_fs_path` keeps its own layer on top of
//! this and is expected to. What lives here is the mechanism: normalization that
//! cannot be fooled, and one [`resolve_under`] that composes with it.

use std::io;
use std::path::{Component, Path, PathBuf};

/// Canonicalize as far as the filesystem allows, then finish the job lexically.
///
/// [`std::fs::canonicalize`] resolves symlinks and `..` correctly but requires
/// every component to exist, so it cannot answer anything about a file that is
/// about to be created. This canonicalizes the **nearest existing ancestor** and
/// re-joins the non-existent remainder, resolving `..` and `.` in that remainder
/// lexically against the canonical prefix.
///
/// The returned path **never contains an unprocessed [`Component::ParentDir`]**.
/// That is the property callers depend on: a `starts_with` check against the
/// result cannot be defeated by a `..` that had not been resolved yet. `..` at
/// the filesystem root is dropped, matching POSIX (`/..` is `/`).
///
/// Relative input is made absolute against the current directory first, so the
/// result is always absolute and two spellings of one file compare equal.
///
/// This is infallible by design: an unreadable or missing ancestor is not an
/// error to report, it just means less of the path could be resolved by the
/// filesystem and more of it lexically. Callers that need a decision want
/// [`resolve_under`].
///
/// # Symlinks
///
/// Symlinks in the existing prefix **are** resolved, by `canonicalize` — that is
/// the point, and it is what makes the result trustworthy for a confinement
/// check.
///
/// A symlink inside the non-existent remainder cannot be resolved, because it
/// does not exist yet. Two consequences for callers:
///
/// - The result describes where the path points *now*. A component created as a
///   symlink afterwards can redirect the real open — this is normalization, not
///   a lock, and it does not close a TOCTOU window on its own. Open the file with
///   [`crate::owner_only_options_no_follow`], or hold
///   [`crate::with_lock_exclusive`], when that window matters.
/// - Only the *final* component is guarded by `O_NOFOLLOW`, so a caller that
///   creates several missing directories along the remainder should re-check
///   after creating them if the path came from outside the process.
pub fn canonicalize_nearest(path: &Path) -> PathBuf {
    // Absolute first, so a relative path is anchored before anything is
    // resolved. `absolute` is lexical and (on unix) leaves `..` alone, which is
    // what we want — resolving `..` before the prefix is canonicalized is the
    // very mistake this function exists to prevent.
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let comps: Vec<Component<'_>> = abs.components().collect();

    // Longest existing ancestor, walking back from the whole path. `canonicalize`
    // is the existence test *and* the resolver: a prefix containing an
    // unresolvable `..` (because what precedes it does not exist) simply fails
    // here and the search continues upward.
    let mut split = comps.len();
    let mut resolved = loop {
        if split == 0 {
            // Nothing at all resolved — not even the root. Fall back to the
            // lexical prefix so the remainder still gets processed rather than
            // being returned with its `..` intact.
            break comps
                .iter()
                .take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
                .collect::<PathBuf>();
        }
        let candidate: PathBuf = comps[..split].iter().collect();
        match std::fs::canonicalize(&candidate) {
            Ok(c) => break c,
            Err(_) => split -= 1,
        }
    };

    // Everything past the existing prefix is resolved lexically. Safe precisely
    // because it does not exist: there is no symlink there to be fooled by.
    for comp in &comps[split..] {
        match comp {
            Component::CurDir => {}
            // `pop` at the root is a no-op, so `/..` stays `/` as POSIX has it.
            Component::ParentDir => {
                resolved.pop();
            }
            // A `Normal` component spelled `.` or `..` is still a traversal.
            // Windows verbatim (`\\?\`) paths are handed to the OS literally, so
            // Rust does not classify their dot segments as `CurDir`/`ParentDir` —
            // taking them at face value here would let exactly the traversal this
            // function exists to stop survive into the result. Nothing is lost by
            // resolving them: no real file can be named `.` or `..`.
            Component::Normal(name) if name.as_encoded_bytes() == b"." => {}
            Component::Normal(name) if name.as_encoded_bytes() == b".." => {
                resolved.pop();
            }
            Component::Normal(name) => resolved.push(name),
            // An absolute component can only appear at index 0, which is always
            // inside the canonicalized prefix; re-rooting here would mean the
            // prefix search failed on the root itself, and pushing it back is the
            // faithful thing to do.
            Component::RootDir | Component::Prefix(_) => resolved.push(comp.as_os_str()),
        }
    }
    resolved
}

/// Resolve `path` against `root` and confirm it stays inside `root`.
///
/// Returns the resolved absolute path, or [`io::ErrorKind::PermissionDenied`]
/// when it escapes. A relative `path` is joined onto `root`; an absolute one is
/// taken as-is and then checked, so passing an absolute path is not a way around
/// the boundary.
///
/// Both sides go through [`canonicalize_nearest`] before being compared, which is
/// what makes the check unfoolable: the classic
/// `root/nonexistent/../../etc/passwd` passes a naive `starts_with(root)` — the
/// spelling begins with `root` — yet resolves outside it. Here the `..` is
/// resolved *first*, so the comparison sees the real destination.
///
/// The comparison is component-wise ([`Path::starts_with`]), so a sibling
/// directory whose name merely begins with the root's (`/srv/data-backup`
/// against root `/srv/data`) is correctly rejected. `root` itself is accepted:
/// a boundary contains itself.
///
/// A path whose parent does not exist yet is accepted when it resolves inside
/// `root` — creating a file is the motivating case, and refusing it would make
/// the primitive useless for the write path.
///
/// # Errors
///
/// [`io::ErrorKind::PermissionDenied`] when the resolved path is outside `root`.
/// Existence is never required and is never an error; this answers "is it
/// inside", not "is it there".
pub fn resolve_under(root: &Path, path: &Path) -> io::Result<PathBuf> {
    let canonical_root = canonicalize_nearest(root);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        canonical_root.join(path)
    };
    let resolved = canonicalize_nearest(&joined);
    if resolved.starts_with(&canonical_root) {
        Ok(resolved)
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "path escapes its root: {} resolves to {}, outside {}",
                path.display(),
                resolved.display(),
                canonical_root.display()
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the whole module rests on: whatever comes back, no caller
    /// downstream has to worry about a `..` it still needs to resolve.
    fn assert_no_parent_components(p: &Path) {
        assert!(
            !p.components().any(|c| matches!(c, Component::ParentDir)),
            "unresolved ParentDir survived in {}",
            p.display()
        );
    }

    #[test]
    fn canonicalize_nearest_matches_canonicalize_when_everything_exists() {
        let td = tempfile::tempdir().unwrap();
        let f = td.path().join("here.txt");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(canonicalize_nearest(&f), std::fs::canonicalize(&f).unwrap());
    }

    #[test]
    fn canonicalize_nearest_resolves_a_path_that_does_not_exist_yet() {
        let td = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(td.path()).unwrap();
        // Neither the directories nor the file exist; canonicalize alone errors.
        let target = root.join("new").join("deep").join("file.txt");
        assert!(std::fs::canonicalize(&target).is_err());
        assert_eq!(canonicalize_nearest(&target), target);
    }

    #[test]
    fn canonicalize_nearest_leaves_no_parent_components() {
        let td = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(td.path()).unwrap();
        for spelling in [
            root.join("a").join("..").join("b.txt"),
            root.join("nonexistent").join("..").join("..").join("etc"),
            root.join(".").join("x").join("..").join("..").join(".."),
            PathBuf::from("relative").join("..").join("thing"),
        ] {
            let got = canonicalize_nearest(&spelling);
            assert_no_parent_components(&got);
            assert!(got.is_absolute(), "not absolute: {}", got.display());
        }
    }

    #[test]
    fn canonicalize_nearest_resolves_dots_lexically_in_the_missing_tail() {
        let td = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(td.path()).unwrap();
        let spelling = root.join("gone").join("..").join("kept.txt");
        assert_eq!(canonicalize_nearest(&spelling), root.join("kept.txt"));
    }

    #[test]
    fn canonicalize_nearest_stops_climbing_at_the_root() {
        // More `..` than there are components: POSIX says `/..` is `/`, and the
        // result must still be a usable absolute path rather than empty.
        let got = canonicalize_nearest(Path::new("/../../../.."));
        assert_no_parent_components(&got);
        assert!(got.is_absolute());
    }

    #[cfg(unix)]
    #[test]
    fn canonicalize_nearest_resolves_symlinks_in_the_existing_prefix() {
        let td = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(td.path()).unwrap();
        let real = root.join("real-dir");
        std::fs::create_dir(&real).unwrap();
        let link = root.join("link-dir");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // The symlink exists, so `canonicalize` handles it and the not-yet-created
        // leaf is joined onto the *resolved* directory.
        assert_eq!(
            canonicalize_nearest(&link.join("new.txt")),
            real.join("new.txt")
        );
    }

    #[test]
    fn resolve_under_accepts_a_plain_path_inside_root() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        let got = resolve_under(root, Path::new("notes.txt")).unwrap();
        assert_eq!(got, canonicalize_nearest(root).join("notes.txt"));
    }

    #[test]
    fn resolve_under_accepts_parent_dots_that_stay_inside() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        std::fs::create_dir(root.join("sub")).unwrap();
        let got = resolve_under(root, Path::new("sub/../notes.txt")).unwrap();
        assert_eq!(got, canonicalize_nearest(root).join("notes.txt"));
        assert_no_parent_components(&got);
    }

    #[test]
    fn resolve_under_accepts_a_path_whose_parent_does_not_exist_yet() {
        // Creating a file is the motivating case; refusing a missing parent would
        // make the primitive useless on the write path.
        let td = tempfile::tempdir().unwrap();
        let got = resolve_under(td.path(), Path::new("new/deep/file.txt")).unwrap();
        assert_eq!(
            got,
            canonicalize_nearest(td.path())
                .join("new")
                .join("deep")
                .join("file.txt")
        );
    }

    #[test]
    fn resolve_under_accepts_the_root_itself() {
        let td = tempfile::tempdir().unwrap();
        let got = resolve_under(td.path(), Path::new(".")).unwrap();
        assert_eq!(got, canonicalize_nearest(td.path()));
        let got_abs = resolve_under(td.path(), td.path()).unwrap();
        assert_eq!(got_abs, canonicalize_nearest(td.path()));
    }

    #[test]
    fn resolve_under_rejects_an_absolute_path_outside_root() {
        let td = tempfile::tempdir().unwrap();
        let err = resolve_under(td.path(), Path::new("/etc/passwd")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    /// THE case this primitive exists for (issue #317).
    ///
    /// `root/nonexistent/../../etc/passwd` passes a naive `starts_with(root)` —
    /// the spelling literally begins with the root — while resolving outside it.
    /// The test asserts both halves: that the naive check is fooled, so the
    /// scenario stays real, and that `resolve_under` is not.
    #[test]
    fn resolve_under_rejects_dot_dot_escape_that_fools_starts_with() {
        let td = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(td.path()).unwrap();
        // Built one component at a time rather than from
        // `"nonexistent/../../etc/passwd"`, so the test means the same thing on
        // every platform: on Windows `canonicalize` returns a verbatim (`\\?\`)
        // path, inside which `/` is an ordinary character rather than a
        // separator, and the string spelling would collapse into a single
        // component that tests nothing.
        let attack = root
            .join("nonexistent")
            .join("..")
            .join("..")
            .join("etc")
            .join("passwd");

        // The trap, demonstrated: unresolved `..` and the prefix test still says
        // "inside".
        //
        // Unix-only, and deliberately so. This asserts the *premise* — that the
        // naive check really is fooled — so the scenario cannot rot into
        // something that passes vacuously. But being fooled is a fact about how
        // a platform parses paths, not about `resolve_under`: on Windows `root`
        // is a verbatim (`\\?\`) path, whose components compare differently, and
        // empirically the prefix test is *not* fooled by this spelling there.
        // That is a weaker attack on Windows, not a weaker defence, so gating
        // the premise is honest. Every assertion below — the rejection, and the
        // proof that it is a genuine escape — runs on every platform, and
        // `dot_segments_spelled_as_normal_components_are_still_resolved` covers
        // the verbatim shape directly.
        #[cfg(unix)]
        assert!(
            attack.starts_with(&root),
            "naive starts_with should be fooled here; test no longer covers the trap"
        );

        let err = resolve_under(&root, &attack).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

        // And it is a genuine escape, not just a rejected spelling: the resolved
        // destination really is above the root.
        let resolved = canonicalize_nearest(&attack);
        assert!(!resolved.starts_with(&root), "{}", resolved.display());
        assert_no_parent_components(&resolved);
    }

    /// Dot segments that arrive as `Normal` components must still be resolved.
    ///
    /// This is the Windows verbatim-path shape reproduced portably: a caller can
    /// hand us a component whose *text* is `..` without the parser having
    /// classified it as `ParentDir`. Taking that at face value would let the
    /// traversal survive into the result, so it is resolved either way — and no
    /// real file can be named `.` or `..`, so nothing legitimate is lost.
    #[test]
    fn dot_segments_spelled_as_normal_components_are_still_resolved() {
        let td = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(td.path()).unwrap();
        let escape = root
            .join("a")
            .join(std::ffi::OsStr::new(".."))
            .join(std::ffi::OsStr::new(".."))
            .join("outside");

        let resolved = canonicalize_nearest(&escape);
        assert_no_parent_components(&resolved);
        assert!(
            !resolved.starts_with(&root),
            "traversal survived: {}",
            resolved.display()
        );
        assert_eq!(
            resolve_under(&root, &escape).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    /// The relative spelling of the same attack, which is how it arrives over an
    /// RPC boundary.
    #[test]
    fn resolve_under_rejects_relative_dot_dot_escape() {
        let td = tempfile::tempdir().unwrap();
        let err = resolve_under(td.path(), Path::new("nonexistent/../../etc/passwd")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    /// Component-wise comparison: a sibling whose name merely starts with the
    /// root's must not be accepted.
    #[test]
    fn resolve_under_rejects_a_sibling_with_a_prefix_name() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().join("data");
        let sibling = td.path().join("data-backup");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&sibling).unwrap();
        let err = resolve_under(&root, &sibling.join("f.txt")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_under_rejects_a_symlink_pointing_out_of_root() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().join("root");
        let outside = td.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        // The symlink exists, so canonicalize resolves it and the escape shows up
        // even though the leaf below it does not exist yet.
        let err = resolve_under(&root, Path::new("escape/f.txt")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }
}
