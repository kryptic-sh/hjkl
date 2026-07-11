//! Cross-platform clipboard library with rich types, async support, and
//! context-aware backend selection (native desktop, OSC 52 over SSH / tmux).
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

/// Returns `true` when the process is running inside an SSH session.
///
/// Checks the standard variables `sshd` exports into the session environment:
/// `SSH_TTY` (interactive session with a controlling terminal), `SSH_CONNECTION`,
/// and `SSH_CLIENT`. Any one being present is treated as "remote".
pub(crate) fn is_ssh_session() -> bool {
    std::env::var_os("SSH_TTY").is_some()
        || std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_CLIENT").is_some()
}

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
    /// Construct a new clipboard handle, choosing the backend that fits the
    /// current context.
    ///
    /// Selection order:
    /// 1. An explicit `HJKL_CLIPBOARD` override (see [`Self::forced_backend`]).
    /// 2. **SSH session** (`SSH_TTY` / `SSH_CONNECTION` / `SSH_CLIENT` set) →
    ///    OSC 52. Over SSH the machine's *native* clipboard belongs to the
    ///    remote host, not the user; the OSC 52 terminal escape is relayed by
    ///    the user's terminal emulator to their **local** clipboard (and is
    ///    wrapped for tmux passthrough automatically when `TMUX` is set).
    /// 3. Otherwise the local-desktop probe:
    ///    - Linux: Wayland → X11 → OSC 52.
    ///    - macOS: NSPasteboard (always available).
    ///    - Windows: Win32 (always available).
    ///    - Other: OSC 52.
    ///
    /// The SSH heuristic is overridable: set `HJKL_CLIPBOARD=x11` (or
    /// `wayland`) when relying on SSH X11 forwarding to reach the local
    /// clipboard natively, or `HJKL_CLIPBOARD=osc52` to force OSC 52 anywhere.
    pub fn new() -> Result<Self, ClipboardError> {
        if let Some(forced) = Self::forced_backend() {
            return Ok(forced);
        }
        // Context-aware default: an SSH session's native clipboard is the
        // remote host's, so route through OSC 52 to the user's local terminal.
        if is_ssh_session() {
            return Ok(Self::with_backend(Box::new(
                backend::osc52::Osc52Backend::new(),
            )));
        }
        Self::probe()
    }

    /// Honor an explicit `HJKL_CLIPBOARD` backend selection. Returns `None`
    /// when the variable is unset or holds a value that isn't usable on this
    /// platform (fall through to the context-aware default).
    ///
    /// Recognized values (case-insensitive): `osc52`, `native`, and the
    /// platform backend names `wayland` / `x11` (Linux), `macos`, `windows`.
    /// `native` forces the platform probe, bypassing the SSH heuristic. A
    /// named native backend that fails to initialize (e.g. `x11` with no
    /// display) falls through rather than erroring.
    fn forced_backend() -> Option<Self> {
        let raw = std::env::var("HJKL_CLIPBOARD").ok()?;
        match raw.trim().to_ascii_lowercase().as_str() {
            "osc52" => Some(Self::with_backend(Box::new(
                backend::osc52::Osc52Backend::new(),
            ))),
            // Bypass the SSH heuristic and use the local-desktop probe.
            "native" | "auto" => Self::probe().ok(),
            #[cfg(target_os = "linux")]
            "wayland" => backend::wayland_backend::WaylandBackend::new()
                .ok()
                .map(|b| Self::with_backend(Box::new(b))),
            #[cfg(target_os = "linux")]
            "x11" => backend::x11_backend::X11Backend::new()
                .ok()
                .map(|b| Self::with_backend(Box::new(b))),
            #[cfg(target_os = "macos")]
            "macos" => Some(Self::with_backend(Box::new(
                backend::macos::MacosBackend::new(),
            ))),
            #[cfg(target_os = "windows")]
            "windows" => Some(Self::with_backend(Box::new(
                backend::windows::WindowsBackend::new(),
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

    /// Serialize env mutation across all env-touching tests so a parallel
    /// `cargo test` run can't observe a torn value (nextest already isolates
    /// per-process).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `f` with `vars` applied (`Some` = set, `None` = remove), restoring
    /// each variable's prior value afterward. Serialized via [`ENV_LOCK`].
    fn with_env<R>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> R) -> R {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(String, Option<String>)> = vars
            .iter()
            .map(|(k, _)| ((*k).to_string(), std::env::var(k).ok()))
            .collect();
        for (k, v) in vars {
            // SAFETY: guarded by ENV_LOCK; restored below.
            unsafe {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
        let result = f();
        for (k, v) in saved {
            // SAFETY: same guard.
            unsafe {
                match v {
                    Some(val) => std::env::set_var(&k, val),
                    None => std::env::remove_var(&k),
                }
            }
        }
        result
    }

    /// `HJKL_CLIPBOARD=osc52` must force the OSC 52 backend on every platform,
    /// bypassing the native probe (e.g. the shared macOS pasteboard).
    #[test]
    fn env_override_forces_osc52_backend() {
        let is_osc = with_env(&[("HJKL_CLIPBOARD", Some("osc52"))], || {
            Clipboard::new().expect("clipboard construct").is_osc52()
        });
        assert!(is_osc, "HJKL_CLIPBOARD=osc52 must force the OSC 52 backend");
    }

    /// An SSH session (no explicit override) selects OSC 52 so writes reach the
    /// user's local terminal rather than the remote host's clipboard.
    #[test]
    fn ssh_session_selects_osc52() {
        let is_osc = with_env(
            &[
                ("HJKL_CLIPBOARD", None),
                ("SSH_TTY", Some("/dev/pts/0")),
                ("SSH_CONNECTION", None),
                ("SSH_CLIENT", None),
            ],
            || Clipboard::new().expect("clipboard construct").is_osc52(),
        );
        assert!(is_osc, "SSH session should select the OSC 52 backend");
    }

    /// An explicit `HJKL_CLIPBOARD` override still wins over the SSH heuristic
    /// (here forcing OSC 52, which is deterministic on every platform).
    #[test]
    fn explicit_override_wins_over_ssh_heuristic() {
        let is_osc = with_env(
            &[
                ("HJKL_CLIPBOARD", Some("osc52")),
                ("SSH_TTY", Some("/dev/pts/1")),
            ],
            || Clipboard::new().expect("clipboard construct").is_osc52(),
        );
        assert!(is_osc);
    }

    #[test]
    fn is_ssh_session_detects_each_variable() {
        for var in ["SSH_TTY", "SSH_CONNECTION", "SSH_CLIENT"] {
            let detected = with_env(
                &[
                    ("SSH_TTY", None),
                    ("SSH_CONNECTION", None),
                    ("SSH_CLIENT", None),
                    (var, Some("x")),
                ],
                is_ssh_session,
            );
            assert!(detected, "{var} should mark the session as SSH");
        }
    }

    #[test]
    fn is_ssh_session_false_without_ssh_vars() {
        let detected = with_env(
            &[
                ("SSH_TTY", None),
                ("SSH_CONNECTION", None),
                ("SSH_CLIENT", None),
            ],
            is_ssh_session,
        );
        assert!(!detected, "no SSH vars → not an SSH session");
    }
}
