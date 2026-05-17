//! Host-agnostic application layer for the hjkl editor.
//!
//! Re-exports the modules that both the TUI (apps/hjkl) and GUI
//! (apps/hjkl-gui) binaries share. This crate must stay free of
//! ratatui / crossterm / floem imports — anything UI-flavored
//! belongs in the host binary.

pub mod completion;
pub mod config;
pub mod editorconfig;
pub mod git;
pub mod git_worker;
pub mod keymap_actions;
pub mod lang;
pub mod picker_action;
pub mod picker_sources;

/// Stable per-buffer identifier carried through async pipelines
/// (syntax, git-signs, format-worker) so workers can multiplex per-buffer
/// state without holding buffer references.
pub type BufferId = u64;
