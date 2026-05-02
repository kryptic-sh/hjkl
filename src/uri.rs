//! [`Uri`] — typed URI for clipboard uri-list operations.
//!
//! Handles percent-encoding/decoding and Windows UNC path mapping.

use std::path::{Path, PathBuf};

use crate::ClipboardError;

/// A URI entry in a `text/uri-list` clipboard payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Uri {
    /// A local file path. Must be absolute — relative paths are rejected with
    /// [`ClipboardError::InvalidUri`].
    File(PathBuf),
    /// Any non-file URI (e.g. `https://example.com`). Passed through verbatim.
    Other(String),
}

// ---------------------------------------------------------------------------
// Percent-encoding helpers (RFC 3986)
// ---------------------------------------------------------------------------
//
// Unreserved set kept unencoded: A-Za-z0-9 - . _ ~ / :
// (We keep `/` and `:` unencoded for readability in paths and URI schemes.)
// Everything else: percent-encode as %HH using uppercase hex.

/// Return true if the byte should be kept as-is in a URI.
#[inline]
fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/' | b':')
}

/// Percent-encode a byte slice. Unreserved bytes pass through; everything else
/// becomes `%HH` (uppercase hex).
fn percent_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for &b in input {
        if is_unreserved(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(
                char::from_digit((b >> 4) as u32, 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
            out.push(
                char::from_digit((b & 0xf) as u32, 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
        }
    }
    out
}

/// Decode a percent-encoded string into bytes.
///
/// Returns `ClipboardError::InvalidUri` if any `%XY` escape is malformed
/// (non-hex digits or truncated).
fn percent_decode(input: &str) -> Result<Vec<u8>, ClipboardError> {
    let bad = || ClipboardError::InvalidUri;
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(bad());
            }
            let hi = hex_val(bytes[i + 1]).ok_or_else(bad)?;
            let lo = hex_val(bytes[i + 2]).ok_or_else(bad)?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

/// Convert a hex digit byte to its numeric value. Returns `None` for non-hex.
#[inline]
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// file:// URI converters
// ---------------------------------------------------------------------------

/// Convert an absolute filesystem path to a `file://` URI string.
///
/// - Unix: `/foo/bar baz` → `file:///foo/bar%20baz`
/// - Windows drive: `C:\foo\bar` → `file:///C:/foo/bar`
/// - Windows UNC: `\\server\share\foo` → `file://server/share/foo`
///
/// Returns [`ClipboardError::InvalidUri`] if the path is relative.
pub(crate) fn path_to_file_uri(path: &Path) -> Result<String, ClipboardError> {
    let path_str = path.to_str().ok_or(ClipboardError::InvalidUri)?;

    if cfg!(windows) {
        // Detect UNC: starts with \\ or // (after str conversion on Windows
        // PathBuf always uses backslashes, but accept both here for robustness).
        if path_str.starts_with("\\\\") || path_str.starts_with("//") {
            // Strip the leading \\ or //, then normalise the rest to forward slashes.
            let without_prefix = path_str
                .strip_prefix("\\\\")
                .or_else(|| path_str.strip_prefix("//"))
                .unwrap_or(&path_str[2..]);
            let normalised = without_prefix.replace('\\', "/");
            // Percent-encode each segment but keep the separating '/' bare.
            let encoded = encode_path_segments(&normalised);
            return Ok(format!("file://{encoded}"));
        }

        // Detect drive letter: e.g. `C:\foo` or `C:/foo`.
        if path_str.len() >= 2 && path_str.as_bytes()[1] == b':' {
            let normalised = path_str.replace('\\', "/");
            let encoded = encode_path_segments(&normalised);
            return Ok(format!("file:///{encoded}"));
        }

        // Relative — reject.
        return Err(ClipboardError::InvalidUri);
    }

    // Unix: must start with '/'.
    if !path_str.starts_with('/') {
        return Err(ClipboardError::InvalidUri);
    }

    // Encode path bytes (the path may contain non-UTF-8; use OsStr bytes).
    // We already checked `to_str()` succeeds, so the bytes are valid UTF-8.
    let encoded = percent_encode(path_str.as_bytes());
    Ok(format!("file://{encoded}"))
}

/// Percent-encode each path segment individually, keeping `/` separators bare.
fn encode_path_segments(path: &str) -> String {
    // Split on `/`, encode each segment, then rejoin.
    path.split('/')
        .map(|seg| percent_encode(seg.as_bytes()))
        .collect::<Vec<_>>()
        .join("/")
}

/// Convert a `file://` URI back to a `PathBuf`.
///
/// On Unix: `file:///foo/bar` → `/foo/bar`.
/// On Windows: `file:///C:/foo` → `C:\foo`, `file://server/share/x` →
/// `\\server\share\x`.
///
/// Returns [`ClipboardError::InvalidUri`] if the URI is malformed.
pub(crate) fn file_uri_to_path(uri: &str) -> Result<PathBuf, ClipboardError> {
    let bad = || ClipboardError::InvalidUri;

    let rest = uri.strip_prefix("file://").ok_or_else(bad)?;

    if cfg!(windows) {
        if let Some(path_part) = rest.strip_prefix('/') {
            // file:///C:/foo  →  C:\foo
            // Decode percent-escapes.
            let decoded_bytes = percent_decode(path_part)?;
            let decoded = String::from_utf8(decoded_bytes).map_err(|_| bad())?;
            // Normalise forward slashes to backslashes.
            let win_path = decoded.replace('/', "\\");
            Ok(PathBuf::from(win_path))
        } else {
            // file://server/share/foo  →  \\server\share\foo
            let decoded_bytes = percent_decode(rest)?;
            let decoded = String::from_utf8(decoded_bytes).map_err(|_| bad())?;
            let win_path = format!("\\\\{}", decoded.replace('/', "\\"));
            Ok(PathBuf::from(win_path))
        }
    } else {
        // Unix: file:///foo/bar  → /foo/bar
        // `rest` starts with `/` (the authority is empty, so the third slash
        // belongs to the path).
        if !rest.starts_with('/') {
            return Err(bad());
        }
        let decoded_bytes = percent_decode(rest)?;
        let decoded = String::from_utf8(decoded_bytes).map_err(|_| bad())?;
        Ok(PathBuf::from(decoded))
    }
}

// ---------------------------------------------------------------------------
// text/uri-list encode / decode (RFC 2483)
// ---------------------------------------------------------------------------

/// Encode a slice of [`Uri`] entries as a `text/uri-list` byte payload.
///
/// Each URI occupies its own line, separated by `\r\n` (RFC 2483). `File`
/// variants are converted via [`path_to_file_uri`]; `Other` variants are
/// written verbatim.
///
/// Returns [`ClipboardError::InvalidUri`] if any `File` entry is relative or
/// otherwise non-absolute.
pub(crate) fn encode_uri_list(uris: &[Uri]) -> Result<Vec<u8>, ClipboardError> {
    let mut out = String::new();
    for uri in uris {
        match uri {
            Uri::File(path) => {
                let s = path_to_file_uri(path)?;
                out.push_str(&s);
            }
            Uri::Other(s) => {
                out.push_str(s);
            }
        }
        out.push_str("\r\n");
    }
    Ok(out.into_bytes())
}

/// Decode a `text/uri-list` byte payload into [`Uri`] entries.
///
/// Comment lines (starting with `#`) are dropped per RFC 2483. Lines with
/// `file://` scheme become `Uri::File(PathBuf)` after percent-decoding. All
/// other non-empty lines become `Uri::Other(String)`. Empty lines are skipped.
///
/// Returns [`ClipboardError::InvalidUri`] on a malformed percent-escape.
pub(crate) fn decode_uri_list(bytes: &[u8]) -> Result<Vec<Uri>, ClipboardError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ClipboardError::io_other("uri-list is not valid UTF-8"))?;

    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("file://") {
            let path = file_uri_to_path(line)?;
            out.push(Uri::File(path));
        } else {
            out.push(Uri::Other(line.to_owned()));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // percent_encode / percent_decode
    // -----------------------------------------------------------------------

    #[test]
    fn percent_encode_spaces() {
        assert_eq!(percent_encode(b"foo bar"), "foo%20bar");
    }

    #[test]
    fn percent_encode_non_ascii() {
        // "café" in UTF-8: c3 a9 for 'é'
        let input = "café".as_bytes();
        let enc = percent_encode(input);
        assert!(enc.contains("%C3%A9"), "expected %C3%A9 in {enc:?}");
    }

    #[test]
    fn percent_decode_roundtrip() {
        let original = b"hello world \xc3\xa9";
        let encoded = percent_encode(original);
        let decoded = percent_decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn percent_decode_invalid_hex() {
        assert!(percent_decode("%ZZ").is_err(), "expected error on %ZZ");
    }

    #[test]
    fn percent_decode_truncated() {
        assert!(
            percent_decode("%2").is_err(),
            "expected error on truncated %2"
        );
    }

    // -----------------------------------------------------------------------
    // path_to_file_uri / file_uri_to_path — Unix cases (always run)
    // -----------------------------------------------------------------------

    #[cfg(not(windows))]
    #[test]
    fn unix_simple_path_roundtrip() {
        let path = Path::new("/foo/bar");
        let uri = path_to_file_uri(path).unwrap();
        assert_eq!(uri, "file:///foo/bar");
        let back = file_uri_to_path(&uri).unwrap();
        assert_eq!(back, PathBuf::from("/foo/bar"));
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_path_with_spaces() {
        let path = Path::new("/foo/bar baz.txt");
        let uri = path_to_file_uri(path).unwrap();
        assert_eq!(uri, "file:///foo/bar%20baz.txt");
        let back = file_uri_to_path(&uri).unwrap();
        assert_eq!(back, PathBuf::from("/foo/bar baz.txt"));
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_path_non_ascii() {
        let path = Path::new("/café/naïve.txt");
        let uri = path_to_file_uri(path).unwrap();
        // 'é' → %C3%A9, 'ï' → %C3%AF
        assert!(
            uri.starts_with("file:///"),
            "should start with file:///: {uri}"
        );
        assert!(uri.contains("%C3%A9"), "expected %C3%A9: {uri}");
        let back = file_uri_to_path(&uri).unwrap();
        assert_eq!(back, PathBuf::from("/café/naïve.txt"));
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_relative_path_rejected() {
        let path = Path::new("relative/path");
        assert!(
            path_to_file_uri(path).is_err(),
            "relative path should be rejected"
        );
    }

    // -----------------------------------------------------------------------
    // path_to_file_uri / file_uri_to_path — Windows cases (Windows runner only)
    // -----------------------------------------------------------------------

    #[cfg(windows)]
    #[test]
    fn windows_drive_path_roundtrip() {
        let path = Path::new("C:\\foo\\bar.txt");
        let uri = path_to_file_uri(path).unwrap();
        assert_eq!(uri, "file:///C:/foo/bar.txt");
        let back = file_uri_to_path(&uri).unwrap();
        assert_eq!(back, PathBuf::from("C:\\foo\\bar.txt"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_unc_path_roundtrip() {
        let path = Path::new("\\\\server\\share\\x.txt");
        let uri = path_to_file_uri(path).unwrap();
        assert_eq!(uri, "file://server/share/x.txt");
        let back = file_uri_to_path(&uri).unwrap();
        assert_eq!(back, PathBuf::from("\\\\server\\share\\x.txt"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_with_spaces() {
        let path = Path::new("D:\\path with spaces\\x.png");
        let uri = path_to_file_uri(path).unwrap();
        assert_eq!(uri, "file:///D:/path%20with%20spaces/x.png");
        let back = file_uri_to_path(&uri).unwrap();
        assert_eq!(back, PathBuf::from("D:\\path with spaces\\x.png"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_relative_rejected() {
        let path = Path::new("relative\\path");
        assert!(path_to_file_uri(path).is_err());
    }

    // -----------------------------------------------------------------------
    // encode_uri_list
    // -----------------------------------------------------------------------

    #[cfg(not(windows))]
    #[test]
    fn encode_uri_list_single_file() {
        let uris = [Uri::File(PathBuf::from("/foo/bar"))];
        let bytes = encode_uri_list(&uris).unwrap();
        assert_eq!(bytes, b"file:///foo/bar\r\n");
    }

    #[cfg(not(windows))]
    #[test]
    fn encode_uri_list_mixed() {
        let uris = [
            Uri::File(PathBuf::from("/foo/bar")),
            Uri::Other("https://example.com".to_owned()),
        ];
        let bytes = encode_uri_list(&uris).unwrap();
        assert_eq!(bytes, b"file:///foo/bar\r\nhttps://example.com\r\n");
    }

    #[cfg(not(windows))]
    #[test]
    fn encode_uri_list_rejects_relative() {
        let uris = [Uri::File(PathBuf::from("relative/path"))];
        assert!(
            encode_uri_list(&uris).is_err(),
            "encode should reject relative File paths"
        );
    }

    #[test]
    fn encode_uri_list_empty() {
        let bytes = encode_uri_list(&[]).unwrap();
        assert!(bytes.is_empty());
    }

    #[test]
    fn encode_uri_list_other_only() {
        let uris = [Uri::Other("https://example.com".to_owned())];
        let bytes = encode_uri_list(&uris).unwrap();
        assert_eq!(bytes, b"https://example.com\r\n");
    }

    // -----------------------------------------------------------------------
    // decode_uri_list
    // -----------------------------------------------------------------------

    #[test]
    fn decode_uri_list_skips_comments_and_blanks() {
        let input = b"# This is a comment\r\n\r\nhttps://example.com\r\n";
        let uris = decode_uri_list(input).unwrap();
        assert_eq!(uris.len(), 1);
        assert!(matches!(&uris[0], Uri::Other(s) if s == "https://example.com"));
    }

    #[cfg(not(windows))]
    #[test]
    fn decode_uri_list_file_entries() {
        let input = b"file:///foo/bar%20baz\r\nhttps://example.com\r\n";
        let uris = decode_uri_list(input).unwrap();
        assert_eq!(uris.len(), 2);
        assert!(matches!(&uris[0], Uri::File(p) if p.as_os_str() == "/foo/bar baz"));
        assert!(matches!(&uris[1], Uri::Other(s) if s == "https://example.com"));
    }

    #[cfg(not(windows))]
    #[test]
    fn encode_decode_roundtrip() {
        let uris = vec![
            Uri::File(PathBuf::from("/foo/bar")),
            Uri::File(PathBuf::from("/path with spaces/x.txt")),
            Uri::Other("https://example.com".to_owned()),
        ];
        let bytes = encode_uri_list(&uris).unwrap();
        let decoded = decode_uri_list(&bytes).unwrap();
        assert_eq!(uris, decoded);
    }

    #[test]
    fn decode_uri_list_bad_percent() {
        let input = b"file:///foo/%ZZ/bar\r\n";
        assert!(
            decode_uri_list(input).is_err(),
            "malformed percent-escape should error"
        );
    }
}
