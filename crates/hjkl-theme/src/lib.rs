//! Unified theme schema for the hjkl editor stack.
//!
//! Phase 1: TOML parse, palette interning, capture fallback chain.
//! No rendering backends in this phase.

pub mod captures;
mod color;
mod error;
mod palette;
mod style;
pub mod theme;

pub use captures::CaptureMap;
pub use color::Color;
pub use error::ThemeError;
pub use palette::Palette;
pub use style::{Modifiers, StyleSpec, UiStyles};
pub use theme::Theme;
