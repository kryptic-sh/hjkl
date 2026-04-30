//! [`Uri`] — typed URI for clipboard uri-list operations.
//!
//! Handles percent-encoding/decoding and Windows UNC path mapping.

use std::path::PathBuf;

/// A URI entry in a `text/uri-list` clipboard payload.
#[derive(Debug, Clone)]
pub enum Uri {
    /// A local file path. Must be absolute — relative paths are rejected with
    /// [`ClipboardError::InvalidUri`][crate::ClipboardError::InvalidUri].
    File(PathBuf),
    /// Any non-file URI (e.g. `https://example.com`). Passed through verbatim.
    Other(String),
}
