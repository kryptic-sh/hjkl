//! [`WaylandBackend`] — `Backend` impl wrapping the Wayland data-control bg
//! thread.
//!
//! Holds a `&'static WaylandThread` (lazy-initialized via
//! [`wayland_thread::wayland_thread`]) and forwards every method to the
//! corresponding free function or async op.

use async_trait::async_trait;

use crate::{BackendKind, Capabilities, ClipboardError, MimeType, Selection};

use super::{
    Backend,
    wayland_thread::{
        self, WaylandOp, WaylandOpResult, WaylandThread, available_clipboard, clear_clipboard,
        get_clipboard, set_clipboard,
    },
};

/// `Backend` implementation for Linux Wayland sessions.
///
/// Lazy-initializes the bg thread on first construction; subsequent instances
/// share the same `&'static WaylandThread`.
pub struct WaylandBackend {
    thread: &'static WaylandThread,
}

impl WaylandBackend {
    /// Probe the running compositor and bind data-control.
    pub fn new() -> Result<Self, ClipboardError> {
        Ok(Self {
            thread: wayland_thread::wayland_thread()?,
        })
    }
}

#[async_trait]
impl Backend for WaylandBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Wayland
    }

    fn capabilities(&self) -> Capabilities {
        // Wayland data-control supports the full sync + async matrix. PRIMARY
        // is advertised; per-call failure surfaces if the compositor lacks the
        // primary protocol.
        Capabilities::WRITE
            | Capabilities::READ
            | Capabilities::CLEAR
            | Capabilities::AVAILABLE
            | Capabilities::PRIMARY
            | Capabilities::IMAGE
            | Capabilities::RICH_TEXT
            | Capabilities::URI_LIST
            | Capabilities::ASYNC_WRITE
            | Capabilities::ASYNC_READ
            | Capabilities::ASYNC_CLEAR
            | Capabilities::ASYNC_AVAILABLE
    }

    fn set(&self, sel: Selection, mime: MimeType, bytes: &[u8]) -> Result<(), ClipboardError> {
        set_clipboard(self.thread, sel, &mime, bytes)
    }

    fn get(&self, sel: Selection, mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        get_clipboard(self.thread, sel, &mime)
    }

    fn clear(&self, sel: Selection) -> Result<(), ClipboardError> {
        clear_clipboard(self.thread, sel)
    }

    fn available(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        available_clipboard(self.thread, sel)
    }

    async fn set_async(
        &self,
        sel: Selection,
        mime: MimeType,
        bytes: Vec<u8>,
    ) -> Result<(), ClipboardError> {
        let fut = self.thread.send_async(WaylandOp::Set { sel, mime, bytes });
        match fut.await {
            WaylandOpResult::Set(r) => r,
            _ => unreachable!("WaylandOp::Set must produce WaylandOpResult::Set"),
        }
    }

    async fn get_async(&self, sel: Selection, mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        let fut = self.thread.send_async(WaylandOp::Get { sel, mime });
        match fut.await {
            WaylandOpResult::Get(r) => r,
            _ => unreachable!("WaylandOp::Get must produce WaylandOpResult::Get"),
        }
    }

    async fn clear_async(&self, sel: Selection) -> Result<(), ClipboardError> {
        let fut = self.thread.send_async(WaylandOp::Clear { sel });
        match fut.await {
            WaylandOpResult::Clear(r) => r,
            _ => unreachable!("WaylandOp::Clear must produce WaylandOpResult::Clear"),
        }
    }

    async fn available_async(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        let fut = self.thread.send_async(WaylandOp::Available { sel });
        match fut.await {
            WaylandOpResult::Available(r) => r,
            _ => unreachable!("WaylandOp::Available must produce WaylandOpResult::Available"),
        }
    }
}
