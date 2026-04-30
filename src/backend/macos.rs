//! macOS clipboard backend via NSPasteboard (raw `objc_msgSend`).
//!
//! Links AppKit + Foundation frameworks and libobjc. Calling convention for
//! `objc_msgSend` differs between x86_64 and ARM64 — each call site casts the
//! function pointer to the exact signature required.

use crate::{ClipboardError, MimeType, Selection};

use super::Backend;

pub(crate) struct MacosBackend;

impl Backend for MacosBackend {
    fn set(&self, _sel: Selection, _mime: MimeType, _bytes: &[u8]) -> Result<(), ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    fn get(&self, _sel: Selection, _mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    fn clear(&self, _sel: Selection) -> Result<(), ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }

    fn available(&self, _sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        unimplemented!("phase 0 scaffold")
    }
}
