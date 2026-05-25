//! Parser-agnostic predicate/directive dispatcher.
//!
//! Mirrors the shape of nvim-treesitter's query predicate/directive system
//! without leaking any editor-specific concepts. Consumers register
//! [`Predicate`] and [`Directive`] implementations by name; the
//! [`Highlighter`] calls them during match iteration.
//!
//! [`Highlighter`]: crate::Highlighter

use std::borrow::Cow;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Source — rope-or-bytes text provider
// ---------------------------------------------------------------------------

/// Polymorphic source for a query match. Callers that already hold the full
/// document as a contiguous byte slice use `Bytes`; callers that hold a
/// `ropey::Rope` (the viewport renderer) use `Rope` and avoid materialising
/// the whole document as a `String`.
pub enum Source<'a> {
    /// Contiguous byte slice — zero-copy borrow path.
    Bytes(&'a [u8]),
    /// Rope snapshot — per-capture-slice allocation path (small, bounded by
    /// capture node size, typically a few hundred bytes).
    Rope(&'a ropey::Rope),
}

impl<'a> Source<'a> {
    /// Total byte length of the source.
    pub fn len_bytes(&self) -> usize {
        match self {
            Source::Bytes(b) => b.len(),
            Source::Rope(r) => r.len_bytes(),
        }
    }

    /// Extract the UTF-8 text at `start..end`. Returns `None` when the range
    /// is out-of-bounds or the bytes are not valid UTF-8.
    pub fn text_slice(&self, start: usize, end: usize) -> Option<Cow<'a, str>> {
        match self {
            Source::Bytes(b) => {
                if end > b.len() || start > end {
                    return None;
                }
                std::str::from_utf8(&b[start..end]).ok().map(Cow::Borrowed)
            }
            Source::Rope(r) => {
                if end > r.len_bytes() || start > end {
                    return None;
                }
                Some(Cow::Owned(r.byte_slice(start..end).to_string()))
            }
        }
    }

    /// Extract the raw bytes at `start..end`. Returns `None` when
    /// out-of-bounds.
    pub fn bytes_slice(&self, start: usize, end: usize) -> Option<Cow<'a, [u8]>> {
        match self {
            Source::Bytes(b) => {
                if end > b.len() || start > end {
                    return None;
                }
                Some(Cow::Borrowed(&b[start..end]))
            }
            Source::Rope(r) => {
                if end > r.len_bytes() || start > end {
                    return None;
                }
                let s = r.byte_slice(start..end).to_string();
                Some(Cow::Owned(s.into_bytes()))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Value types
// ---------------------------------------------------------------------------

/// Typed value stored in [`MatchMetadata`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaValue {
    Str(String),
    Int(i64),
    Bool(bool),
}

/// Agnostic metadata bag attached to each query match.
///
/// - `per_capture` — keyed by tree-sitter capture index, then by string key.
///   Written by directives like `#set! @cap key val`.
/// - `pattern` — pattern-level metadata keyed by string key.
///   Written by directives like `#set! "key" val` / `#set! key val`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatchMetadata {
    pub per_capture: HashMap<u32, HashMap<String, MetaValue>>,
    pub pattern: HashMap<String, MetaValue>,
}

impl MatchMetadata {
    /// Return per-capture metadata for `capture_idx`, if any.
    pub fn capture(&self, capture_idx: u32) -> Option<&HashMap<String, MetaValue>> {
        self.per_capture.get(&capture_idx)
    }

    /// Mutably access per-capture metadata for `capture_idx`, creating on demand.
    pub fn capture_mut(&mut self, capture_idx: u32) -> &mut HashMap<String, MetaValue> {
        self.per_capture.entry(capture_idx).or_default()
    }
}

// ---------------------------------------------------------------------------
// MatchContext
// ---------------------------------------------------------------------------

/// Argument to a predicate or directive, as resolved from the raw predicate
/// step stream.
#[derive(Debug, Clone)]
pub enum PredicateArg<'a> {
    /// A capture index referring to a node in the match.
    Capture(u32),
    /// A raw string literal from the query.
    Str(&'a str),
}

/// Read-only view into a single [`tree_sitter::QueryMatch`], presented to
/// predicate and directive implementations.
pub struct MatchContext<'a> {
    /// Index of the pattern that produced this match.
    pub pattern_index: usize,
    /// All captures in this match: `(capture_index, Node)` pairs.
    pub captures: &'a [(u32, tree_sitter::Node<'a>)],
    /// Raw source bytes — kept for API compatibility with existing predicate
    /// impls and tests that construct `MatchContext` directly with `source`.
    /// New code should access text via [`MatchContext::capture_text`] which
    /// reads from `src` when available, falling back to `source`.
    pub source: &'a [u8],
    /// Polymorphic source. When `None` the legacy `source: &[u8]` field is
    /// used instead, keeping all existing tests and call sites working without
    /// changes.
    pub src: Option<&'a Source<'a>>,
    /// Arguments from the predicate/directive step (excluding the operator name).
    pub args: &'a [PredicateArg<'a>],
    /// All capture names from the compiled query (indexed by capture index).
    pub capture_names: &'a [String],
}

impl<'a> MatchContext<'a> {
    /// Return the UTF-8 text of the first node that has the given capture
    /// index, or `None` if the capture is absent or the slice is not valid
    /// UTF-8.
    ///
    /// When the context was built with a `Source::Rope`, this returns
    /// `Cow::Owned`; otherwise `Cow::Borrowed` (zero-copy from the `&[u8]`
    /// slice).
    pub fn capture_text(&self, capture_idx: u32) -> Option<Cow<'_, str>> {
        let node = self.first_capture(capture_idx)?;
        let start = node.start_byte();
        let end = node.end_byte();
        if let Some(src) = self.src {
            src.text_slice(start, end)
        } else {
            if end > self.source.len() || start > end {
                return None;
            }
            std::str::from_utf8(&self.source[start..end])
                .ok()
                .map(Cow::Borrowed)
        }
    }

    /// Return the first [`tree_sitter::Node`] that has the given capture index.
    pub fn first_capture(&self, capture_idx: u32) -> Option<tree_sitter::Node<'a>> {
        self.captures
            .iter()
            .find(|(idx, _)| *idx == capture_idx)
            .map(|(_, node)| *node)
    }
}

// ---------------------------------------------------------------------------
// Traits
// ---------------------------------------------------------------------------

/// A named boolean filter applied to each query match.
///
/// Return `false` to cause the containing match to be skipped entirely.
pub trait Predicate: Send + Sync + std::fmt::Debug {
    /// Name this predicate is registered under, e.g. `"contains?"`.
    fn name(&self) -> &str;

    /// Evaluate against the current match context. `true` = keep match.
    fn eval(&self, ctx: &MatchContext<'_>) -> bool;
}

/// A named side-effecting action applied to each query match.
///
/// Directives mutate [`MatchMetadata`] but cannot veto a match.
pub trait Directive: Send + Sync + std::fmt::Debug {
    /// Name this directive is registered under, e.g. `"set!"`.
    fn name(&self) -> &str;

    /// Apply against the current match context, writing into `meta`.
    fn apply(&self, ctx: &MatchContext<'_>, meta: &mut MatchMetadata);
}

// ---------------------------------------------------------------------------
// Closure sugar
// ---------------------------------------------------------------------------

/// Wrap a closure as a [`Predicate`] without defining a named struct.
///
/// ```ignore
/// registry.register_predicate(predicate_fn("my-check?", |ctx| {
///     ctx.capture_text(0).map_or(false, |t| t.starts_with("_"))
/// }));
/// ```
pub fn predicate_fn<F>(name: &'static str, f: F) -> Box<dyn Predicate>
where
    F: Fn(&MatchContext<'_>) -> bool + Send + Sync + 'static,
{
    struct ClosurePredicate<F> {
        name: &'static str,
        f: F,
    }
    impl<F> std::fmt::Debug for ClosurePredicate<F> {
        fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(fmt, "ClosurePredicate({})", self.name)
        }
    }
    impl<F: Fn(&MatchContext<'_>) -> bool + Send + Sync> Predicate for ClosurePredicate<F> {
        fn name(&self) -> &str {
            self.name
        }
        fn eval(&self, ctx: &MatchContext<'_>) -> bool {
            (self.f)(ctx)
        }
    }
    Box::new(ClosurePredicate { name, f })
}

/// Wrap a closure as a [`Directive`] without defining a named struct.
pub fn directive_fn<F>(name: &'static str, f: F) -> Box<dyn Directive>
where
    F: Fn(&MatchContext<'_>, &mut MatchMetadata) + Send + Sync + 'static,
{
    struct ClosureDirective<F> {
        name: &'static str,
        f: F,
    }
    impl<F> std::fmt::Debug for ClosureDirective<F> {
        fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(fmt, "ClosureDirective({})", self.name)
        }
    }
    impl<F: Fn(&MatchContext<'_>, &mut MatchMetadata) + Send + Sync> Directive for ClosureDirective<F> {
        fn name(&self) -> &str {
            self.name
        }
        fn apply(&self, ctx: &MatchContext<'_>, meta: &mut MatchMetadata) {
            (self.f)(ctx, meta);
        }
    }
    Box::new(ClosureDirective { name, f })
}

// ---------------------------------------------------------------------------
// PredicateRegistry
// ---------------------------------------------------------------------------

/// Registry of named [`Predicate`] and [`Directive`] implementations.
///
/// Build with [`PredicateRegistry::with_builtins`] for the default set, or
/// start from [`PredicateRegistry::new`] for a blank slate.
#[derive(Default)]
pub struct PredicateRegistry {
    predicates: HashMap<String, Box<dyn Predicate>>,
    directives: HashMap<String, Box<dyn Directive>>,
}

impl PredicateRegistry {
    /// Empty registry — no predicates or directives registered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry pre-populated with all builtins from [`crate::builtins`].
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        crate::builtins::register_builtins(&mut r);
        r
    }

    /// Register a predicate. Returns `&mut Self` for chaining.
    pub fn register_predicate(&mut self, p: Box<dyn Predicate>) -> &mut Self {
        self.predicates.insert(p.name().to_string(), p);
        self
    }

    /// Register a directive. Returns `&mut Self` for chaining.
    pub fn register_directive(&mut self, d: Box<dyn Directive>) -> &mut Self {
        self.directives.insert(d.name().to_string(), d);
        self
    }

    /// Look up a predicate by name.
    pub fn get_predicate(&self, name: &str) -> Option<&dyn Predicate> {
        self.predicates.get(name).map(|p| p.as_ref())
    }

    /// Look up a directive by name.
    pub fn get_directive(&self, name: &str) -> Option<&dyn Directive> {
        self.directives.get(name).map(|d| d.as_ref())
    }
}
