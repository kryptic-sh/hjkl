//! Vim-mode editor engine built on top of [`hjkl_buffer`].
//!
//! Exposes an [`Editor`] you can drop into a ratatui layout, a command
//! grammar that covers the bulk of vim's normal / insert / visual /
//! visual-line / visual-block modes, text-object operators, dot-repeat,
//! and ex-command handling (`:s/foo/bar/g`, `:w`, `:q`, `:noh`, ...).
//! Rendering goes through `hjkl_buffer::BufferView`; selection / gutter
//! highlights are painted in the same single-pass as text.
//!
//! Imported wholesale from sqeel-vim with full git history. The trait
//! extraction (Selection / SelectionSet / Buffer + Host sub-traits per
//! [`SPEC.md`][spec]) lands progressively under [`crate::types`]. Pre-1.0
//! churn — the public surface may change in patch bumps.
//!
//! [spec]: https://github.com/kryptic-sh/hjkl/blob/main/crates/hjkl-engine/SPEC.md
//!
//! The legacy public surface is intentionally narrow:
//!
//! - [`Editor`] — the editor widget.
//! - [`KeybindingMode`] / [`VimMode`] — mode enums used by host apps.
//! - [`ex::run`] / [`ex::ExEffect`] — drive ex-mode commands.

mod buffer_impl;
mod editor;
mod input;
pub mod motions;
mod registers;
pub mod types;
mod vim;

pub use editor::{Editor, LspIntent};
pub use input::{Input, Key};
pub use registers::{Registers, Slot};

pub use types::{
    Attrs, Buffer, BufferEdit, BufferId, Color, Cursor, CursorShape, DefaultHost, Edit,
    EditorSnapshot, EngineError, EngineHost, Highlight, HighlightKind, Host, Input as PlannedInput,
    Mode, Modifiers, MouseEvent, MouseKind, OptionValue, Options, Pos, Query, RenderFrame, Search,
    Selection, SelectionKind, SelectionSet, SnapshotMode, SpecialKey, Style, Viewport, WrapMode,
};
pub use vim::SearchPrompt;

// ── Deprecated re-export aliases (slated for removal at 0.1.0) ──────────
//
// 0.0.26 introduced these prefixed names because trait extraction collided
// with the legacy `Edit` value type and the in-crate `Input`/`Viewport`
// shapes. The clashes are resolved at the engine-root surface today, so the
// canonical re-exports drop the prefix. The old names are kept as
// `#[deprecated]` type aliases so consumers pinning `=0.0.30` migrate at
// their own pace; they are deleted at the 0.1.0 cut.

// Trait aliases — `pub use` is the only way to re-export a trait under a
// new name; type aliases reject traits. The `#[deprecated]` attribute
// applies to the alias itself (consumers naming `SpecBuffer` get the
// warning; consumers naming `Buffer` directly do not).

/// Deprecated alias for [`Buffer`]. Renamed in 0.0.31 — the "Spec" prefix
/// was a 0.0.26 stop-gap that is no longer needed.
#[deprecated(since = "0.0.31", note = "renamed to `hjkl_engine::Buffer`")]
pub use Buffer as SpecBuffer;

/// Deprecated alias for [`BufferEdit`]. Renamed in 0.0.31 — the "Spec"
/// prefix was a 0.0.26 stop-gap that is no longer needed.
#[deprecated(since = "0.0.31", note = "renamed to `hjkl_engine::BufferEdit`")]
pub use BufferEdit as SpecBufferEdit;

/// Deprecated alias for [`Edit`]. Renamed in 0.0.31 — the prior `EditOp`
/// disambiguation is no longer needed at this surface.
#[deprecated(since = "0.0.31", note = "renamed to `hjkl_engine::Edit`")]
pub type EditOp = Edit;

/// Deprecated alias for [`Viewport`]. Renamed in 0.0.31 — the "Planned"
/// prefix was a 0.0.26 stop-gap that is no longer needed.
#[deprecated(since = "0.0.31", note = "renamed to `hjkl_engine::Viewport`")]
pub type PlannedViewport = Viewport;

/// Which keyboard discipline the editor uses. Currently vim-only, but
/// kept as an enum so future emacs / plain bindings can slot in without
/// touching the public signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeybindingMode {
    #[default]
    Vim,
}

#[cfg(feature = "serde")]
impl serde::Serialize for KeybindingMode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("vim")
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for KeybindingMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let _ = String::deserialize(d)?;
        Ok(KeybindingMode::Vim)
    }
}

/// Coarse vim-mode a host app can display in its status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VimMode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
    VisualBlock,
}
