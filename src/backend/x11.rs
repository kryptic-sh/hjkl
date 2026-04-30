//! Linux X11 clipboard backend via libxcb (`dlopen`).
//!
//! Runs a singleton background thread that owns an invisible window, handles
//! `SelectionRequest` events, and auto-`SAVE_TARGETS` after every set.

use crate::{ClipboardError, MimeType, Selection};

use super::Backend;

pub(crate) struct X11Backend;

impl Backend for X11Backend {
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
