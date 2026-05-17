//! Thin shim — all logic lives in [`hjkl_keymap_crossterm`].
//!
//! Re-export the public surface so existing `crate::keymap_translate::from_crossterm`
//! and `crate::keymap_translate::to_crossterm` call sites keep working without change.
pub use hjkl_keymap_crossterm::*;
