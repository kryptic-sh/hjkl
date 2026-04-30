//! [`ClipboardError`] — the single error type for this crate.

use std::fmt;
use std::io;

/// Errors that can be returned by clipboard operations.
#[derive(Debug)]
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
    /// A URI was relative or otherwise malformed (RFC 3986 requires absolute).
    InvalidUri,
    /// An underlying I/O error.
    Io(io::Error),
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
            Self::InvalidUri => write!(f, "URI must be absolute (RFC 3986)"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for ClipboardError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for ClipboardError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
