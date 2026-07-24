//! Direct cell-write `ratatui::widgets::Widget` for [`hjkl_buffer::View`].
//!
//! ## Render path
//!
//! [`BufferView`] implements `ratatui::widgets::Widget`. The widget is
//! **single-pass** — text, selection, gutter signs, and styled spans all paint
//! together. There is no separate `Paragraph` or layout step. Writes one cell
//! at a time so syntax span fg, cursor-line bg, cursor cell REVERSED, and
//! selection bg layer in a single pass without the grapheme / wrap machinery
//! `Paragraph` does.
//!
//! Caller wraps a `&View` in [`BufferView`], hands it the style table
//! that resolves opaque [`hjkl_buffer::Span`] style ids to real ratatui styles
//! via a [`StyleResolver`], and renders into a `ratatui::Frame`.
//!
//! ## StyleResolver hooks
//!
//! The [`StyleResolver`] trait is the host's bridge from opaque `u32` style
//! ids (stored in [`hjkl_buffer::Span::style`]) to real `ratatui::style::Style`
//! values. Implement it against your own theme. A convenience blanket impl
//! exists for closures `Fn(u32) -> Style`.

pub mod render;

pub use render::{
    BufferView, Conceal, DiagOverlay, Gutter, GutterNumbers, SearchRanges, Sign, StyleResolver,
};
