//! [`SshAwareBackend`] — decorator that falls back to OSC 52 when the inner
//! backend can't service a write.
//!
//! On `set` / `set_async`: tries the inner backend first; if it returns
//! [`ClipboardError::BackendUnavailable`] / [`ClipboardError::UnsupportedMime`]
//! / [`ClipboardError::NoDisplay`], falls back to OSC 52 (terminal escape;
//! works over SSH + tmux).
//!
//! `get` / `clear` / `available` and their async variants are forwarded to the
//! inner backend unchanged — OSC 52 cannot read or enumerate, so there is no
//! useful fallback there.
//!
//! [`Capabilities`] is the union of the inner backend's caps and the OSC 52
//! caps (`WRITE | CLEAR`), so callers see both surfaces as available.
//!
//! # Example
//!
//! ```
//! use hjkl_clipboard::{BackendKind, Capabilities, Clipboard};
//! use hjkl_clipboard::backend::mock::MockBackend;
//! use hjkl_clipboard::backend::ssh_aware::SshAwareBackend;
//!
//! // Wrap any Backend — this example uses MockBackend so it compiles on every
//! // target. Production code wraps WaylandBackend / X11Backend / etc.
//! let native = MockBackend::new(BackendKind::Mock, Capabilities::all());
//! let cb = Clipboard::with_backend(Box::new(SshAwareBackend::new(Box::new(native))));
//! ```

use async_trait::async_trait;

use crate::{Backend, BackendKind, Capabilities, ClipboardError, MimeType, Selection};

use super::osc52::Osc52Backend;

/// Decorator that wraps a `Box<dyn Backend>` and falls back to OSC 52 on
/// write failures.
pub struct SshAwareBackend {
    inner: Box<dyn Backend>,
    osc52: Osc52Backend,
}

impl SshAwareBackend {
    /// Wrap `inner`. The OSC 52 fallback is constructed lazily on first
    /// fallback (cheap — `Osc52Backend::new()` is unit-struct construction).
    pub fn new(inner: Box<dyn Backend>) -> Self {
        Self {
            inner,
            osc52: Osc52Backend::new(),
        }
    }

    fn should_fallback(err: &ClipboardError) -> bool {
        matches!(
            err,
            ClipboardError::BackendUnavailable
                | ClipboardError::UnsupportedMime
                | ClipboardError::NoDisplay
                | ClipboardError::FocusRequired
        )
    }
}

#[async_trait]
impl Backend for SshAwareBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::SshAware
    }

    fn capabilities(&self) -> Capabilities {
        // OSC 52 contributes WRITE | CLEAR. Inner contributes everything it
        // supports natively (READ, AVAILABLE, async, etc).
        self.inner.capabilities() | self.osc52.capabilities()
    }

    fn set(&self, sel: Selection, mime: MimeType, bytes: &[u8]) -> Result<(), ClipboardError> {
        match self.inner.set(sel, mime.clone(), bytes) {
            Err(e) if Self::should_fallback(&e) => self.osc52.set(sel, mime, bytes),
            other => other,
        }
    }

    fn get(&self, sel: Selection, mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        // OSC 52 cannot read; only the inner backend can satisfy reads.
        self.inner.get(sel, mime)
    }

    fn clear(&self, sel: Selection) -> Result<(), ClipboardError> {
        match self.inner.clear(sel) {
            Err(e) if Self::should_fallback(&e) => self.osc52.clear(sel),
            other => other,
        }
    }

    fn available(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        // OSC 52 cannot enumerate; only the inner backend has real data.
        self.inner.available(sel)
    }

    async fn set_async(
        &self,
        sel: Selection,
        mime: MimeType,
        bytes: Vec<u8>,
    ) -> Result<(), ClipboardError> {
        // Prefer inner async if available; otherwise sync inner; on fallback
        // case use OSC 52 sync (no async OSC 52).
        let inner_caps = self.inner.capabilities();
        let primary = if inner_caps.contains(Capabilities::ASYNC_WRITE) {
            self.inner.set_async(sel, mime.clone(), bytes.clone()).await
        } else {
            self.inner.set(sel, mime.clone(), &bytes)
        };
        match primary {
            Err(e) if Self::should_fallback(&e) => self.osc52.set(sel, mime, &bytes),
            other => other,
        }
    }

    async fn get_async(&self, sel: Selection, mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        if self.inner.capabilities().contains(Capabilities::ASYNC_READ) {
            self.inner.get_async(sel, mime).await
        } else {
            self.inner.get(sel, mime)
        }
    }

    async fn clear_async(&self, sel: Selection) -> Result<(), ClipboardError> {
        let primary = if self
            .inner
            .capabilities()
            .contains(Capabilities::ASYNC_CLEAR)
        {
            self.inner.clear_async(sel).await
        } else {
            self.inner.clear(sel)
        };
        match primary {
            Err(e) if Self::should_fallback(&e) => self.osc52.clear(sel),
            other => other,
        }
    }

    async fn available_async(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        if self
            .inner
            .capabilities()
            .contains(Capabilities::ASYNC_AVAILABLE)
        {
            self.inner.available_async(sel).await
        } else {
            self.inner.available(sel)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Clipboard;
    use crate::backend::mock::MockBackend;

    #[test]
    fn falls_back_to_osc52_on_unsupported_mime() {
        // Inner mock advertises full caps but rejects everything via default
        // (preset_get not called → returns UnsupportedMime).
        let inner = MockBackend::new(BackendKind::Mock, Capabilities::WRITE);
        // MockBackend.set always succeeds — to test fallback we need a mock
        // that returns UnsupportedMime. Force it via preset_get? No — set is
        // hardcoded Ok. Instead use a mock with WRITE cap but inject error
        // by wrapping inner that fails. For now: use a custom failing inner.

        // Custom failing inner backend.
        struct FailingInner;
        #[async_trait]
        impl Backend for FailingInner {
            fn kind(&self) -> BackendKind {
                BackendKind::Mock
            }
            fn capabilities(&self) -> Capabilities {
                Capabilities::WRITE
            }
            fn set(&self, _: Selection, _: MimeType, _: &[u8]) -> Result<(), ClipboardError> {
                Err(ClipboardError::UnsupportedMime)
            }
            fn get(&self, _: Selection, _: MimeType) -> Result<Vec<u8>, ClipboardError> {
                Err(ClipboardError::UnsupportedMime)
            }
            fn clear(&self, _: Selection) -> Result<(), ClipboardError> {
                Err(ClipboardError::UnsupportedMime)
            }
            fn available(&self, _: Selection) -> Result<Vec<MimeType>, ClipboardError> {
                Ok(Vec::new())
            }
        }
        let _ = inner;

        let ssh = SshAwareBackend::new(Box::new(FailingInner));
        let cb = Clipboard::with_backend(Box::new(ssh));
        // Set should fall back to OSC 52, which writes to stdout and returns Ok.
        assert!(cb.set(Selection::Clipboard, MimeType::Text, b"hi").is_ok());
    }

    #[test]
    fn capabilities_are_union() {
        let inner = MockBackend::new(BackendKind::Mock, Capabilities::READ);
        let ssh = SshAwareBackend::new(Box::new(inner));
        let caps = ssh.capabilities();
        assert!(caps.contains(Capabilities::READ), "inner READ propagated");
        assert!(
            caps.contains(Capabilities::WRITE),
            "OSC 52 WRITE propagated"
        );
        assert!(
            caps.contains(Capabilities::CLEAR),
            "OSC 52 CLEAR propagated"
        );
    }

    #[test]
    fn kind_reports_ssh_aware() {
        let inner = MockBackend::new(BackendKind::Mock, Capabilities::all());
        let ssh = SshAwareBackend::new(Box::new(inner));
        assert_eq!(ssh.kind(), BackendKind::SshAware);
    }
}
