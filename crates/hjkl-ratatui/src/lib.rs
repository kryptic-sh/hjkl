//! **Deprecated.** Use [`hjkl-editor-tui`](https://crates.io/crates/hjkl-editor-tui) instead.
//!
//! This crate is a thin re-export shim for `hjkl-editor-tui` and will receive
//! no further functional updates after 0.7.x. Migrate by replacing the dep:
//!
//! ```toml
//! # Before
//! hjkl-ratatui = "0.7"
//!
//! # After
//! hjkl-editor-tui = "0.1"
//! ```
//!
//! And updating `use hjkl_ratatui` → `use hjkl_editor_tui` in your source.
#![forbid(unsafe_code)]
#[allow(unused_imports)]
pub use hjkl_editor_tui::*;
