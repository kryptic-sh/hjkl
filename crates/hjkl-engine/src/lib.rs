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
//! extraction (Selection / SelectionSet / View + Host sub-traits) lands
//! progressively under [`crate::types`]. Pre-1.0 churn — the public surface
//! may change in patch bumps. See [docs.rs](https://docs.rs/hjkl-engine) for
//! the canonical API reference.
//!
//! The legacy public surface is intentionally narrow:
//!
//! - [`Editor`] — the editor widget.
//! - [`VimMode`] — mode enum used by host apps.
//! - [`ex::run`] / [`ex::ExEffect`] — drive ex-mode commands.

pub mod abbrev;
pub mod buf_helpers;
mod buffer_impl;
mod discipline;
mod editor;
pub mod input;
pub mod keymap_motion;
pub mod motions;
mod registers;
pub mod rope_util;
pub mod search;
pub mod selection_shift;
pub mod substitute;
pub mod tag;
pub mod types;
mod viewport_math;

pub use discipline::{DisciplineState, NoDiscipline};
pub use editor::{CursorScrollTarget, Editor, LspIntent, MarkJump, Settings, UndoGranularity};
pub use input::{Input, Key, decode_macro, from_planned as decode_planned_input};
pub use registers::{Registers, Slot};
pub use selection_shift::{Sel, shift_position, shift_sel};

pub use buffer_impl::{BufferFoldProvider, BufferFoldProviderMut, SnapshotFoldProvider};
pub use keymap_motion::MotionKind;
pub use substitute::{
    SubstError, SubstFlags, SubstituteCmd, SubstituteMatch, SubstituteOutcome,
    apply_collected_matches, apply_substitute, collect_substitute_matches, parse_substitute,
};
pub use types::{
    Attrs, BufferEdit, BufferId, Color, ContentEdit, Cursor, CursorShape, DefaultHost, Edit,
    EditorSnapshot, EngineError, FoldOp, FoldProvider, Highlight, HighlightKind, Host,
    Input as PlannedInput, Mode, Modifiers, MouseEvent, MouseKind, NoopFoldProvider, OptionValue,
    Options, Pos, Query, RenderFrame, Search, Selection, SelectionKind, SelectionSet, SnapshotMode,
    SpecialKey, Style, View, Viewport, WrapMode,
};
// The vim FSM itself now lives in `hjkl-vim` (#267). What stays here is the
// engine-owned substrate it happens to use — abbreviations, the search prompt,
// scroll/insert directions — plus the shared vocabulary types from
// `hjkl-vim-types`, which both crates name and neither owns.
pub use abbrev::{Abbrev, AbbrevTrigger};
pub use search::SearchPrompt;
pub use tag::matching_tag_pair;
pub use types::{InsertDir, ScrollDir};

pub use hjkl_vim_types::{
    InsertEntry, InsertReason, InsertSession, LastChange, LastVisual, Motion, Operator, Pending,
    RangeKind,
};

/// The FSM-internal mode discriminator used by `Editor::fsm_mode()` and
/// `Editor::set_fsm_mode()`. Re-exported as `FsmMode` to avoid clashing with
/// the `types::Mode` buffer-side enum that is already exported as `Mode`.
///
/// Used by `hjkl-vim::normal` and `hjkl-vim::dispatch_input` for mode
/// comparisons.
pub use hjkl_vim_types::Mode as FsmMode;

// 0.0.32 dropped the `#[deprecated]` re-export aliases introduced at
// 0.0.31 (`SpecBuffer`, `SpecBufferEdit`, `EditOp`, `PlannedViewport`).
// Consumers must use the canonical names: `View`, `BufferEdit`,
// `Edit`, `Viewport`.

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
/// shape). Unlike [`VimMode`] — which names vim-specific states — `CoarseMode`
/// is a minimal projection: "are we inserting text, selecting, or idle?"
///
/// App chrome reads this instead of `VimMode` so it stays behind the engine's
/// discipline seam ([`DisciplineState`]): the installed discipline (vim today)
/// maps its own modes onto these variants via `DisciplineState::coarse_mode`.
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
