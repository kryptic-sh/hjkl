//! Backend trait and platform probe.
//!
//! Each platform module implements `Backend`. `probe()` selects the best
//! available backend at runtime.

#[cfg(target_os = "linux")]
pub(crate) mod dlopen;
#[cfg(target_os = "macos")]
pub(crate) mod macos;
pub(crate) mod osc52;
#[cfg(target_os = "linux")]
pub(crate) mod wayland;
#[cfg(target_os = "linux")]
pub(crate) mod wayland_socket;
#[cfg(target_os = "linux")]
pub(crate) mod wayland_thread;
#[cfg(target_os = "linux")]
pub(crate) mod wayland_wire;
#[cfg(target_os = "windows")]
pub(crate) mod windows;
#[cfg(target_os = "linux")]
pub(crate) mod x11;
#[cfg(target_os = "linux")]
pub(crate) mod x11_thread;

use crate::{ClipboardError, MimeType, Selection};

/// The internal trait implemented by every clipboard backend.
///
/// `get` and `available` are unused on Linux (X11/Wayland threads handle them
/// directly). They exist for the Windows/macOS backends (Phase 3/4) where the
/// Backend trait is the dispatch mechanism.
#[allow(dead_code)]
pub(crate) trait Backend: Send + Sync + 'static {
    fn set(&self, sel: Selection, mime: MimeType, bytes: &[u8]) -> Result<(), ClipboardError>;

    fn get(&self, sel: Selection, mime: MimeType) -> Result<Vec<u8>, ClipboardError>;

    fn clear(&self, sel: Selection) -> Result<(), ClipboardError>;

    fn available(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError>;
}
