//! Windows clipboard backend via Win32 user32/kernel32.
//!
//! Uses `OpenClipboard(NULL)`, `EmptyClipboard`, `GlobalAlloc(GMEM_MOVEABLE)`,
//! and `SetClipboardData` / `GetClipboardData` for all supported formats.

use crate::{ClipboardError, MimeType, Selection};

use super::Backend;

pub(crate) struct WindowsBackend;

impl Backend for WindowsBackend {
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
