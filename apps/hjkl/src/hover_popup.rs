//! Hover popup shim — re-exports from `hjkl_hover` + `hjkl_hover_tui`.
//!
//! All logic lives in the `hjkl-hover` / `hjkl-hover-tui` crates (closes #126 + #15).
//! This module is a thin compatibility adapter so existing `crate::hover_popup::*`
//! call sites compile unchanged.

pub use hjkl_hover::{HoverAnchor, HoverState};

/// Backward-compat alias — new code should use [`HoverState`] directly.
pub type HoverPopup = HoverState;

/// Construct a [`HoverState`] from the legacy `(col, row)` tuple anchor.
///
/// Matches the old `HoverPopup::new(content, (col, row))` signature used by
/// the LSP hover-at-mouse response handler.
pub fn new(content: String, anchor: (u16, u16)) -> HoverState {
    HoverState::new(content, HoverAnchor::new(anchor.0, anchor.1))
}
