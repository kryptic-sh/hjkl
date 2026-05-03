//! `hjkl-bonsai` — generic tree-sitter syntax highlighting for the hjkl editor stack.
//!
//! Grammars are loaded at runtime via the [`runtime`] module: the loader
//! resolves `<name>.so` from a system / user / cache lookup chain, falling
//! back to a clone + compile-on-demand path. Pair a [`runtime::Grammar`] with
//! a [`Highlighter`] to drive parsing.
//!
//! # Quick start
//!
//! ```no_run
//! use std::sync::Arc;
//! use hjkl_bonsai::{Highlighter, DotFallbackTheme, Theme};
//! use hjkl_bonsai::runtime::{Grammar, GrammarLoader, GrammarRegistry};
//!
//! let registry = GrammarRegistry::embedded()?;
//! let loader = GrammarLoader::user_default()?;
//!
//! let spec = registry.by_name("rust").unwrap();
//! let grammar = Arc::new(Grammar::load("rust", spec, &loader)?);
//! let mut highlighter = Highlighter::new(grammar)?;
//! let spans = highlighter.highlight(b"fn main() {}");
//!
//! let theme = DotFallbackTheme::dark();
//! for span in &spans {
//!     if let Some(_style) = theme.style(span.capture()) {
//!         // apply style to byte_range in your renderer
//!     }
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```

pub mod comment_markers;
pub mod highlighter;
pub mod runtime;
pub mod theme;

// Flat re-exports for the primary public API surface.
pub use comment_markers::{CommentMarkerPass, MarkerWord, default_markers};
pub use highlighter::{HighlightSpan, Highlighter, ParseError, Syntax};
pub use theme::{DotFallbackTheme, Style, Theme};
pub use tree_sitter::{InputEdit, Point};
