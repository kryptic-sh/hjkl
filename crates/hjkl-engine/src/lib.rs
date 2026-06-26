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
mod discipline;
mod editor;
pub mod input;
pub mod keymap_motion;
pub mod motions;
mod registers;
pub mod search;
pub mod substitute;
pub mod types;
mod viewport_math;
pub mod vim;

pub use discipline::{DisciplineState, NoDiscipline};
pub use editor::{Editor, LspIntent, MarkJump, UndoGranularity};
pub use input::{Input, Key, decode_macro, from_planned as decode_planned_input};
pub use registers::{Registers, Slot};

pub use buffer_impl::{BufferFoldProvider, BufferFoldProviderMut};
pub use keymap_motion::MotionKind;
pub use substitute::{
    SubstError, SubstFlags, SubstituteCmd, SubstituteMatch, SubstituteOutcome,
    apply_collected_matches, apply_substitute, collect_substitute_matches, parse_substitute,
};
pub use types::{
    Attrs, Buffer, BufferEdit, BufferId, Color, ContentEdit, Cursor, CursorShape, DefaultHost,
    Edit, EditorSnapshot, EngineError, FoldOp, FoldProvider, Highlight, HighlightKind, Host,
    Input as PlannedInput, Mode, Modifiers, MouseEvent, MouseKind, NoopFoldProvider, OptionValue,
    Options, Pos, Query, RenderFrame, Search, Selection, SelectionKind, SelectionSet, SnapshotMode,
    SpecialKey, Style, Viewport, WrapMode,
};
pub use vim::{
    Abbrev, AbbrevTrigger, InsertDir, InsertEntry, InsertReason, InsertSession, LastChange,
    LastVisual, Motion, Operator, Pending, RangeKind, ScrollDir, SearchPrompt, op_is_change,
    parse_motion,
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

/// Which keyboard discipline the editor uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeybindingMode {
    #[default]
    Vim,
    /// Non-modal VSCode-style editing: always in "insert" mode, Ctrl+S saves,
    /// Ctrl+Z/Y undo/redo. Selection/clipboard/find are tracked separately.
    Vscode,
}

impl KeybindingMode {
    /// Parse a config string into a [`KeybindingMode`]. Unrecognised values
    /// fall back to `Vim` (same pattern as `hjkl_icons::IconMode::from_config`).
    pub fn from_config(s: &str) -> Self {
        match s {
            "vscode" => KeybindingMode::Vscode,
            _ => KeybindingMode::Vim,
        }
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for KeybindingMode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            KeybindingMode::Vim => s.serialize_str("vim"),
            KeybindingMode::Vscode => s.serialize_str("vscode"),
        }
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for KeybindingMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(KeybindingMode::from_config(&raw))
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

/// Discipline-agnostic coarse mode for app chrome (status badge, cursor
/// shape) that must work the same whether the active keybinding discipline is
/// vim, vscode, or a future helix/emacs. Unlike [`VimMode`] — which names
/// vim-specific states — `CoarseMode` is the projection every discipline can
/// express: "are we inserting text, selecting, in a command prompt, or idle?"
///
/// This is the seam app chrome reads instead of `VimMode` (epic #265 G3): the
/// vim discipline maps its modes onto these; non-modal disciplines (vscode)
/// project their own state. Today it is derived from [`VimMode`]; once the FSM
/// state is pluggable, each discipline supplies its own projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CoarseMode {
    /// Idle / command-ready (vim Normal).
    #[default]
    Normal,
    /// Text is being inserted at the caret (vim Insert).
    Insert,
    /// A character-wise selection is active (vim Visual).
    Select,
    /// A line-wise selection is active (vim VisualLine).
    SelectLine,
    /// A block / column selection is active (vim VisualBlock).
    SelectBlock,
}

/// A read-only *view* layered over the real input [`VimMode`]. Unlike a vim
/// mode (which decides how keystrokes are interpreted), a `ViewMode` only
/// changes what the buffer presents — input is still interpreted as Normal.
///
/// `Blame` is the git-blame overlay: the editor is read-only and the host
/// renders per-commit framing. It is only meaningful while the input mode is
/// `Normal`; any transition to Insert/Visual/etc. drops it back to `Normal`
/// (see [`Editor::is_blame`]). New read-only overlays (diff, conflict, …)
/// become additional variants here without touching `VimMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Normal,
    Blame,
}

#[cfg(test)]
mod tests {
    use super::KeybindingMode;

    // ── KeybindingMode::from_config ────────────────────────────────────────

    #[test]
    fn from_config_vim_maps_to_vim() {
        assert_eq!(KeybindingMode::from_config("vim"), KeybindingMode::Vim);
    }

    #[test]
    fn from_config_vscode_maps_to_vscode() {
        assert_eq!(
            KeybindingMode::from_config("vscode"),
            KeybindingMode::Vscode
        );
    }

    #[test]
    fn from_config_unknown_falls_back_to_vim() {
        assert_eq!(KeybindingMode::from_config("emacs"), KeybindingMode::Vim);
        assert_eq!(KeybindingMode::from_config(""), KeybindingMode::Vim);
        assert_eq!(KeybindingMode::from_config("VSCode"), KeybindingMode::Vim);
    }

    #[test]
    fn default_is_vim() {
        assert_eq!(KeybindingMode::default(), KeybindingMode::Vim);
    }

    // ── Serde round-trip ───────────────────────────────────────────────────

    #[cfg(feature = "serde")]
    #[test]
    fn serde_vim_round_trip() {
        let json = serde_json::to_string(&KeybindingMode::Vim).unwrap();
        assert_eq!(json, "\"vim\"");
        let back: KeybindingMode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, KeybindingMode::Vim);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_vscode_round_trip() {
        let json = serde_json::to_string(&KeybindingMode::Vscode).unwrap();
        assert_eq!(json, "\"vscode\"");
        let back: KeybindingMode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, KeybindingMode::Vscode);
    }
}
