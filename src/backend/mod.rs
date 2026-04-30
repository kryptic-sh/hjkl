//! Backend trait and platform probe.
//!
//! Each platform module implements `Backend`. `probe()` selects the best
//! available backend at runtime.

pub(crate) mod bg_thread;
pub(crate) mod cf_hdrop;
pub(crate) mod cf_html;
pub(crate) mod dib_png;
pub(crate) mod dlopen;
pub(crate) mod macos;
pub(crate) mod osc52;
pub(crate) mod wayland;
pub(crate) mod windows;
pub(crate) mod x11;

use crate::{ClipboardError, MimeType, Selection};

/// The internal trait implemented by every clipboard backend.
pub(crate) trait Backend: Send + Sync + 'static {
    fn set(&self, sel: Selection, mime: MimeType, bytes: &[u8]) -> Result<(), ClipboardError>;

    fn get(&self, sel: Selection, mime: MimeType) -> Result<Vec<u8>, ClipboardError>;

    fn clear(&self, sel: Selection) -> Result<(), ClipboardError>;

    fn available(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError>;
}
