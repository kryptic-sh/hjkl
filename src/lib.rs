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
pub(crate) mod cf_hdrop;
pub(crate) mod cf_html;
pub(crate) mod dib_png;
pub(crate) mod oneshot;
pub(crate) mod osc52;
pub(crate) mod reply;

pub use error::ClipboardError;
pub use mime::MimeType;
pub use selection::Selection;
pub use uri::Uri;

// ---------------------------------------------------------------------------
// Backend selector — grows as phases land.
// ---------------------------------------------------------------------------

/// Which backend is active for this Clipboard handle.
enum ClipboardBackend {
    /// Wayland data-control (Linux, phase 6b+).
    #[cfg(target_os = "linux")]
    Wayland,
    /// X11 via XCB (Linux, phase 5b+).
    #[cfg(target_os = "linux")]
    X11,
    /// Scaffold placeholder for platforms/phases not yet wired.
    Unimplemented,
}

/// A handle to the system clipboard.
///
/// Internally selects the best available backend (Wayland data-control, X11
/// XCB, macOS NSPasteboard, Win32, or OSC 52 terminal fallback). The backend
/// is chosen once at construction time.
///
/// All methods take `&self` — the handle is cheaply clonable and shareable
/// across threads.
pub struct Clipboard {
    backend: ClipboardBackend,
}

impl Clipboard {
    /// Construct a new clipboard handle, probing for the best available backend.
    ///
    /// Probe order (Linux): Wayland → X11 → error.
    ///
    /// # Known limitation (Phase 7 fix required)
    ///
    /// Both `wayland_thread()` and `x11_thread()` use OnceLock to memoize the
    /// first connection attempt. If `Clipboard::new()` is called multiple times
    /// and the first Wayland attempt produced an Io error (instead of
    /// LibNotFound/NoDisplay/FocusRequired), subsequent calls will not retry
    /// X11 — they will see the same Io error from the OnceLock. For typical
    /// usage (one `Clipboard::new()` per process) this is not observable.
    /// Phase 7 must fix by making ClipboardError Clone or storing kind tags.
    pub fn new() -> Result<Self, ClipboardError> {
        #[cfg(target_os = "linux")]
        {
            // Prefer Wayland if available.
            match backend::wayland_thread::wayland_thread() {
                Ok(_) => {
                    return Ok(Self {
                        backend: ClipboardBackend::Wayland,
                    });
                }
                // Fall through to X11 when Wayland is absent or has no data-control.
                Err(ClipboardError::LibNotFound)
                | Err(ClipboardError::NoDisplay)
                | Err(ClipboardError::FocusRequired) => {}
                Err(e) => return Err(e),
            }

            // Try X11; fall through on LibNotFound / NoDisplay.
            match backend::x11_thread::x11_thread() {
                Ok(_) => {
                    return Ok(Self {
                        backend: ClipboardBackend::X11,
                    });
                }
                Err(ClipboardError::LibNotFound) | Err(ClipboardError::NoDisplay) => {}
                Err(e) => return Err(e),
            }
        }
        // Other backends (macOS, Windows, OSC 52) land in later phases.
        Err(ClipboardError::NoDisplay)
    }

    // -------------------------------------------------------------------------
    // Sync API
    // -------------------------------------------------------------------------

    /// Write `bytes` to `sel` as `mime`.
    #[allow(unused_variables)]
    pub fn set(&self, sel: Selection, mime: MimeType, bytes: &[u8]) -> Result<(), ClipboardError> {
        match &self.backend {
            #[cfg(target_os = "linux")]
            ClipboardBackend::Wayland => {
                let thread = backend::wayland_thread::wayland_thread()?;
                backend::wayland_thread::set_clipboard(thread, sel, &mime, bytes)
            }
            #[cfg(target_os = "linux")]
            ClipboardBackend::X11 => {
                let thread = backend::x11_thread::x11_thread()?;
                backend::x11_thread::set_clipboard(thread, sel, &mime, bytes)
            }
            ClipboardBackend::Unimplemented => unimplemented!("phase 0 scaffold"),
        }
    }

    /// Read the current contents of `sel` as `mime`.
    #[allow(unused_variables)]
    pub fn get(&self, sel: Selection, mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        match &self.backend {
            #[cfg(target_os = "linux")]
            ClipboardBackend::Wayland => {
                let thread = backend::wayland_thread::wayland_thread()?;
                backend::wayland_thread::get_clipboard(thread, sel, &mime)
            }
            #[cfg(target_os = "linux")]
            ClipboardBackend::X11 => {
                let thread = backend::x11_thread::x11_thread()?;
                backend::x11_thread::get_clipboard(thread, sel, &mime)
            }
            ClipboardBackend::Unimplemented => unimplemented!("phase 0 scaffold"),
        }
    }

    /// Clear `sel`.
    #[allow(unused_variables)]
    pub fn clear(&self, sel: Selection) -> Result<(), ClipboardError> {
        match &self.backend {
            #[cfg(target_os = "linux")]
            ClipboardBackend::Wayland => {
                let thread = backend::wayland_thread::wayland_thread()?;
                backend::wayland_thread::clear_clipboard(thread, sel)
            }
            #[cfg(target_os = "linux")]
            ClipboardBackend::X11 => {
                let thread = backend::x11_thread::x11_thread()?;
                backend::x11_thread::clear_clipboard(thread, sel)
            }
            ClipboardBackend::Unimplemented => unimplemented!("phase 0 scaffold"),
        }
    }

    /// Return the MIME types currently available in `sel`.
    #[allow(unused_variables)]
    pub fn available(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        match &self.backend {
            #[cfg(target_os = "linux")]
            ClipboardBackend::Wayland => {
                let thread = backend::wayland_thread::wayland_thread()?;
                backend::wayland_thread::available_clipboard(thread, sel)
            }
            #[cfg(target_os = "linux")]
            ClipboardBackend::X11 => {
                let thread = backend::x11_thread::x11_thread()?;
                backend::x11_thread::available_clipboard(thread, sel)
            }
            ClipboardBackend::Unimplemented => unimplemented!("phase 0 scaffold"),
        }
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
    /// [`ClipboardError::InvalidUri`]. Encoding validation happens before
    /// the backend is called, so encoding errors are visible even in tests
    /// that don't wire up a real backend.
    pub fn set_uri_list(&self, sel: Selection, uris: &[Uri]) -> Result<(), ClipboardError> {
        // Encoding validates URIs (relative paths → InvalidUri) before
        // touching the backend, so encoding errors surface immediately.
        let bytes = crate::uri::encode_uri_list(uris)?;
        self.set(sel, MimeType::UriList, &bytes)
    }

    /// Read a uri-list from `sel` and parse it into typed [`Uri`] values.
    pub fn get_uri_list(&self, sel: Selection) -> Result<Vec<Uri>, ClipboardError> {
        let bytes = self.get(sel, MimeType::UriList)?;
        crate::uri::decode_uri_list(&bytes)
    }
}
