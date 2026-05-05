//! # hjkl-buffer
//!
//! Rope-backed text buffer with vim-shaped semantics: charwise/linewise/
//! blockwise selection, motions matching vim edge cases (no `h` wrap, `$`
//! clamp, sticky col on `j`/`k`), folds, viewport, and search.
//!
//! Extracted from `sqeel-buffer` with full git history.
//!
//! ## Features
//!
//! - `ratatui` (off by default): enables the `render` module with a direct
//!   cell-write `ratatui::widgets::Widget` impl for [`Buffer`] via
//!   [`BufferView`].
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
//! [`Buffer`] as the source of truth for text content**. Any divergence
//! between cached state (engine-side selections, undo stacks, search matches)
//! and the buffer's `lines()` is a bug. The invariants documented on each type
//! are the contract that lets the engine cache aggressively without risking
//! that divergence.
//!
//! Open issues: <https://github.com/kryptic-sh/hjkl/issues>.
//!
//! ## Testing your `Buffer` use
//!
//! Property tests are encouraged for any non-trivial caller. The crate ships
//! its own test suite; reuse [`Buffer::from_str`] to construct fixtures from
//! inline strings.
//!
//! Things worth proving:
//!
//! - After any sequence of valid edits + their inverses, the buffer returns to
//!   its original `lines()`.
//! - For any valid [`Position`] and motion call, the resulting cursor is itself
//!   valid.
//! - [`Buffer::dirty_gen`] strictly increases across mutations and stays
//!   constant across read-only queries.

#![deny(unsafe_op_in_unsafe_fn)]

mod buffer;
mod edit;
mod folds;
mod motion;
mod position;
#[cfg(feature = "ratatui")]
mod render;
mod selection;
mod span;
mod viewport;
pub mod wrap;

pub use buffer::Buffer;
pub use edit::{Edit, MotionKind};
pub use folds::Fold;
pub use motion::is_keyword_char;
pub use position::Position;
#[cfg(feature = "ratatui")]
pub use render::{BufferView, Conceal, Gutter, GutterNumbers, Sign, StyleResolver};
pub use selection::{RowSpan, Selection};
pub use span::Span;
pub use viewport::Viewport;
pub use wrap::Wrap;
