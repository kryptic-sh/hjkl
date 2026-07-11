//! `hjkl-bonsai` — generic tree-sitter syntax highlighting for the hjkl editor stack.
//!
//! Grammars are loaded at runtime via the [`runtime`] module: the loader
//! resolves `<name>.so` from a system / user / cache lookup chain, falling
//! back to a clone + compile-on-demand path. Pair a [`runtime::Grammar`] with
//! a [`Highlighter`] to drive parsing.
//!
//! # ⚠️ Security: on-demand loading downloads and executes remote code
//!
//! When a grammar is not already present under a system or user directory,
//! the on-demand path (used by [`runtime::GrammarLoader::load`],
//! [`runtime::Grammar::load`], and [`runtime::AsyncGrammarLoader`]) does all
//! of the following **automatically, with the current user's privileges**:
//!
//! 1. **Downloads** the upstream grammar's source by shelling out to `git`
//!    to clone the remote repository named in the manifest (plus the curated
//!    helix / nvim-treesitter query repos).
//! 2. **Compiles** that freshly-downloaded C/C++ source by invoking the
//!    system C/C++ compiler (`$CC` / `$CXX`, else `cc` / `c++`).
//! 3. **Loads and runs** the resulting shared library via `dlopen`
//!    ([`libloading::Library::new`]) and calls into it to parse buffers.
//!
//! Steps 2 and 3 both execute **arbitrary native code** from the downloaded
//! source: a malicious or compromised grammar repo can run anything the C
//! compiler or the loaded `.so` chooses to, in-process. This is inherent to
//! the tree-sitter grammar model (helix, neovim, and every other tree-sitter
//! host work the same way) and is **not sandboxed**.
//!
//! The trust boundary is therefore:
//! - the embedded **manifest** ([`runtime::GrammarRegistry::embedded`]) and
//!   the git remotes / revisions it pins,
//! - the transport security of `git` (use HTTPS/SSH remotes),
//! - the integrity of the local source/artifact caches.
//!
//! To avoid on-demand fetching + compilation entirely, ship pre-built,
//! vetted `.so` + `.scm` pairs under a system directory (see the "Distro
//! packagers" section of the crate README) — system-dir grammars are used
//! as-is and never trigger a clone or compile. Callers that must not fetch
//! untrusted code should resolve only via [`runtime::GrammarLoader::lookup_only`]
//! and treat a miss as "no highlighting" rather than falling back to the
//! build path.
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
pub mod folds;
pub mod hex_color;
pub mod highlighter;
pub mod predicate;
pub mod query_sanitize;
pub mod rainbow;
pub mod runtime;
pub mod theme;

// Flat re-exports for the primary public API surface.
pub use comment_markers::{CommentMarkerPass, MarkerWord, default_markers};
pub use folds::{
    DEFAULT_FOLD_MARKER_CLOSE, DEFAULT_FOLD_MARKER_OPEN, DEFAULT_REGION_MARKER_CLOSE,
    DEFAULT_REGION_MARKER_OPEN, builtin_folds, extract_fold_ranges, extract_fold_ranges_rope,
    extract_marker_fold_ranges_rope, extract_marker_fold_ranges_rope_multi,
};
pub use hex_color::{HEX_BG_KEY, HEX_COLOR_CAPTURE, HEX_FG_KEY, HexColorPass};
pub use highlighter::parse_counter;
pub use highlighter::{HighlightSpan, Highlighter, ParseError, Syntax};
pub use predicate::{
    Directive, MatchContext, MatchMetadata, MetaValue, Predicate, PredicateArg, PredicateRegistry,
    Source, directive_fn, predicate_fn,
};
pub use rainbow::{
    RAINBOW_BRACKET_CAPTURE, RAINBOW_DEPTH_KEY, builtin_rainbows, rainbow_spans, rainbow_spans_rope,
};
pub use theme::{DotFallbackTheme, StyleSpec, Theme};
pub use tree_sitter::{InputEdit, Point};
