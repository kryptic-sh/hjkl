//! [`Selection`] — clipboard vs primary selection discriminant.

/// Which clipboard selection to operate on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// The system clipboard (Ctrl+C / Ctrl+V on most platforms).
    Clipboard,
    /// The primary selection (middle-click paste). Available on X11 and on
    /// Wayland compositors that expose `zwp_primary_selection_v1`. No-op on
    /// macOS / Windows.
    Primary,
}
