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

/// Convert a `file://` URL to a path, but only if the resolved path is inside
/// `root`. Returns `None` when `u` is not a file URL, conversion fails, or the
/// path escapes `root`. Defense-in-depth against a server returning URIs
/// outside the workspace.
pub fn to_path_within(u: &url::Url, root: &Path) -> Option<PathBuf> {
    let p = to_path(u)?;
    // Normalize away `.`/`..` lexically (no filesystem access) before the prefix check.
    let normalized = normalize_lexical(&p);
    let root_norm = normalize_lexical(root);
    if normalized.starts_with(&root_norm) {
        Some(p)
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
