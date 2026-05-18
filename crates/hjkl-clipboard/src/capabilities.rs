//! [`BackendKind`] + [`Capabilities`] — runtime introspection of the active
//! clipboard backend.

use bitflags::bitflags;

/// Stable identifier for the active backend.
///
/// Returned by [`Clipboard::kind`][crate::Clipboard::kind] for diagnostics,
/// status display, and feature gating without parsing strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BackendKind {
    /// Wayland data-control protocol.
    Wayland,
    /// X11 selections via XCB.
    X11,
    /// macOS NSPasteboard.
    MacOs,
    /// Win32 clipboard.
    Windows,
    /// OSC 52 terminal escape — write-only, text-only.
    Osc52,
    /// In-memory test backend.
    Mock,
    /// SSH-aware decorator wrapping a native backend with OSC 52 fallback.
    SshAware,
}

impl BackendKind {
    /// Stable lowercase identifier suitable for status lines or log fields.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Wayland => "wayland",
            Self::X11 => "x11",
            Self::MacOs => "macos",
            Self::Windows => "windows",
            Self::Osc52 => "osc52",
            Self::Mock => "mock",
            Self::SshAware => "ssh-aware",
        }
    }
}

impl std::fmt::Display for BackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

bitflags! {
    /// Per-backend capability flags. Returned by
    /// [`Clipboard::capabilities`][crate::Clipboard::capabilities].
    ///
    /// Callers should check the relevant flag before invoking a method that
    /// might return [`ClipboardError::UnsupportedMime`][crate::ClipboardError::UnsupportedMime]
    /// or [`ClipboardError::UnsupportedAsync`][crate::ClipboardError::UnsupportedAsync]
    /// — capability checks are cheap, error round-trips can be expensive
    /// (Wayland/X11 thread hop).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Capabilities: u32 {
        /// `set` works at all (sync).
        const WRITE          = 1 << 0;
        /// `get` works at all (sync).
        const READ           = 1 << 1;
        /// `clear` works (sync).
        const CLEAR          = 1 << 2;
        /// `available` returns a real, non-empty list when data is present
        /// (sync).
        const AVAILABLE      = 1 << 3;
        /// `Selection::Primary` is honored alongside `Selection::Clipboard`.
        const PRIMARY        = 1 << 4;
        /// Image MIME types (PNG, JPEG, etc) round-trip.
        const IMAGE          = 1 << 5;
        /// Rich-text MIME types (HTML, RTF) round-trip.
        const RICH_TEXT      = 1 << 6;
        /// `text/uri-list` (file copy/paste) round-trips.
        const URI_LIST       = 1 << 7;

        /// `set_async` is a real async operation (not a `ready()` wrapper).
        const ASYNC_WRITE     = 1 << 8;
        /// `get_async` is a real async operation.
        const ASYNC_READ      = 1 << 9;
        /// `clear_async` is a real async operation.
        const ASYNC_CLEAR     = 1 << 10;
        /// `available_async` is a real async operation.
        const ASYNC_AVAILABLE = 1 << 11;
    }
}
