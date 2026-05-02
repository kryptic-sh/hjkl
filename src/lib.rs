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
#[derive(Clone)]
enum ClipboardBackend {
    /// Wayland data-control (Linux, phase 6b+).
    #[cfg(target_os = "linux")]
    Wayland,
    /// X11 via XCB (Linux, phase 5b+).
    #[cfg(target_os = "linux")]
    X11,
    /// macOS NSPasteboard (native).
    #[cfg(target_os = "macos")]
    Macos,
    /// Win32 clipboard (native).
    #[cfg(target_os = "windows")]
    Windows,
    /// OSC 52 terminal escape — write-only, text-only.
    /// Not used on macOS/Windows which always have their native backends.
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Osc52,
}

/// A handle to the system clipboard.
///
/// Internally selects the best available backend (Wayland data-control, X11
/// XCB, macOS NSPasteboard, Win32, or OSC 52 terminal fallback). The backend
/// is chosen once at construction time.
///
/// All methods take `&self`. The handle is `Clone` and `Send + Sync` — wrap in
/// `Arc` or clone freely to share across threads.
#[derive(Clone)]
pub struct Clipboard {
    backend: ClipboardBackend,
}

impl Clipboard {
    /// Construct a new clipboard handle, probing for the best available backend.
    ///
    /// Probe order:
    /// - Linux: Wayland → X11 → OSC 52.
    /// - macOS: NSPasteboard (always available; no fallback needed).
    /// - Windows: Win32 (always available; no fallback needed).
    /// - Other: OSC 52.
    ///
    /// `ClipboardError` is `Clone` so `OnceLock` singletons preserve the typed
    /// error variant across calls — `LibNotFound`/`NoDisplay`/`FocusRequired`
    /// all trigger the correct fallthrough on every call, not just the first.
    pub fn new() -> Result<Self, ClipboardError> {
        Self::new_impl()
    }

    #[cfg(target_os = "linux")]
    fn new_impl() -> Result<Self, ClipboardError> {
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

        // OSC 52 fallback: write-only, text-only, works over SSH / tmux.
        Ok(Self {
            backend: ClipboardBackend::Osc52,
        })
    }

    #[cfg(target_os = "macos")]
    fn new_impl() -> Result<Self, ClipboardError> {
        // NSPasteboard is always present on macOS.
        Ok(Self {
            backend: ClipboardBackend::Macos,
        })
    }

    #[cfg(target_os = "windows")]
    fn new_impl() -> Result<Self, ClipboardError> {
        // Win32 clipboard is always present on Windows.
        Ok(Self {
            backend: ClipboardBackend::Windows,
        })
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    fn new_impl() -> Result<Self, ClipboardError> {
        // OSC 52 fallback for other platforms.
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
            #[cfg(target_os = "macos")]
            ClipboardBackend::Macos => {
                use backend::Backend as _;
                backend::macos::MacosBackend::new().set(sel, mime, bytes)
            }
            #[cfg(target_os = "windows")]
            ClipboardBackend::Windows => {
                use backend::Backend as _;
                backend::windows::WindowsBackend::new().set(sel, mime, bytes)
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            ClipboardBackend::Osc52 => {
                use backend::Backend as _;
                backend::osc52::Osc52Backend::new().set(sel, mime, bytes)
            }
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
            #[cfg(target_os = "macos")]
            ClipboardBackend::Macos => {
                use backend::Backend as _;
                backend::macos::MacosBackend::new().get(sel, mime)
            }
            #[cfg(target_os = "windows")]
            ClipboardBackend::Windows => {
                use backend::Backend as _;
                backend::windows::WindowsBackend::new().get(sel, mime)
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            ClipboardBackend::Osc52 => Err(ClipboardError::UnsupportedMime),
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
            #[cfg(target_os = "macos")]
            ClipboardBackend::Macos => {
                use backend::Backend as _;
                backend::macos::MacosBackend::new().clear(sel)
            }
            #[cfg(target_os = "windows")]
            ClipboardBackend::Windows => {
                use backend::Backend as _;
                backend::windows::WindowsBackend::new().clear(sel)
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            ClipboardBackend::Osc52 => {
                use backend::Backend as _;
                backend::osc52::Osc52Backend::new().clear(sel)
            }
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
            #[cfg(target_os = "macos")]
            ClipboardBackend::Macos => {
                use backend::Backend as _;
                backend::macos::MacosBackend::new().available(sel)
            }
            #[cfg(target_os = "windows")]
            ClipboardBackend::Windows => {
                use backend::Backend as _;
                backend::windows::WindowsBackend::new().available(sel)
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            ClipboardBackend::Osc52 => Ok(vec![]),
        }
    }

    // -------------------------------------------------------------------------
    // Async API (hand-rolled Future, runtime-agnostic)
    // -------------------------------------------------------------------------

    /// Async version of [`set`][Self::set].
    ///
    /// Bytes are cloned inside the method so the future is `'static` and the
    /// caller's slice does not need to outlive the returned future.
    ///
    /// X11/Wayland: routes through the bg thread Oneshot future.
    /// macOS/Windows: the native backends are synchronous; async = sync wrapped
    ///   in `std::future::ready` (no blocking thread spawn).
    /// OSC 52: wraps the synchronous write in `std::future::ready`.
    #[allow(unused_variables)]
    pub async fn set_async(
        &self,
        sel: Selection,
        mime: MimeType,
        bytes: &[u8],
    ) -> Result<(), ClipboardError> {
        let bytes = bytes.to_vec();
        match &self.backend {
            #[cfg(target_os = "linux")]
            ClipboardBackend::Wayland => {
                let thread = backend::wayland_thread::wayland_thread()?;
                let fut =
                    thread.send_async(backend::wayland_thread::WaylandOp::Set { sel, mime, bytes });
                match fut.await {
                    backend::wayland_thread::WaylandOpResult::Set(r) => r,
                    _ => unreachable!(),
                }
            }
            #[cfg(target_os = "linux")]
            ClipboardBackend::X11 => {
                let thread = backend::x11_thread::x11_thread()?;
                let (mime_atom, mime_name) =
                    backend::x11_thread::mime_to_atom_or_name(&thread.atoms, &mime);
                let sel_atom = backend::x11_thread::sel_to_atom(&thread.atoms, sel);
                let fut = thread.send_async(backend::x11_thread::X11Op::Set {
                    sel_atom,
                    mime_atom,
                    mime_name,
                    bytes,
                });
                match fut.await {
                    backend::x11_thread::X11OpResult::Set(r) => r,
                    _ => unreachable!(),
                }
            }
            #[cfg(target_os = "macos")]
            ClipboardBackend::Macos => {
                use backend::Backend as _;
                std::future::ready(backend::macos::MacosBackend::new().set(sel, mime, &bytes)).await
            }
            #[cfg(target_os = "windows")]
            ClipboardBackend::Windows => {
                use backend::Backend as _;
                std::future::ready(backend::windows::WindowsBackend::new().set(sel, mime, &bytes))
                    .await
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            ClipboardBackend::Osc52 => {
                use backend::Backend as _;
                std::future::ready(backend::osc52::Osc52Backend::new().set(sel, mime, &bytes)).await
            }
        }
    }

    /// Async version of [`get`][Self::get].
    ///
    /// X11/Wayland: routes through the bg thread Oneshot future.
    /// macOS/Windows: sync-wrapped in `std::future::ready`.
    /// OSC 52: always returns `UnsupportedMime` (terminal clipboard is write-only).
    #[allow(unused_variables)]
    pub async fn get_async(
        &self,
        sel: Selection,
        mime: MimeType,
    ) -> Result<Vec<u8>, ClipboardError> {
        match &self.backend {
            #[cfg(target_os = "linux")]
            ClipboardBackend::Wayland => {
                let thread = backend::wayland_thread::wayland_thread()?;
                let fut = thread.send_async(backend::wayland_thread::WaylandOp::Get { sel, mime });
                match fut.await {
                    backend::wayland_thread::WaylandOpResult::Get(r) => r,
                    _ => unreachable!(),
                }
            }
            #[cfg(target_os = "linux")]
            ClipboardBackend::X11 => {
                let thread = backend::x11_thread::x11_thread()?;
                let (mime_atom, mime_name) =
                    backend::x11_thread::mime_to_atom_or_name(&thread.atoms, &mime);
                let sel_atom = backend::x11_thread::sel_to_atom(&thread.atoms, sel);
                let fut = thread.send_async(backend::x11_thread::X11Op::Get {
                    sel_atom,
                    mime_atom,
                    mime_name,
                });
                match fut.await {
                    backend::x11_thread::X11OpResult::Get(r) => r,
                    _ => unreachable!(),
                }
            }
            #[cfg(target_os = "macos")]
            ClipboardBackend::Macos => {
                use backend::Backend as _;
                std::future::ready(backend::macos::MacosBackend::new().get(sel, mime)).await
            }
            #[cfg(target_os = "windows")]
            ClipboardBackend::Windows => {
                use backend::Backend as _;
                std::future::ready(backend::windows::WindowsBackend::new().get(sel, mime)).await
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            ClipboardBackend::Osc52 => {
                std::future::ready(Err(ClipboardError::UnsupportedMime)).await
            }
        }
    }

    /// Async version of [`clear`][Self::clear].
    ///
    /// macOS/Windows: sync-wrapped in `std::future::ready`.
    #[allow(unused_variables)]
    pub async fn clear_async(&self, sel: Selection) -> Result<(), ClipboardError> {
        match &self.backend {
            #[cfg(target_os = "linux")]
            ClipboardBackend::Wayland => {
                let thread = backend::wayland_thread::wayland_thread()?;
                let fut = thread.send_async(backend::wayland_thread::WaylandOp::Clear { sel });
                match fut.await {
                    backend::wayland_thread::WaylandOpResult::Clear(r) => r,
                    _ => unreachable!(),
                }
            }
            #[cfg(target_os = "linux")]
            ClipboardBackend::X11 => {
                let thread = backend::x11_thread::x11_thread()?;
                let sel_atom = backend::x11_thread::sel_to_atom(&thread.atoms, sel);
                let fut = thread.send_async(backend::x11_thread::X11Op::Clear { sel_atom });
                match fut.await {
                    backend::x11_thread::X11OpResult::Clear(r) => r,
                    _ => unreachable!(),
                }
            }
            #[cfg(target_os = "macos")]
            ClipboardBackend::Macos => {
                use backend::Backend as _;
                std::future::ready(backend::macos::MacosBackend::new().clear(sel)).await
            }
            #[cfg(target_os = "windows")]
            ClipboardBackend::Windows => {
                use backend::Backend as _;
                std::future::ready(backend::windows::WindowsBackend::new().clear(sel)).await
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            ClipboardBackend::Osc52 => {
                use backend::Backend as _;
                std::future::ready(backend::osc52::Osc52Backend::new().clear(sel)).await
            }
        }
    }

    /// Async version of [`available`][Self::available].
    ///
    /// macOS/Windows: sync-wrapped in `std::future::ready`.
    /// OSC 52: always returns an empty list (terminal clipboard state cannot
    /// be queried).
    #[allow(unused_variables)]
    pub async fn available_async(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        match &self.backend {
            #[cfg(target_os = "linux")]
            ClipboardBackend::Wayland => {
                let thread = backend::wayland_thread::wayland_thread()?;
                let fut = thread.send_async(backend::wayland_thread::WaylandOp::Available { sel });
                match fut.await {
                    backend::wayland_thread::WaylandOpResult::Available(r) => r,
                    _ => unreachable!(),
                }
            }
            #[cfg(target_os = "linux")]
            ClipboardBackend::X11 => {
                let thread = backend::x11_thread::x11_thread()?;
                let sel_atom = backend::x11_thread::sel_to_atom(&thread.atoms, sel);
                let fut = thread.send_async(backend::x11_thread::X11Op::Available { sel_atom });
                match fut.await {
                    backend::x11_thread::X11OpResult::Available(r) => {
                        let raw_atoms = r?;
                        let mut mimes: Vec<MimeType> = Vec::new();
                        for atom in raw_atoms {
                            if let Some(mime) =
                                backend::x11_thread::atom_to_mime(&thread.atoms, atom)
                                && !mimes.contains(&mime)
                            {
                                mimes.push(mime);
                            }
                        }
                        Ok(mimes)
                    }
                    _ => unreachable!(),
                }
            }
            #[cfg(target_os = "macos")]
            ClipboardBackend::Macos => {
                use backend::Backend as _;
                std::future::ready(backend::macos::MacosBackend::new().available(sel)).await
            }
            #[cfg(target_os = "windows")]
            ClipboardBackend::Windows => {
                use backend::Backend as _;
                std::future::ready(backend::windows::WindowsBackend::new().available(sel)).await
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            ClipboardBackend::Osc52 => std::future::ready(Ok(vec![])).await,
        }
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

    /// Returns a stable identifier for the active backend.
    ///
    /// Useful for diagnostics and detecting silent OSC 52 fallback (e.g. when
    /// no display server is reachable).
    pub fn backend_name(&self) -> &'static str {
        match &self.backend {
            #[cfg(target_os = "linux")]
            ClipboardBackend::Wayland => "wayland",
            #[cfg(target_os = "linux")]
            ClipboardBackend::X11 => "x11",
            #[cfg(target_os = "macos")]
            ClipboardBackend::Macos => "macos",
            #[cfg(target_os = "windows")]
            ClipboardBackend::Windows => "windows",
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            ClipboardBackend::Osc52 => "osc52",
        }
    }

    /// Return true if the active backend is OSC 52.
    ///
    /// Used in tests to verify fallback selection without needing a display.
    #[cfg(all(test, not(any(target_os = "macos", target_os = "windows"))))]
    pub(crate) fn is_osc52(&self) -> bool {
        matches!(self.backend, ClipboardBackend::Osc52)
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn backend_name_returns_valid_string() {
        let valid = ["wayland", "x11", "macos", "windows", "osc52"];
        // Clipboard::new() may fail in CI without a display — that's fine;
        // we only check the return value when construction succeeds.
        if let Ok(cb) = Clipboard::new() {
            assert!(
                valid.contains(&cb.backend_name()),
                "unexpected backend_name: {}",
                cb.backend_name()
            );
        }
    }

    /// Verify the exact OSC 52 escape sequence for a known payload.
    ///
    /// Uses `Osc52Backend::set_inner` with a `Vec<u8>` sink — hermetic,
    /// no stdout, no TTY dependency.
    ///
    /// Wire format (non-tmux): `\x1b]52;c;<base64>\x07`
    /// Wire format (tmux DCS): `\x1bPtmux;\x1b\x1b]52;c;<base64>\x07\x1b\`
    /// "hello" always encodes to base64 body `aGVsbG8=`.
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[test]
    fn osc52_backend_set_and_get() {
        use backend::osc52::Osc52Backend;
        use osc52::is_in_tmux;

        let b = Osc52Backend::new();
        let mut buf = Vec::new();

        b.set_inner(Selection::Clipboard, MimeType::Text, b"hello", &mut buf)
            .expect("set_inner failed");

        let seq = std::str::from_utf8(&buf).expect("output not UTF-8");

        // Adapt expected framing to the runtime environment.
        let body = if is_in_tmux() {
            assert!(
                seq.starts_with("\x1bPtmux;\x1b\x1b]52;c;"),
                "wrong DCS prefix: {seq:?}"
            );
            assert!(seq.ends_with("\x07\x1b\\"), "wrong DCS suffix: {seq:?}");
            seq.strip_prefix("\x1bPtmux;\x1b\x1b]52;c;")
                .unwrap()
                .strip_suffix("\x07\x1b\\")
                .unwrap()
        } else {
            assert!(seq.starts_with("\x1b]52;c;"), "wrong OSC prefix: {seq:?}");
            assert!(seq.ends_with('\x07'), "wrong BEL suffix: {seq:?}");
            seq.strip_prefix("\x1b]52;c;")
                .unwrap()
                .strip_suffix('\x07')
                .unwrap()
        };

        assert_eq!(body, "aGVsbG8=", "base64 mismatch for 'hello'");

        // get is always UnsupportedMime for OSC 52.
        let cb = Clipboard {
            backend: ClipboardBackend::Osc52,
        };
        assert!(cb.is_osc52(), "expected Osc52 backend");
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
