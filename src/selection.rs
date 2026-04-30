//! [`Selection`] — clipboard vs primary selection discriminant.

/// Which clipboard selection to operate on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// The system clipboard (Ctrl+C / Ctrl+V on most platforms).
    Clipboard,
    /// The X11 primary selection (middle-click paste). No-op on non-X11
    /// platforms.
    Primary,
}
