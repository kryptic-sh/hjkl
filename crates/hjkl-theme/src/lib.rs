//! Unified theme schema for the hjkl editor stack.
//!
//! Phase 1: TOML parse, palette interning, capture fallback chain.
//! No rendering backends in this phase.
//!
//! # Quick start
//!
//! ```rust
//! use hjkl_theme::loader;
//!
//! // Load from a file path:
//! // let theme = loader::load_from_path(std::path::Path::new("my-theme.toml")).unwrap();
//!
//! // Parse from a TOML string:
//! let theme = loader::parse_toml("\"@keyword\" = \"#cba6f7\"").unwrap();
//!
//! // Fall back to the built-in dark theme:
//! let theme = loader::default_theme();
//! ```

pub mod captures;
mod color;
mod error;
pub mod loader;
mod palette;
mod style;
pub mod theme;

pub use captures::CaptureMap;
pub use color::Color;
pub use error::ThemeError;
pub use palette::Palette;
pub use style::{Modifiers, StyleSpec, UiStyles};
pub use theme::Theme;
