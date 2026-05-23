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
//! let loader = GrammarLoader::user_default(registry.meta())?;
//!
//! let spec = registry.by_name("rust").unwrap();
//! let grammar = Arc::new(Grammar::load("rust", spec, &loader, registry.meta())?);
//! let mut highlighter = Highlighter::new(grammar)?;
//! let spans = highlighter.highlight(b"fn main() {}");
//!
//! let theme = DotFallbackTheme::dark();
//! for span in &spans {
//!     if let Some(_spec) = theme.style(span.capture()) {
//!         // apply style to byte_range in your renderer
//!     }
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```

pub mod builtins;
pub mod comment_markers;
pub mod hex_color;
pub mod highlighter;
pub mod predicate;
pub mod query_sanitize;
pub mod runtime;
pub mod theme;

// Flat re-exports for the primary public API surface.
pub use comment_markers::{CommentMarkerPass, MarkerWord, default_markers};
pub use hex_color::{HEX_BG_KEY, HEX_COLOR_CAPTURE, HEX_FG_KEY, HexColorPass};
pub use highlighter::parse_counter;
pub use highlighter::{HighlightSpan, Highlighter, ParseError, Syntax};
pub use predicate::{
    Directive, MatchContext, MatchMetadata, MetaValue, Predicate, PredicateArg, PredicateRegistry,
    directive_fn, predicate_fn,
};
pub use theme::{DotFallbackTheme, StyleSpec, Theme};
pub use tree_sitter::{InputEdit, Point};
