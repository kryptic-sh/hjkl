//! OSC 52 clipboard backend — write-only SSH/terminal fallback.

use crate::{ClipboardError, MimeType, Selection};

use super::Backend;

pub(crate) struct Osc52Backend;

impl Backend for Osc52Backend {
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
