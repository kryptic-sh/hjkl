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
}
