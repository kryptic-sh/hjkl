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

mod buf_helpers;
mod buffer_impl;
mod editor;
mod input;
pub mod motions;
mod registers;
pub mod search;
pub mod types;
mod viewport_math;
mod vim;

pub use editor::{Editor, LspIntent};
pub use input::{Input, Key};
pub use registers::{Registers, Slot};

pub use buffer_impl::{BufferFoldProvider, BufferFoldProviderMut};
pub use types::{
    Attrs, Buffer, BufferEdit, BufferId, Color, ContentEdit, Cursor, CursorShape, DefaultHost,
    Edit, EditorSnapshot, EngineError, FoldOp, FoldProvider, Highlight, HighlightKind, Host,
    Input as PlannedInput, Mode, Modifiers, MouseEvent, MouseKind, NoopFoldProvider, OptionValue,
    Options, Pos, Query, RenderFrame, Search, Selection, SelectionKind, SelectionSet, SnapshotMode,
    SpecialKey, Style, Viewport, WrapMode,
};
pub use vim::SearchPrompt;

/// Drive the vim FSM with one [`Input`]. Returns `true` if the engine
/// consumed the keystroke. Hosts that don't pull in the `crossterm`
/// feature reach the FSM through this function (the `crossterm`-gated
/// [`Editor::handle_key`] is a thin wrapper around it).
pub fn step<H: types::Host>(editor: &mut Editor<hjkl_buffer::Buffer, H>, input: Input) -> bool {
    vim::step(editor, input)
}

// 0.0.32 dropped the `#[deprecated]` re-export aliases introduced at
// 0.0.31 (`SpecBuffer`, `SpecBufferEdit`, `EditOp`, `PlannedViewport`).
// Consumers must use the canonical names: `Buffer`, `BufferEdit`,
// `Edit`, `Viewport`.

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
