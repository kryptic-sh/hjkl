//! Thin shim — all logic lives in [`hjkl_keymap_tui`].
//!
//! Re-export the public surface so existing `crate::keymap_translate::from_ttyr`
//! and `crate::keymap_translate::to_ttyr` call sites keep working without change.
pub use hjkl_keymap_tui::*;
