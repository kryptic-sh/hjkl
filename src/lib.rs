//! Cross-platform clipboard library with rich types, async support, and OSC 52
//! fallback for SSH.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use hjkl_clipboard::{Clipboard, Selection, MimeType};
//!
//! let cb = Clipboard::new().unwrap();
//! cb.set(Selection::Clipboard, MimeType::Text, b"hello").unwrap();
//! let data = cb.get(Selection::Clipboard, MimeType::Text).unwrap();
//! assert_eq!(data, b"hello");
//! ```

// Phase 0 scaffold: most items are wired up but not yet called.
#![allow(dead_code)]

pub mod error;
pub mod mime;
pub mod selection;
pub mod uri;

pub(crate) mod backend;
pub(crate) mod base64;
pub(crate) mod oneshot;
pub(crate) mod osc52;
pub(crate) mod reply;

pub use error::ClipboardError;
pub use mime::MimeType;
pub use selection::Selection;
pub use uri::Uri;

/// A handle to the system clipboard.
///
/// Internally selects the best available backend (Wayland data-control, X11
/// XCB, macOS NSPasteboard, Win32, or OSC 52 terminal fallback). The backend
/// is chosen once at construction time.
///
/// All methods take `&self` — the handle is cheaply clonable and shareable
/// across threads.
pub struct Clipboard {
    // Phase 1 will fill this in.
    _private: (),
}

impl Clipboard {
    /// Construct a new clipboard handle, probing for the best available backend.
    pub fn new() -> Result<Self, ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    // -------------------------------------------------------------------------
    // Sync API
    // -------------------------------------------------------------------------

    /// Write `bytes` to `sel` as `mime`.
    pub fn set(
        &self,
        _sel: Selection,
        _mime: MimeType,
        _bytes: &[u8],
    ) -> Result<(), ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    /// Read the current contents of `sel` as `mime`.
    pub fn get(&self, _sel: Selection, _mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    /// Clear `sel`.
    pub fn clear(&self, _sel: Selection) -> Result<(), ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    /// Return the MIME types currently available in `sel`.
    pub fn available(&self, _sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    // -------------------------------------------------------------------------
    // Async API (hand-rolled Future, runtime-agnostic)
    // -------------------------------------------------------------------------

    /// Async version of [`set`][Self::set].
    pub async fn set_async(
        &self,
        _sel: Selection,
        _mime: MimeType,
        _bytes: &[u8],
    ) -> Result<(), ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    /// Async version of [`get`][Self::get].
    pub async fn get_async(
        &self,
        _sel: Selection,
        _mime: MimeType,
    ) -> Result<Vec<u8>, ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    /// Async version of [`clear`][Self::clear].
    pub async fn clear_async(&self, _sel: Selection) -> Result<(), ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    /// Async version of [`available`][Self::available].
    pub async fn available_async(&self, _sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    // -------------------------------------------------------------------------
    // Typed uri-list helpers
    // -------------------------------------------------------------------------

    /// Write a list of URIs to `sel`.
    ///
    /// Relative paths in `File` variants return
    /// [`ClipboardError::InvalidUri`].
    pub fn set_uri_list(&self, _sel: Selection, _uris: &[Uri]) -> Result<(), ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    /// Read a uri-list from `sel` and parse it into typed [`Uri`] values.
    pub fn get_uri_list(&self, _sel: Selection) -> Result<Vec<Uri>, ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }
}
