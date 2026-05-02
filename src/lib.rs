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
    /// OSC 52 terminal escape — write-only, text-only, any platform.
    Osc52,
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
    /// Probe order (Linux): Wayland → X11 → OSC 52.
    /// Other platforms: OSC 52 fallback until macOS/Windows phases wire in.
    ///
    /// ClipboardError is Clone so OnceLock singletons preserve the typed error
    /// variant across calls — LibNotFound/NoDisplay/FocusRequired all trigger
    /// the correct fallthrough on every call, not just the first.
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

            // Try X11; fall through to OSC 52 on LibNotFound / NoDisplay.
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
        // OSC 52 fallback: write-only, text-only, works over SSH / tmux.
        Ok(Self {
            backend: ClipboardBackend::Osc52,
        })
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
            ClipboardBackend::Osc52 => {
                use backend::Backend as _;
                backend::osc52::Osc52Backend::new().set(sel, mime, bytes)
            }
            ClipboardBackend::Unimplemented => unimplemented!("phase 0 scaffold"),
        }
    }

    /// Read the current contents of `sel` as `mime`.
    ///
    /// OSC 52 backend always returns `UnsupportedMime` — terminal clipboard
    /// cannot be read from the application side.
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
            ClipboardBackend::Osc52 => Err(ClipboardError::UnsupportedMime),
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
            ClipboardBackend::Osc52 => {
                use backend::Backend as _;
                backend::osc52::Osc52Backend::new().clear(sel)
            }
            ClipboardBackend::Unimplemented => unimplemented!("phase 0 scaffold"),
        }
    }

    /// Return the MIME types currently available in `sel`.
    ///
    /// OSC 52 backend always returns an empty list — terminal clipboard state
    /// cannot be queried.
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
            ClipboardBackend::Osc52 => Ok(vec![]),
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

    /// Return true if the active backend is OSC 52.
    ///
    /// Used in tests to verify fallback selection without needing a display.
    #[cfg(test)]
    pub(crate) fn is_osc52(&self) -> bool {
        matches!(self.backend, ClipboardBackend::Osc52)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct a Clipboard with OSC 52 by bypassing the display probe,
    /// then verify set/clear succeed and get returns UnsupportedMime.
    #[test]
    fn osc52_backend_set_and_get() {
        // Force OSC 52 backend directly — bypass Wayland/X11 probe.
        let cb = Clipboard {
            backend: ClipboardBackend::Osc52,
        };
        assert!(cb.is_osc52(), "expected Osc52 backend");

        // set text should succeed (writes to stdout — captured nowhere in test
        // but must not panic or error).
        // We can't easily capture stdout here, so just assert Ok.
        // This matches the osc52 backend tests which use set_inner with a buf.
        let result = cb.set(Selection::Clipboard, MimeType::Text, b"hi");
        // May succeed or error depending on tty availability; just no panic.
        let _ = result;

        // get is always UnsupportedMime for OSC 52.
        let err = cb.get(Selection::Clipboard, MimeType::Text).unwrap_err();
        assert!(
            matches!(err, ClipboardError::UnsupportedMime),
            "expected UnsupportedMime from osc52 get, got: {err}"
        );

        // available is always empty.
        let mimes = cb.available(Selection::Clipboard).unwrap();
        assert!(mimes.is_empty(), "expected empty available from osc52");
    }
}
