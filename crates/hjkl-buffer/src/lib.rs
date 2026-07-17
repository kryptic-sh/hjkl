//! # hjkl-buffer
//!
//! Rope-backed text buffer with vim-shaped semantics: charwise/linewise/
//! blockwise selection, motions matching vim edge cases (no `h` wrap, `$`
//! clamp, sticky col on `j`/`k`), folds, viewport, and search.
//!
//! Extracted from `sqeel-buffer` with full git history.
//!
//! ## Pre-1.0 stability
//!
//! Pre-1.0: signatures may shift between patch versions. The invariants
//! documented on each type and function are the load-bearing semantics — they
//! will not silently change without a CHANGELOG entry and a deliberate version
//! bump.
//!
//! ## Why so many invariants?
//!
//! Most of them follow from one rule: **the engine layer treats
//! [`View`] as the source of truth for text content**. Any divergence
//! between cached state (engine-side selections, undo stacks, search matches)
//! and the buffer's `lines()` is a bug. The invariants documented on each type
//! are the contract that lets the engine cache aggressively without risking
//! that divergence.
//!
//! Open issues: <https://github.com/kryptic-sh/hjkl/issues>.
//!
//! ## Testing your `View` use
//!
//! Property tests are encouraged for any non-trivial caller. The crate ships
//! its own test suite; reuse [`View::from_str`] to construct fixtures from
//! inline strings.
//!
//! Things worth proving:
//!
//! - After any sequence of valid edits + their inverses, the buffer returns to
//!   its original `lines()`.
//! - For any valid [`Position`] and motion call, the resulting cursor is itself
//!   valid.
//! - [`View::dirty_gen`] strictly increases across mutations and stays
//!   constant across read-only queries.

#![deny(unsafe_op_in_unsafe_fn)]

mod buffer;
pub mod content;
mod edit;
mod engine_types;
mod folds;
pub mod geom;
pub mod listchars;
mod motion;
mod position;
pub mod search;
mod selection;
mod span;
mod undo;
mod viewport;
pub mod wrap;

pub use buffer::View;
pub use buffer::{rope_line_bytes, rope_line_str};
pub use content::Buffer;
pub use edit::{Edit, MotionKind};
pub use engine_types::{ContentEdit, EngineEdit, FoldOp, Pos};
pub use folds::{Fold, invalidate_folds, shift_fold, shift_folds_after_edit};
pub use geom::{char_col_to_visual_col, visual_col_to_char_col};
pub use listchars::{ListChars, apply_listchars};
pub use motion::is_keyword_char;
pub use position::Position;
pub use search::search_match_ranges;
pub use selection::{RowSpan, Selection};
pub use span::Span;
pub use undo::{MarkSnapshot, UndoEntry};
pub use viewport::{Viewport, is_big_viewport_jump};
pub use wrap::{Wrap, char_col_for_visual_offset, visual_offset_for_char_col, wrap_segments};

/// Stable per-buffer identifier carried through async pipelines
/// (syntax, git-signs, format-worker) so workers can multiplex per-buffer
/// state without holding buffer references.
///
/// Assigned by the application layer; 0 is a valid test sentinel.
///
/// # Example
///
/// ```
/// use hjkl_buffer::BufferId;
/// let id: BufferId = 42;
/// assert_eq!(id, 42);
/// ```
pub type BufferId = u64;
