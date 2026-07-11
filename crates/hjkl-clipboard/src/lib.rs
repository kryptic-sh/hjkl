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
//!
//! # Custom backends
//!
//! [`Clipboard::with_backend`] accepts any `Box<dyn Backend>`, enabling test
//! mocks ([`backend::mock::MockBackend`]), decorators
//! ([`backend::ssh_aware::SshAwareBackend`]), or third-party impls. Check
//! [`Clipboard::capabilities`] before invoking methods that may return
//! [`ClipboardError::UnsupportedMime`] or [`ClipboardError::UnsupportedAsync`].

pub mod capabilities;
pub mod error;
pub mod mime;
pub mod selection;
pub mod uri;

pub mod backend;
pub(crate) mod base64;
pub(crate) mod cf_hdrop;
pub(crate) mod cf_html;
pub(crate) mod dib_png;
pub(crate) mod oneshot;
pub(crate) mod osc52;
pub(crate) mod reply;

pub use backend::Backend;
pub use capabilities::{BackendKind, Capabilities};
pub use error::ClipboardError;
pub use mime::MimeType;
pub use selection::Selection;
pub use uri::Uri;

/// A handle to the system clipboard.
///
/// Internally holds a `Box<dyn Backend>` chosen by [`Clipboard::new`] (probes
/// the best available platform backend) or supplied by the caller via
/// [`Clipboard::with_backend`].
///
/// Use [`Clipboard::kind`] / [`Clipboard::capabilities`] to introspect the
/// active backend before invoking methods that may return
/// [`ClipboardError::UnsupportedMime`] / [`ClipboardError::UnsupportedAsync`].
pub struct Clipboard {
    backend: Box<dyn Backend>,
}

impl Clipboard {
    /// Construct a new clipboard handle, probing for the best available
    /// backend.
    ///
    /// Probe order:
    /// - Linux: Wayland → X11 → OSC 52.
    /// - macOS: NSPasteboard (always available).
    /// - Windows: Win32 (always available).
    /// - Other: OSC 52.
    ///
    /// The `HJKL_CLIPBOARD` environment variable overrides the probe: set it to
    /// `osc52` to force the terminal OSC 52 backend on any platform (useful in
    /// headless/CI/PTY environments where touching the real system clipboard is
    /// unwanted — e.g. it avoids cross-process contention on the single shared
    /// macOS pasteboard).
    pub fn new() -> Result<Self, ClipboardError> {
        if let Some(forced) = Self::forced_backend() {
            return Ok(forced);
        }
        Self::probe()
    }

    /// Honor an explicit `HJKL_CLIPBOARD` backend selection. Returns `None`
    /// when the variable is unset or holds an unrecognized value (fall through
    /// to the platform probe).
    fn forced_backend() -> Option<Self> {
        match std::env::var("HJKL_CLIPBOARD").ok()?.trim() {
            "osc52" => Some(Self::with_backend(Box::new(
                backend::osc52::Osc52Backend::new(),
            ))),
            _ => None,
        }
    }

    /// Construct a clipboard handle from a caller-supplied backend.
    ///
    /// Use this for tests ([`backend::mock::MockBackend`]), decorators
    /// ([`backend::ssh_aware::SshAwareBackend`]), or any custom `Backend` impl.
    pub fn with_backend(backend: Box<dyn Backend>) -> Self {
        Self { backend }
    }

    #[cfg(target_os = "linux")]
    fn probe() -> Result<Self, ClipboardError> {
        // Prefer Wayland.
        match backend::wayland_backend::WaylandBackend::new() {
            Ok(b) => return Ok(Self::with_backend(Box::new(b))),
            Err(ClipboardError::LibNotFound)
            | Err(ClipboardError::NoDisplay)
            | Err(ClipboardError::FocusRequired) => {}
            Err(e) => return Err(e),
        }
        // Try X11.
        match backend::x11_backend::X11Backend::new() {
            Ok(b) => return Ok(Self::with_backend(Box::new(b))),
            Err(ClipboardError::LibNotFound) | Err(ClipboardError::NoDisplay) => {}
            Err(e) => return Err(e),
        }
        // OSC 52 fallback.
        Ok(Self::with_backend(Box::new(
            backend::osc52::Osc52Backend::new(),
        )))
    }

    #[cfg(target_os = "macos")]
    fn probe() -> Result<Self, ClipboardError> {
        Ok(Self::with_backend(Box::new(
            backend::macos::MacosBackend::new(),
        )))
    }

    #[cfg(target_os = "windows")]
    fn probe() -> Result<Self, ClipboardError> {
        Ok(Self::with_backend(Box::new(
            backend::windows::WindowsBackend::new(),
        )))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    fn probe() -> Result<Self, ClipboardError> {
        Ok(Self::with_backend(Box::new(
            backend::osc52::Osc52Backend::new(),
        )))
    }

    // -------------------------------------------------------------------------
    // Introspection
    // -------------------------------------------------------------------------

    /// Stable identifier for the active backend.
    pub fn kind(&self) -> BackendKind {
        self.backend.kind()
    }

    /// Capability bitmask for the active backend. Cheap — call before any
    /// op that could return `UnsupportedMime` / `UnsupportedAsync`.
    pub fn capabilities(&self) -> Capabilities {
        self.backend.capabilities()
    }

    /// Stable lowercase string identifier (kept for diagnostic output).
    /// Equivalent to `self.kind().as_str()`.
    pub fn backend_name(&self) -> &'static str {
        self.backend.kind().as_str()
    }

    // -------------------------------------------------------------------------
    // Sync API
    // -------------------------------------------------------------------------

    /// Write `bytes` to `sel` as `mime`.
    pub fn set(&self, sel: Selection, mime: MimeType, bytes: &[u8]) -> Result<(), ClipboardError> {
        self.backend.set(sel, mime, bytes)
    }

    /// Read the current contents of `sel` as `mime`.
    pub fn get(&self, sel: Selection, mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        self.backend.get(sel, mime)
    }

    /// Clear `sel`.
    pub fn clear(&self, sel: Selection) -> Result<(), ClipboardError> {
        self.backend.clear(sel)
    }

    /// Return the MIME types currently available in `sel`.
    pub fn available(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        self.backend.available(sel)
    }

    // -------------------------------------------------------------------------
    // Async API — backends opt in via Capabilities::ASYNC_*; default returns
    // ClipboardError::UnsupportedAsync.
    // -------------------------------------------------------------------------

    /// Async version of [`set`][Self::set]. Bytes are cloned so the future is
    /// `'static`.
    pub async fn set_async(
        &self,
        sel: Selection,
        mime: MimeType,
        bytes: &[u8],
    ) -> Result<(), ClipboardError> {
        self.backend.set_async(sel, mime, bytes.to_vec()).await
    }

    /// Async version of [`get`][Self::get].
    pub async fn get_async(
        &self,
        sel: Selection,
        mime: MimeType,
    ) -> Result<Vec<u8>, ClipboardError> {
        self.backend.get_async(sel, mime).await
    }

    /// Async version of [`clear`][Self::clear].
    pub async fn clear_async(&self, sel: Selection) -> Result<(), ClipboardError> {
        self.backend.clear_async(sel).await
    }

    /// Async version of [`available`][Self::available].
    pub async fn available_async(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        self.backend.available_async(sel).await
    }

    // -------------------------------------------------------------------------
    // Typed uri-list helpers
    // -------------------------------------------------------------------------

    /// Write a list of URIs to `sel`.
    ///
    /// Relative paths in `File` variants return [`ClipboardError::InvalidUri`].
    /// Encoding validation happens before the backend is called, so encoding
    /// errors are visible even in tests that don't wire up a real backend.
    pub fn set_uri_list(&self, sel: Selection, uris: &[Uri]) -> Result<(), ClipboardError> {
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
    /// Used in tests to verify fallback / forced backend selection without
    /// needing a display. Available on every platform so the
    /// `HJKL_CLIPBOARD=osc52` override can be verified on macOS/Windows too.
    #[cfg(test)]
    pub(crate) fn is_osc52(&self) -> bool {
        self.backend.kind() == BackendKind::Osc52
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn backend_name_returns_valid_string() {
        let valid = ["wayland", "x11", "macos", "windows", "osc52"];
        if let Ok(cb) = Clipboard::new() {
            assert!(
                valid.contains(&cb.backend_name()),
                "unexpected backend_name: {}",
                cb.backend_name()
            );
        }
    }

    /// Verify the exact OSC 52 escape sequence for a known payload.
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
        let cb = Clipboard::with_backend(Box::new(Osc52Backend::new()));
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

    /// `HJKL_CLIPBOARD=osc52` must force the OSC 52 backend on every platform,
    /// bypassing the native probe (e.g. the shared macOS pasteboard).
    #[test]
    fn env_override_forces_osc52_backend() {
        // Serialize env mutation so a parallel `cargo test` run can't observe a
        // torn value (nextest already isolates per-process).
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let prev = std::env::var("HJKL_CLIPBOARD").ok();
        // SAFETY: guarded by ENV_LOCK; the previous value is restored below.
        unsafe { std::env::set_var("HJKL_CLIPBOARD", "osc52") };

        let is_osc = Clipboard::new().expect("clipboard construct").is_osc52();

        // SAFETY: same guard; restore the prior environment.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HJKL_CLIPBOARD", v),
                None => std::env::remove_var("HJKL_CLIPBOARD"),
            }
        }

        assert!(is_osc, "HJKL_CLIPBOARD=osc52 must force the OSC 52 backend");
    }
}
