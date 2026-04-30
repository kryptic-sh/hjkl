//! Linux Wayland clipboard backend via libwayland-client (`dlopen`).
//!
//! Probes for `ext_data_control_v1` → `wlr_data_control_v1` in priority order.
//! Falls back to OSC 52 when neither is available (e.g. GNOME without
//! xdg-desktop-portal).

use crate::{ClipboardError, MimeType, Selection};

use super::Backend;

pub(crate) struct WaylandBackend;

impl Backend for WaylandBackend {
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
