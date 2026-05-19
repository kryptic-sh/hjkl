//! Vim-mode editor engine built on top of [`hjkl_buffer`].
//!
//! Exposes an [`Editor`] that is fully toolkit-agnostic. Covers the bulk
//! of vim's normal / insert / visual / visual-line / visual-block modes,
//! text-object operators, dot-repeat, and ex-command handling
//! (`:s/foo/bar/g`, `:w`, `:q`, `:noh`, ...). Rendering goes through
//! `hjkl_buffer::BufferView`; selection / gutter highlights are painted in
//! the same single-pass as text. TUI/crossterm adapters live in the
//! `hjkl-engine-tui` companion crate.
//!
//! Imported wholesale from sqeel-vim with full git history. The trait
//! extraction (Selection / SelectionSet / Buffer + Host sub-traits) lands
//! progressively under [`crate::types`]. Pre-1.0 churn — the public surface
//! may change in patch bumps. See [docs.rs](https://docs.rs/hjkl-engine) for
//! the canonical API reference.
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
pub mod keymap_motion;
pub mod motions;
mod registers;
pub mod search;
pub mod substitute;
pub mod types;
mod viewport_math;
mod vim;

pub use editor::{Editor, LspIntent, MarkJump, StepBookkeeping};
pub use input::{Input, Key, decode_macro, from_planned as decode_planned_input};
pub use registers::{Registers, Slot};

pub use buffer_impl::{BufferFoldProvider, BufferFoldProviderMut};
pub use keymap_motion::MotionKind;
pub use substitute::{
    SubstError, SubstFlags, SubstituteCmd, SubstituteOutcome, apply_substitute, parse_substitute,
};
pub use types::{
    Attrs, Buffer, BufferEdit, BufferId, Color, ContentEdit, Cursor, CursorShape, DefaultHost,
    Edit, EditorSnapshot, EngineError, FoldOp, FoldProvider, Highlight, HighlightKind, Host,
    Input as PlannedInput, Mode, Modifiers, MouseEvent, MouseKind, NoopFoldProvider, OptionValue,
    Options, Pos, Query, RenderFrame, Search, Selection, SelectionKind, SelectionSet, SnapshotMode,
    SpecialKey, Style, Viewport, WrapMode,
};
pub use vim::{
    InsertDir, InsertEntry, InsertReason, InsertSession, LastChange, LastVisual, Motion, Operator,
    Pending, RangeKind, ScrollDir, SearchPrompt, op_is_change, parse_motion,
};

/// The FSM-internal mode discriminator used by `Editor::fsm_mode()` and
/// `Editor::set_fsm_mode()`. Re-exported as `FsmMode` to avoid clashing with
/// the `types::Mode` buffer-side enum that is already exported as `Mode`.
///
/// Used by `hjkl-vim::normal` and `hjkl-vim::dispatch_input` for mode
/// comparisons.
pub use vim::Mode as FsmMode;

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
