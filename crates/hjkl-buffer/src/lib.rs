//! # hjkl-buffer
//!
//! Rope-backed text buffer with vim-shaped semantics: charwise/linewise/
//! blockwise selection, motions matching vim edge cases (no `h` wrap, `$`
//! clamp, sticky col on `j`/`k`), folds, viewport, and search.
//!
//! Extracted from `sqeel-buffer` with full git history. See
//! [MIGRATION.md][plan] for the roadmap and stability contract.
//!
//! ## Features
//!
//! - `ratatui` (off by default): enables the [`render`] module with a direct
//!   cell-write `ratatui::widgets::Widget` impl for [`Buffer`].
//!
//! [plan]: https://github.com/kryptic-sh/hjkl/blob/main/MIGRATION.md

#![deny(unsafe_op_in_unsafe_fn)]

mod buffer;
mod edit;
mod folds;
mod motion;
mod position;
#[cfg(feature = "ratatui")]
mod render;
mod search;
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
pub use render::{BufferView, Gutter, Sign, StyleResolver};
pub use selection::{RowSpan, Selection};
pub use span::Span;
pub use viewport::Viewport;
pub use wrap::Wrap;
