//! Engine-level types relocated from `hjkl-engine` so that [`crate::Buffer`]
//! can own per-buffer engine state (undo stack, change log, pending edits,
//! fold ops) without requiring `hjkl-buffer` to depend on `hjkl-engine`.
//!
//! `hjkl-engine` re-exports these via `pub use hjkl_buffer::{...}` so all
//! existing call sites continue to compile without change.

use std::ops::Range;

// ── Pos ───────────────────────────────────────────────────────────────────

/// Grapheme-indexed position. `line` is zero-based row; `col` is zero-based
/// grapheme column within that line.
///
/// Note that `col` counts graphemes, not bytes or chars. Motions and
/// rendering both honor grapheme boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

impl Pos {
    pub const ORIGIN: Pos = Pos { line: 0, col: 0 };

    pub const fn new(line: u32, col: u32) -> Self {
        Pos { line, col }
    }
}

// ── EngineEdit ────────────────────────────────────────────────────────────

/// A pending or applied edit. Multi-cursor edits fan out to `Vec<EngineEdit>`
/// ordered in **reverse byte offset** so each entry's positions remain valid
/// after the prior entry applies.
///
/// Named `EngineEdit` here to avoid collision with [`crate::Edit`] (the
/// buffer-level edit enum). `hjkl-engine` re-exports this as
/// `pub use hjkl_buffer::EngineEdit as Edit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineEdit {
    pub range: Range<Pos>,
    pub replacement: String,
}

impl EngineEdit {
    pub fn insert(at: Pos, text: impl Into<String>) -> Self {
        EngineEdit {
            range: at..at,
            replacement: text.into(),
        }
    }

    pub fn delete(range: Range<Pos>) -> Self {
        EngineEdit {
            range,
            replacement: String::new(),
        }
    }

    pub fn replace(range: Range<Pos>, text: impl Into<String>) -> Self {
        EngineEdit {
            range,
            replacement: text.into(),
        }
    }
}

// ── ContentEdit ───────────────────────────────────────────────────────────

/// Engine-native representation of a single buffer mutation in the
/// shape tree-sitter's `InputEdit` consumes. Emitted by
/// `hjkl_engine::Editor::mutate_edit` and drained by hosts via
/// `hjkl_engine::Editor::take_content_edits` so the syntax layer can fan
/// edits into a retained tree without the engine taking a tree-sitter
/// dependency.
///
/// Positions are `(row, col_byte)` — byte offsets within the row, not
/// char counts. Multi-row inserts/deletes set `new_end_position.0` /
/// `old_end_position.0` to the relevant row delta. Conversion to
/// `tree_sitter::InputEdit` is mechanical (see `apps/hjkl/src/syntax.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_position: (u32, u32),
    pub old_end_position: (u32, u32),
    pub new_end_position: (u32, u32),
}

// ── FoldOp ────────────────────────────────────────────────────────────────

/// A fold operation dispatched by the engine's `z…` keystrokes, `:fold*` ex
/// commands, and the edit-pipeline's "edits inside a fold open it"
/// invalidation.
///
/// `FoldOp` is engine-canonical (per the design doc's resolved
/// question 8.2): hosts don't invent their own fold-op enums. Each
/// host that exposes folds embeds a `FoldOp` variant in its `Intent`
/// enum (or simply observes the engine's pending-fold-op queue via
/// `hjkl_engine::Editor::take_fold_ops`).
///
/// Row indices are zero-based and match the row coordinate space used
/// by [`crate::View`]'s fold methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FoldOp {
    /// `:fold {start,end}` / `zf{motion}` / visual-mode `zf` — register a
    /// new fold spanning `[start_row, end_row]` (inclusive). The `closed`
    /// flag matches the underlying [`crate::Fold::closed`].
    Add {
        start_row: usize,
        end_row: usize,
        closed: bool,
    },
    /// `zd` — drop the fold under `row` if any.
    RemoveAt(usize),
    /// `zo` — open the fold under `row` if any.
    OpenAt(usize),
    /// `zc` — close the fold under `row` if any.
    CloseAt(usize),
    /// `za` — flip the fold under `row` between open / closed.
    ToggleAt(usize),
    /// `zR` — open every fold in the buffer.
    OpenAll,
    /// `zM` — close every fold in the buffer.
    CloseAll,
    /// `zE` — eliminate every fold.
    ClearAll,
    /// Edit-driven fold invalidation. Drops every fold touching the
    /// row range `[start_row, end_row]`. Mirrors vim's "edits inside a
    /// fold open it" behaviour. Fired by the engine's edit pipeline,
    /// not bound to a `z…` keystroke.
    Invalidate { start_row: usize, end_row: usize },
}
