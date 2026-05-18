//! [`ClipboardError`] — the single error type for this crate.

use std::fmt;
use std::io;
use std::sync::Arc;

/// Errors that can be returned by clipboard operations.
///
/// # Cloneability note
///
/// `io::Error` does not implement `Clone`. To make `ClipboardError` cloneable
/// (required so singletons can store `OnceLock<Result<T, ClipboardError>>`
/// and return typed errors on every call), the `Io` variant wraps `io::Error`
/// in `Arc`. Callers matching on `Io` can deref to `&io::Error` via `&**arc`.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ClipboardError {
    /// A required native library (libxcb, libwayland-client) was not found.
    LibNotFound,
    /// No usable display server or TTY was detected.
    NoDisplay,
    /// The payload exceeds the OSC 52 cap or the platform maximum.
    PayloadTooLarge,
    /// Wayland compositor lacks a data-control protocol implementation.
    FocusRequired,
    /// The requested MIME type is not supported by the active backend (e.g.
    /// non-text over OSC 52).
    UnsupportedMime,
    /// The active backend does not implement async for this operation. Check
    /// [`Capabilities`][crate::Capabilities] for `ASYNC_*` flags before calling
    /// async methods.
    UnsupportedAsync,
    /// A URI was relative or otherwise malformed (RFC 3986 requires absolute).
    InvalidUri,
    /// The active backend was not available at runtime (transient — display
    /// server died, library load failed mid-session, etc).
    BackendUnavailable,
    /// An underlying I/O error.
    Io(Arc<io::Error>),
}

impl ClipboardError {
    /// Convenience constructor — wraps an `io::Error` in `Arc`.
    pub(crate) fn io(e: io::Error) -> Self {
        Self::Io(Arc::new(e))
    }

    /// Convenience constructor for string-described I/O errors.
    pub(crate) fn io_other(msg: &str) -> Self {
        Self::io(io::Error::other(msg))
    }
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LibNotFound => write!(f, "required native library not found"),
            Self::NoDisplay => write!(f, "no display server or TTY available"),
            Self::PayloadTooLarge => write!(f, "payload exceeds size cap"),
            Self::FocusRequired => {
                write!(f, "compositor requires focus (no data-control protocol)")
            }
            Self::UnsupportedMime => write!(f, "MIME type not supported by active backend"),
            Self::UnsupportedAsync => {
                write!(f, "async not supported by active backend")
            }
            Self::InvalidUri => write!(f, "URI must be absolute (RFC 3986)"),
            Self::BackendUnavailable => {
                write!(f, "backend was not available at runtime")
            }
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for ClipboardError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(&**e),
            _ => None,
        }
    }
}

impl From<io::Error> for ClipboardError {
    fn from(e: io::Error) -> Self {
        Self::io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_clipboard_error_smoke() {
        let variants: Vec<ClipboardError> = vec![
            ClipboardError::LibNotFound,
            ClipboardError::NoDisplay,
            ClipboardError::PayloadTooLarge,
            ClipboardError::FocusRequired,
            ClipboardError::UnsupportedMime,
            ClipboardError::UnsupportedAsync,
            ClipboardError::InvalidUri,
            ClipboardError::BackendUnavailable,
            ClipboardError::io_other("test io error"),
        ];
        for v in &variants {
            let cloned = v.clone();
            // Display impls must agree between original and clone.
            assert_eq!(
                v.to_string(),
                cloned.to_string(),
                "clone Display mismatch for {v:?}"
            );
        }
    }

    #[test]
    fn io_arc_clone_shares_message() {
        let e = ClipboardError::io_other("shared arc message");
        let e2 = e.clone();
        assert_eq!(e.to_string(), e2.to_string());
    }
}
