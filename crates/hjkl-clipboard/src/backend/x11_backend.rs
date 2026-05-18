//! [`X11Backend`] — `Backend` impl wrapping the X11 selection bg thread.
//!
//! Holds a `&'static X11Thread` (lazy-initialized via [`x11_thread::x11_thread`])
//! and forwards every method to the corresponding free function or async op.

use async_trait::async_trait;

use crate::{BackendKind, Capabilities, ClipboardError, MimeType, Selection};

use super::{
    Backend,
    x11_thread::{
        self, X11Op, X11OpResult, X11Thread, atom_to_mime, available_clipboard, clear_clipboard,
        get_clipboard, mime_to_atom_or_name, sel_to_atom, set_clipboard,
    },
};

/// `Backend` implementation for Linux X11 sessions.
///
/// Lazy-initializes the bg thread on first construction; subsequent instances
/// share the same `&'static X11Thread`.
pub struct X11Backend {
    thread: &'static X11Thread,
}

impl X11Backend {
    /// Connect to the X server and intern atoms.
    pub fn new() -> Result<Self, ClipboardError> {
        Ok(Self {
            thread: x11_thread::x11_thread()?,
        })
    }
}

#[async_trait]
impl Backend for X11Backend {
    fn kind(&self) -> BackendKind {
        BackendKind::X11
    }

    fn capabilities(&self) -> Capabilities {
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
        let (mime_atom, mime_name) = mime_to_atom_or_name(&self.thread.atoms, &mime);
        let sel_atom = sel_to_atom(&self.thread.atoms, sel);
        let fut = self.thread.send_async(X11Op::Set {
            sel_atom,
            mime_atom,
            mime_name,
            bytes,
        });
        match fut.await {
            X11OpResult::Set(r) => r,
            _ => unreachable!("X11Op::Set must produce X11OpResult::Set"),
        }
    }

    async fn get_async(&self, sel: Selection, mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        let (mime_atom, mime_name) = mime_to_atom_or_name(&self.thread.atoms, &mime);
        let sel_atom = sel_to_atom(&self.thread.atoms, sel);
        let fut = self.thread.send_async(X11Op::Get {
            sel_atom,
            mime_atom,
            mime_name,
        });
        match fut.await {
            X11OpResult::Get(r) => r,
            _ => unreachable!("X11Op::Get must produce X11OpResult::Get"),
        }
    }

    async fn clear_async(&self, sel: Selection) -> Result<(), ClipboardError> {
        let sel_atom = sel_to_atom(&self.thread.atoms, sel);
        let fut = self.thread.send_async(X11Op::Clear { sel_atom });
        match fut.await {
            X11OpResult::Clear(r) => r,
            _ => unreachable!("X11Op::Clear must produce X11OpResult::Clear"),
        }
    }

    async fn available_async(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        let sel_atom = sel_to_atom(&self.thread.atoms, sel);
        let fut = self.thread.send_async(X11Op::Available { sel_atom });
        match fut.await {
            X11OpResult::Available(r) => {
                let raw_atoms = r?;
                let mut mimes: Vec<MimeType> = Vec::new();
                for atom in raw_atoms {
                    if let Some(mime) = atom_to_mime(&self.thread.atoms, atom)
                        && !mimes.contains(&mime)
                    {
                        mimes.push(mime);
                    }
                }
                Ok(mimes)
            }
            _ => unreachable!("X11Op::Available must produce X11OpResult::Available"),
        }
    }
}
