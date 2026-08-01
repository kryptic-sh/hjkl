//! Path ↔ `file://` URL helpers.

use std::path::{Path, PathBuf};

/// Returned when a path cannot be converted to a `file://` URL.
#[derive(Debug, Clone, Copy)]
pub struct NotAbsoluteError;

impl std::fmt::Display for NotAbsoluteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("path is not absolute or cannot be represented as a file:// URL")
    }
}

impl std::error::Error for NotAbsoluteError {}

/// Convert a filesystem path to a `file://` URL.
///
/// Returns `Err(NotAbsoluteError)` if `p` is not absolute or the URL conversion fails.
pub fn from_path(p: &Path) -> Result<url::Url, NotAbsoluteError> {
    url::Url::from_file_path(p).map_err(|_| NotAbsoluteError)
}

/// Convert a `file://` URL back to an absolute [`PathBuf`].
///
/// Returns `None` if `u` is not a `file://` URL or the conversion fails.
pub fn to_path(u: &url::Url) -> Option<PathBuf> {
    u.to_file_path().ok()
}

/// Convert a `file://` URL to a path, but only if the normalized path is inside
/// `root`. Returns `None` when `u` is not a file URL, conversion fails, or the
/// path escapes `root`. Defense-in-depth against a server returning URIs
/// outside the workspace.
///
/// The returned path is the *normalized* one — the same value the containment
/// check was made against, so a caller can never act on a spelling this
/// function did not approve. It is therefore not always byte-identical to
/// `to_path(u)`: `.` and `..` segments are folded out.
///
/// # This check is lexical and symlink-unaware
///
/// [`normalize_lexical`] folds `..` by popping the previous component, which is
/// not how the kernel resolves it when that component is a symlink:
/// `<root>/link/../secret` folds to `<root>/secret` and is accepted here, but
/// on disk it resolves through `link`'s *target's* parent and can land anywhere.
/// This function never touches the filesystem, so it cannot see that.
///
/// `hjkl_fs::resolve_under` (`crates/hjkl-fs/src/path.rs`) is the stronger
/// check: it canonicalizes both sides *before* comparing, so the `..` is
/// resolved by the OS and a symlink anywhere in the prefix is followed to its
/// real destination — the case this function misses. Prefer it wherever a
/// filesystem round-trip is acceptable; this one exists for the cheap,
/// no-syscall path.
///
/// # Nothing in this workspace calls it
///
/// Kept public because `hjkl-lsp` is published and may have external consumers.
/// The crate's own code and the app both use [`to_path`]; the containment
/// decision for workspace edits is made in the app, in
/// `apply_workspace_edit` (`apps/hjkl/src/app/lsp_glue.rs`), which deliberately
/// *warns* rather than refuses — out-of-root edits are applied to unsaved
/// buffers and the user is told before a `:w` persists them. So do not read the
/// existence of this function as evidence that containment is enforced on the
/// LSP edit path; it is not.
pub fn to_path_within(u: &url::Url, root: &Path) -> Option<PathBuf> {
    let p = to_path(u)?;
    // Normalize away `.`/`..` lexically (no filesystem access) before the
    // prefix check, and hand back the normalized path — validating one
    // spelling and returning another is how `..` slips past a caller.
    let normalized = normalize_lexical(&p);
    let root_norm = normalize_lexical(root);
    if normalized.starts_with(&root_norm) {
        Some(normalized)
    } else {
        None
    }
}

/// Lexically normalize a path: fold `..` by popping the previous component,
/// drop `.`, and keep `Normal`/`RootDir`/`Prefix` components as-is. No
/// filesystem access (symlinks are not resolved).
fn normalize_lexical(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            Component::Normal(_) | Component::RootDir | Component::Prefix(_) => {
                out.push(comp.as_os_str());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // `url::Url::from_file_path` requires platform-absolute paths: Unix-style
    // (`/foo`) on Unix, drive-rooted (`C:\foo`) on Windows. The roundtrip
    // semantics are the same on both; only the input path syntax differs.
    #[cfg(unix)]
    const SIMPLE: &str = "/home/user/foo.rs";
    #[cfg(windows)]
    const SIMPLE: &str = r"C:\Users\user\foo.rs";

    #[cfg(unix)]
    const WITH_SPACES: &str = "/home/user/my project/foo bar.rs";
    #[cfg(windows)]
    const WITH_SPACES: &str = r"C:\Users\user\my project\foo bar.rs";

    #[cfg(unix)]
    const NESTED: &str = "/a/b/c/d/e.toml";
    #[cfg(windows)]
    const NESTED: &str = r"C:\a\b\c\d\e.toml";

    #[test]
    fn roundtrip_simple_path() {
        let p = Path::new(SIMPLE);
        let url = from_path(p).unwrap();
        assert_eq!(url.scheme(), "file");
        let back = to_path(&url).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn roundtrip_path_with_spaces() {
        let p = Path::new(WITH_SPACES);
        let url = from_path(p).unwrap();
        assert!(
            url.as_str().contains("%20"),
            "spaces must be percent-encoded"
        );
        let back = to_path(&url).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn roundtrip_nested_path() {
        let p = Path::new(NESTED);
        let url = from_path(p).unwrap();
        let back = to_path(&url).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn relative_path_returns_err() {
        let p = Path::new("relative/path.rs");
        assert!(from_path(p).is_err());
    }

    #[test]
    fn non_file_url_returns_none() {
        let url = url::Url::parse("https://example.com/foo").unwrap();
        assert!(to_path(&url).is_none());
    }

    #[cfg(unix)]
    const ROOT: &str = "/tmp/workspace";
    #[cfg(windows)]
    const ROOT: &str = r"C:\tmp\workspace";

    #[cfg(unix)]
    const INSIDE: &str = "/tmp/workspace/src/main.rs";
    #[cfg(windows)]
    const INSIDE: &str = r"C:\tmp\workspace\src\main.rs";

    #[cfg(unix)]
    const SIBLING: &str = "/tmp/workspace-evil/src/main.rs";
    #[cfg(windows)]
    const SIBLING: &str = r"C:\tmp\workspace-evil\src\main.rs";

    #[test]
    fn to_path_within_accepts_path_inside_root() {
        let url = from_path(Path::new(INSIDE)).unwrap();
        let p = to_path_within(&url, Path::new(ROOT));
        assert_eq!(p, Some(PathBuf::from(INSIDE)));
    }

    #[test]
    fn to_path_within_rejects_parent_escape() {
        // `..` segments that resolve outside the root must be rejected.
        // (The URL parser may fold dot segments itself; either way the
        // resolved path lands outside `root` and must return None.)
        let url = url::Url::parse("file:///tmp/workspace/../secrets/key.pem").unwrap();
        assert!(to_path_within(&url, Path::new("/tmp/workspace")).is_none());
    }

    #[cfg(unix)]
    const DOTTED_INSIDE: &str = "/tmp/workspace/a/../b/c.rs";
    #[cfg(windows)]
    const DOTTED_INSIDE: &str = r"C:\tmp\workspace\a\..\b\c.rs";

    #[cfg(unix)]
    const DOTTED_FOLDED: &str = "/tmp/workspace/b/c.rs";
    #[cfg(windows)]
    const DOTTED_FOLDED: &str = r"C:\tmp\workspace\b\c.rs";

    #[test]
    fn to_path_within_returns_normalized_path() {
        // `url::Url::parse` runs the URL standard's path state machine and
        // folds dot segments itself, so a parsed URL never carries `..` this
        // far. `from_path` does NOT — it writes the components through
        // verbatim — so this is the construction that actually reaches the
        // function with `..` intact.
        let url = from_path(Path::new(DOTTED_INSIDE)).unwrap();
        assert!(
            url.as_str().contains("/../"),
            "from_path must preserve `..` for this test to exercise anything"
        );
        assert_eq!(to_path(&url).unwrap(), PathBuf::from(DOTTED_INSIDE));

        // The `..` stays inside the root, so the path is accepted — and what
        // comes back is the folded spelling that was checked, not the raw one.
        let p = to_path_within(&url, Path::new(ROOT));
        assert_eq!(p, Some(PathBuf::from(DOTTED_FOLDED)));
    }

    #[test]
    fn to_path_within_rejects_sibling_directory() {
        // A sibling whose name shares the root as a string prefix must not
        // pass the containment check (component-wise starts_with).
        let url = from_path(Path::new(SIBLING)).unwrap();
        assert!(to_path_within(&url, Path::new(ROOT)).is_none());
    }

    #[test]
    fn normalize_lexical_folds_dot_segments() {
        #[cfg(unix)]
        {
            assert_eq!(
                normalize_lexical(Path::new("/a/b/../c/./d")),
                PathBuf::from("/a/c/d")
            );
            assert_eq!(
                normalize_lexical(Path::new("/a/../../b")),
                PathBuf::from("/b")
            );
        }
        #[cfg(windows)]
        {
            assert_eq!(
                normalize_lexical(Path::new(r"C:\a\b\..\c\.\d")),
                PathBuf::from(r"C:\a\c\d")
            );
        }
    }
}
