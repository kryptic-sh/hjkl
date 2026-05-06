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

    #[test]
    fn roundtrip_simple_path() {
        let p = Path::new("/home/user/foo.rs");
        let url = from_path(p).unwrap();
        assert_eq!(url.scheme(), "file");
        let back = to_path(&url).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn roundtrip_path_with_spaces() {
        let p = Path::new("/home/user/my project/foo bar.rs");
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
        let p = Path::new("/a/b/c/d/e.toml");
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
