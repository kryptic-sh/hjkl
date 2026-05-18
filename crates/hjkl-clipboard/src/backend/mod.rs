//! Backend trait and platform probe.
//!
//! Each platform module implements [`Backend`]. [`crate::Clipboard::new`]
//! selects the best available backend at runtime; [`crate::Clipboard::with_backend`]
//! lets callers inject any `Box<dyn Backend>` (mocks, decorators, custom impls).

#[cfg(target_os = "linux")]
pub(crate) mod dlopen;
#[cfg(target_os = "macos")]
pub(crate) mod macos;
pub mod mock;
pub(crate) mod osc52;
pub mod ssh_aware;
#[cfg(target_os = "linux")]
pub(crate) mod wayland;
#[cfg(target_os = "linux")]
pub mod wayland_backend;
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
pub mod x11_backend;
#[cfg(target_os = "linux")]
pub(crate) mod x11_thread;

use crate::{BackendKind, Capabilities, ClipboardError, MimeType, Selection};
use async_trait::async_trait;

/// Trait implemented by every clipboard backend.
///
/// Sync methods are required; async methods default to
/// [`ClipboardError::UnsupportedAsync`] so backends can opt into async
/// incrementally. Check
/// [`capabilities`][Backend::capabilities] for `ASYNC_*` flags before calling
/// async variants.
///
/// Implementations must be `Send + Sync + 'static` so they can be stored in
/// `Box<dyn Backend>` and shared across threads.
#[async_trait]
pub trait Backend: Send + Sync + 'static {
    /// Stable identifier for diagnostics + status display.
    fn kind(&self) -> BackendKind;

    /// Capability bitmask. Callers should check before invoking methods that
    /// might return `UnsupportedMime` / `UnsupportedAsync`.
    fn capabilities(&self) -> Capabilities;

    /// Write `bytes` to `sel` as `mime`. Sync.
    fn set(&self, sel: Selection, mime: MimeType, bytes: &[u8]) -> Result<(), ClipboardError>;

    /// Read the current contents of `sel` as `mime`. Sync.
    fn get(&self, sel: Selection, mime: MimeType) -> Result<Vec<u8>, ClipboardError>;

    /// Clear `sel`. Sync.
    fn clear(&self, sel: Selection) -> Result<(), ClipboardError>;

    /// Return MIME types currently available in `sel`. Sync.
    fn available(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError>;

    /// Async version of [`set`][Backend::set]. Default returns
    /// [`UnsupportedAsync`][ClipboardError::UnsupportedAsync].
    async fn set_async(
        &self,
        _sel: Selection,
        _mime: MimeType,
        _bytes: Vec<u8>,
    ) -> Result<(), ClipboardError> {
        Err(ClipboardError::UnsupportedAsync)
    }

    /// Async version of [`get`][Backend::get]. Default returns
    /// [`UnsupportedAsync`][ClipboardError::UnsupportedAsync].
    async fn get_async(&self, _sel: Selection, _mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        Err(ClipboardError::UnsupportedAsync)
    }

    /// Async version of [`clear`][Backend::clear]. Default returns
    /// [`UnsupportedAsync`][ClipboardError::UnsupportedAsync].
    async fn clear_async(&self, _sel: Selection) -> Result<(), ClipboardError> {
        Err(ClipboardError::UnsupportedAsync)
    }

    /// Async version of [`available`][Backend::available]. Default returns
    /// [`UnsupportedAsync`][ClipboardError::UnsupportedAsync].
    async fn available_async(&self, _sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        Err(ClipboardError::UnsupportedAsync)
    }
}
