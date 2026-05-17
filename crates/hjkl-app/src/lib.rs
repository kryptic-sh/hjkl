//! Host-agnostic application layer for the hjkl editor.
//!
//! Re-exports the modules that both the TUI (apps/hjkl) and GUI
//! (apps/hjkl-gui) binaries share. This crate must stay free of
//! ratatui / crossterm / floem imports — anything UI-flavored
//! belongs in the host binary.

pub mod config;
pub mod editorconfig;
pub mod lang;
