//! `hjkl-tree-sitter` — generic tree-sitter syntax highlighting for the hjkl editor stack.
//!
//! # Quick start
//!
//! ```rust
//! use std::path::Path;
//! use hjkl_tree_sitter::{LanguageRegistry, Highlighter, DotFallbackTheme, Theme};
//!
//! let registry = LanguageRegistry::new();
//! let config = registry.by_name("rust").unwrap();
//! let mut highlighter = Highlighter::new(config).unwrap();
//! let spans = highlighter.highlight(b"fn main() {}");
//!
//! let theme = DotFallbackTheme::dark();
//! for span in &spans {
//!     if let Some(style) = theme.style(span.capture()) {
//!         // apply style to byte_range in your renderer
//!         let _ = style;
//!     }
//! }
//! ```

pub mod comment_markers;
pub mod highlighter;
pub mod languages;
pub mod registry;
pub mod theme;

// Flat re-exports for the primary public API surface.
pub use comment_markers::{CommentMarkerPass, MarkerWord, default_markers};
pub use highlighter::{HighlightSpan, Highlighter, ParseError, Syntax};
pub use registry::{LanguageConfig, LanguageRegistry, detect_language_for_path};
pub use theme::{DotFallbackTheme, Style, Theme};
pub use tree_sitter::{InputEdit, Point};
