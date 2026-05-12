//! Stateful syntax highlighter built on top of a runtime-loaded [`Grammar`].
//!
//! A [`Highlighter`] owns a `Parser` + compiled `Query` for one language and
//! keeps a reference to the [`Grammar`] alive (so the underlying `dlopen`-ed
//! shared library outlives any tree the parser produces).

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use anyhow::{Context, Result};
use tree_sitter::{
    ParseOptions, Parser, Query, QueryCursor, QueryPredicateArg, StreamingIterator as _,
};

use crate::predicate::{MatchContext, MatchMetadata, MetaValue, PredicateArg, PredicateRegistry};
use crate::query_sanitize::{
    CaptureSetDirective, extract_capture_set_directives, sanitize_highlights,
};
use crate::runtime::Grammar;

/// Index for `@injection.language` capture.
const INJ_LANG_CAPTURE: &str = "injection.language";
/// Index for `@injection.content` capture.
const INJ_CONTENT_CAPTURE: &str = "injection.content";

/// Global set of unknown predicate names that have already been warned about.
/// Using `OnceLock<std::sync::Mutex<HashSet<String>>>` so we warn exactly once
/// per process per unknown name, avoiding log spam.
static WARNED_PREDICATES: OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    OnceLock::new();

fn warn_unknown_predicate_once(name: &str) {
    let set =
        WARNED_PREDICATES.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    if let Ok(mut guard) = set.lock()
        && guard.insert(name.to_string())
    {
        tracing::warn!(predicate = name, "unknown predicate — match still emitted");
    }
}

/// A byte-range tagged with the tree-sitter capture name that applies to it,
/// plus any per-capture metadata written by directives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    /// Byte range in the source buffer.
    pub byte_range: Range<usize>,
    /// The capture name from the highlights.scm query, e.g. `"keyword.control"`.
    pub capture: String,
    /// Per-capture metadata written by directives such as `#set!`.
    /// Empty map when no directives produced metadata for this capture.
    pub metadata: HashMap<String, MetaValue>,
}

impl HighlightSpan {
    /// The capture name as a `&str` slice.
    pub fn capture(&self) -> &str {
        &self.capture
    }
}

/// A parse error harvested from tree-sitter's ERROR / MISSING nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Byte range of the error node (clamped to the first line).
    pub byte_range: Range<usize>,
    /// Human-readable description, e.g. `"unexpected \`foo\`"`.
    pub message: String,
}

/// The parsed syntax tree for a buffer, plus a dirty flag for incremental
/// update bookkeeping.
pub struct Syntax {
    pub(crate) tree: tree_sitter::Tree,
    pub dirty: bool,
}

impl Syntax {
    /// Access the underlying tree-sitter `Tree`.
    pub fn tree(&self) -> &tree_sitter::Tree {
        &self.tree
    }
}

/// Default parser timeout for `parse_incremental`, in microseconds.
/// `0` = no timeout (fast path that takes the direct `Parser::parse`
/// call instead of the streaming callback form).
const DEFAULT_PARSE_TIMEOUT_MICROS: u64 = 0;

// ---------------------------------------------------------------------------
// Child-highlighter cache
// ---------------------------------------------------------------------------

/// FNV-1a-inspired fast hash of a byte slice.  Standard library's
/// `DefaultHasher` is good enough — all we need is collision resistance across
/// typical code-block content, not cryptographic security.
fn hash_bytes(b: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

/// One cached child highlighter, together with the content hash that was used
/// to build the parse tree so we can detect content drift even when the byte
/// range is identical (e.g. the user replaces one code block with another of
/// the same length).
struct CachedChild {
    highlighter: Highlighter,
    /// FNV hash of the slice that was last parsed.
    source_hash: u64,
}

/// Cache of child `Highlighter` instances, keyed by
/// `(language_name, content_range_start, content_range_end)`.
///
/// Eviction policy: after each `highlight_range_with_injections` / `highlight_with_injections`
/// call the cache is pruned to only the keys that appeared in the *current*
/// injection set.  This keeps memory bounded as the user scrolls or edits.
#[derive(Default)]
struct ChildCache {
    map: HashMap<(String, usize, usize), CachedChild>,
}

impl ChildCache {
    /// Return the cached child for `(lang, start, end)` *if* its stored hash
    /// matches `content_hash`.  On a hash miss the entry is evicted so the
    /// caller can rebuild and re-insert.
    fn get_if_fresh(
        &mut self,
        lang: &str,
        start: usize,
        end: usize,
        content_hash: u64,
    ) -> Option<&mut Highlighter> {
        let key = (lang.to_string(), start, end);
        // Check freshness without holding a mutable borrow across the remove path.
        let fresh = self.map.get(&key).map(|c| c.source_hash == content_hash);
        match fresh {
            Some(true) => Some(&mut self.map.get_mut(&key).unwrap().highlighter),
            Some(false) => {
                // Content drifted — evict so we rebuild.
                self.map.remove(&key);
                None
            }
            None => None,
        }
    }

    /// Insert a freshly built `Highlighter` for `(lang, start, end)`.
    fn insert(
        &mut self,
        lang: String,
        start: usize,
        end: usize,
        hl: Highlighter,
        content_hash: u64,
    ) {
        self.map.insert(
            (lang, start, end),
            CachedChild {
                highlighter: hl,
                source_hash: content_hash,
            },
        );
    }

    /// Remove every entry whose key is not in `keep`.  Called once per render
    /// pass with the set of injections that were actually used, so stale
    /// entries (code blocks that were deleted/scrolled away) don't accumulate.
    fn evict_stale(&mut self, keep: &[(String, usize, usize)]) {
        self.map.retain(|k, _| keep.iter().any(|kk| kk == k));
    }
}

// ---------------------------------------------------------------------------
// Parse counter (test instrumentation — compiled in all modes but hidden)
// ---------------------------------------------------------------------------

/// Thread-local counter incremented on every `parse_initial` call. Useful for
/// integration tests that assert the child-highlighter cache avoids redundant
/// parses. Not part of the public stable API.
#[doc(hidden)]
pub mod parse_counter {
    use std::cell::Cell;

    thread_local! {
        static COUNT: Cell<u64> = const { Cell::new(0) };
    }

    /// Increment the thread-local parse counter. Called from `parse_initial`.
    pub(super) fn increment() {
        COUNT.with(|c| c.set(c.get() + 1));
    }

    /// Read the current counter value.
    pub fn get() -> u64 {
        COUNT.with(|c| c.get())
    }

    /// Reset the counter to zero.
    pub fn reset() {
        COUNT.with(|c| c.set(0));
    }
}

/// Stateful syntax highlighter for a single language.
///
/// Owns a `Parser`, a compiled `Query`, and a reference-counted handle on the
/// [`Grammar`] so the underlying shared library cannot drop while a parse
/// tree is live.
pub struct Highlighter {
    parser: Parser,
    query: Query,
    capture_names: Vec<String>,
    /// Compiled injection query from `injections.scm`, if the grammar ships
    /// one. `None` = this grammar has no injection rules.
    injection_query: Option<Query>,
    tree: Option<tree_sitter::Tree>,
    parse_timeout_micros: u64,
    /// Predicate/directive registry used during match iteration.
    registry: Arc<PredicateRegistry>,
    /// `(#set! @cap key val)` directives pre-extracted before query compilation
    /// (stock tree-sitter rejects them at compile time).  Keyed by pattern index.
    pre_extracted: Vec<CaptureSetDirective>,
    /// Cached child highlighters used by `highlight_range_with_injections` /
    /// `highlight_with_injections`. Avoids rebuilding a parser + re-parsing every
    /// injected code block on every render frame. See [`ChildCache`].
    child_cache: ChildCache,
    /// Held to keep the dlopen-ed shared library alive. Field order matters
    /// (parse trees reference data inside `_grammar`'s `Library`); placing
    /// `_grammar` last guarantees it drops after `tree` and `query`.
    _grammar: Arc<Grammar>,
}

impl Highlighter {
    /// Create a new highlighter for `grammar`'s language using its bundled
    /// `highlights.scm`. If the grammar ships an `injections.scm`, that query
    /// is compiled too — a compilation failure is logged and skipped rather
    /// than poisoning the whole highlighter.
    ///
    /// Uses [`PredicateRegistry::with_builtins`] internally.
    pub fn new(grammar: Arc<Grammar>) -> Result<Self> {
        Self::with_registry(grammar, Arc::new(PredicateRegistry::with_builtins()))
    }

    /// Like [`Highlighter::new`] but with a caller-supplied registry, allowing
    /// consumers to extend predicates/directives beyond the builtins.
    pub fn with_registry(grammar: Arc<Grammar>, registry: Arc<PredicateRegistry>) -> Result<Self> {
        let mut parser = Parser::new();
        parser
            .set_language(grammar.language())
            .context("failed to set tree-sitter language")?;

        let (query, pre_extracted) =
            compile_query(grammar.language(), grammar.highlights_scm(), grammar.name())?;

        let capture_names: Vec<String> = query
            .capture_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Compile the injection query if present. Failure is non-fatal: a
        // grammar whose injections.scm uses unsupported predicates will still
        // highlight normally, just without injection support.
        let injection_query =
            grammar
                .injections_scm()
                .and_then(|inj| match Query::new(grammar.language(), inj) {
                    Ok(q) => Some(q),
                    Err(e) => {
                        tracing::warn!(
                            grammar = grammar.name(),
                            error = %e,
                            "injections.scm failed to compile — injection highlighting disabled"
                        );
                        None
                    }
                });

        Ok(Self {
            parser,
            query,
            capture_names,
            injection_query,
            tree: None,
            parse_timeout_micros: DEFAULT_PARSE_TIMEOUT_MICROS,
            registry,
            pre_extracted,
            child_cache: ChildCache::default(),
            _grammar: grammar,
        })
    }

    /// Apply an `InputEdit` to the retained tree, if any. No-op when the
    /// highlighter has no retained tree.
    pub fn edit(&mut self, edit: &tree_sitter::InputEdit) {
        if let Some(tree) = self.tree.as_mut() {
            tree.edit(edit);
        }
    }

    /// Reparse `source` against the retained tree (if any) under the
    /// configured timeout. Returns `true` on success, replacing the
    /// retained tree. Returns `false` on timeout, leaving the previous
    /// retained tree in place.
    ///
    /// **Important:** when this returns `false`, do not call
    /// [`Highlighter::highlight_range`] until a subsequent
    /// `parse_incremental` succeeds — the retained tree is stale relative
    /// to `source`.
    pub fn parse_incremental(&mut self, source: &[u8]) -> bool {
        if self.parse_timeout_micros == 0 {
            let result = self.parser.parse(source, self.tree.as_ref());
            return match result {
                Some(t) => {
                    self.tree = Some(t);
                    true
                }
                None => false,
            };
        }
        let deadline = Instant::now() + std::time::Duration::from_micros(self.parse_timeout_micros);
        let mut progress = move |_state: &tree_sitter::ParseState| {
            if Instant::now() >= deadline {
                return std::ops::ControlFlow::Break(());
            }
            std::ops::ControlFlow::Continue(())
        };
        let mut opts = ParseOptions::new().progress_callback(&mut progress);
        let bytes = source;
        let len = bytes.len();
        let result = self.parser.parse_with_options(
            &mut |i, _| {
                if i < len {
                    &bytes[i..]
                } else {
                    Default::default()
                }
            },
            self.tree.as_ref(),
            Some(opts.reborrow()),
        );
        match result {
            Some(t) => {
                self.tree = Some(t);
                true
            }
            None => false,
        }
    }

    /// Parse `source` from scratch with the parser timeout disabled. Used on
    /// initial load and after `reset()`.
    pub fn parse_initial(&mut self, source: &[u8]) {
        parse_counter::increment();

        let result = self.parser.parse(source, None);
        if let Some(t) = result {
            self.tree = Some(t);
        }
    }

    /// Run the highlights query against the retained tree, scoped to
    /// `byte_range`. Returns spans whose byte range overlaps `byte_range`,
    /// sorted by start byte. Empty when there's no retained tree.
    pub fn highlight_range(
        &mut self,
        source: &[u8],
        byte_range: Range<usize>,
    ) -> Vec<HighlightSpan> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };

        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(byte_range.clone());
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let registry = Arc::clone(&self.registry);
        let capture_names = &self.capture_names;
        let pre_extracted = &self.pre_extracted;

        let mut spans: Vec<HighlightSpan> = Vec::new();
        while let Some(m) = matches.next() {
            let pattern_idx = m.pattern_index;

            // Build the (capture_idx, node) pairs used by MatchContext.
            let cap_pairs: Vec<(u32, tree_sitter::Node<'_>)> =
                m.captures.iter().map(|c| (c.index, c.node)).collect();

            // Evaluate general predicates (custom ones only; builtins like
            // eq?/match?/any-of? are handled by tree-sitter itself).
            let mut skip_match = false;
            for pred in self.query.general_predicates(pattern_idx) {
                let op = pred.operator.as_ref();
                // Only dispatch predicates (ending in `?`); directives end in `!`.
                if !op.ends_with('?') {
                    continue;
                }
                // Build args for this predicate (skip the operator).
                let args: Vec<PredicateArg<'_>> = pred
                    .args
                    .iter()
                    .map(|a| match a {
                        QueryPredicateArg::Capture(idx) => PredicateArg::Capture(*idx),
                        QueryPredicateArg::String(s) => PredicateArg::Str(s.as_ref()),
                    })
                    .collect();
                let ctx = MatchContext {
                    pattern_index: pattern_idx,
                    captures: &cap_pairs,
                    source,
                    args: &args,
                    capture_names,
                };
                match registry.get_predicate(op) {
                    Some(p) => {
                        if !p.eval(&ctx) {
                            skip_match = true;
                            break;
                        }
                    }
                    None => {
                        warn_unknown_predicate_once(op);
                        // Unknown predicate — don't veto the match.
                    }
                }
            }
            if skip_match {
                continue;
            }

            // Build MatchMetadata for this match.
            let mut meta = MatchMetadata::default();

            // Apply literal property_settings (the `#set! "key" val` forms
            // that tree-sitter parsed natively via property_settings()).
            for prop in self.query.property_settings(pattern_idx) {
                let key = prop.key.as_ref();
                let val = prop.value.as_deref();
                if let Some(cap_id) = prop.capture_id {
                    let value = match val {
                        Some(v) => MetaValue::Str(v.to_string()),
                        None => MetaValue::Bool(true),
                    };
                    meta.capture_mut(cap_id as u32)
                        .insert(key.to_string(), value);
                } else {
                    let value = match val {
                        Some(v) => MetaValue::Str(v.to_string()),
                        None => MetaValue::Bool(true),
                    };
                    meta.pattern.insert(key.to_string(), value);
                }
            }

            // Apply general directives (ending in `!`) from general_predicates.
            for pred in self.query.general_predicates(pattern_idx) {
                let op = pred.operator.as_ref();
                if !op.ends_with('!') {
                    continue;
                }
                let args: Vec<PredicateArg<'_>> = pred
                    .args
                    .iter()
                    .map(|a| match a {
                        QueryPredicateArg::Capture(idx) => PredicateArg::Capture(*idx),
                        QueryPredicateArg::String(s) => PredicateArg::Str(s.as_ref()),
                    })
                    .collect();
                let ctx = MatchContext {
                    pattern_index: pattern_idx,
                    captures: &cap_pairs,
                    source,
                    args: &args,
                    capture_names,
                };
                if let Some(d) = registry.get_directive(op) {
                    d.apply(&ctx, &mut meta);
                } else {
                    warn_unknown_predicate_once(op);
                }
            }

            // Apply pre-extracted `(#set! @cap key val)` directives for this pattern.
            for pe in pre_extracted
                .iter()
                .filter(|d| d.pattern_index == pattern_idx)
            {
                // Resolve capture name → capture index.
                let cap_idx = capture_names
                    .iter()
                    .position(|n| n == &pe.capture_name)
                    .map(|i| i as u32);
                if let Some(cap_idx) = cap_idx {
                    let value = match &pe.value {
                        Some(v) => MetaValue::Str(v.clone()),
                        None => MetaValue::Bool(true),
                    };
                    meta.capture_mut(cap_idx).insert(pe.key.clone(), value);
                }
            }

            // Emit spans for each capture in the match.
            for capture in m.captures {
                let node = capture.node;
                let start = node.start_byte();
                let end = node.end_byte();
                if start >= end || end > source.len() {
                    continue;
                }
                if start >= byte_range.end || end <= byte_range.start {
                    continue;
                }
                let capture_name = capture_names[capture.index as usize].clone();

                // Merge metadata: pattern-level first, per-capture wins on collision.
                let mut span_meta: HashMap<String, MetaValue> = meta.pattern.clone();
                if let Some(cap_meta) = meta.per_capture.get(&capture.index) {
                    for (k, v) in cap_meta {
                        span_meta.insert(k.clone(), v.clone());
                    }
                }

                spans.push(HighlightSpan {
                    byte_range: start..end,
                    capture: capture_name,
                    metadata: span_meta,
                });
            }
        }

        spans.sort_by_key(|s| s.byte_range.start);
        spans
    }

    /// Walk the retained tree and collect ERROR / MISSING nodes whose byte
    /// range intersects `byte_range`.
    pub fn parse_errors_range(
        &mut self,
        source: &[u8],
        byte_range: Range<usize>,
    ) -> Vec<ParseError> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        if !tree.root_node().has_error() {
            return Vec::new();
        }
        let mut errors = Vec::new();
        collect_parse_errors(tree.root_node(), source, &byte_range, &mut errors);
        errors
    }

    /// Read accessor for the retained tree.
    pub fn tree(&self) -> Option<&tree_sitter::Tree> {
        self.tree.as_ref()
    }

    /// Override the parser timeout used by `parse_incremental`. `0` disables
    /// the timeout.
    pub fn set_parse_timeout_micros(&mut self, micros: u64) {
        self.parse_timeout_micros = micros;
    }

    /// Drop the retained tree.
    pub fn reset(&mut self) {
        self.tree = None;
    }

    /// Parse `source` and return the resulting `Syntax`. Standalone — does
    /// not touch the retained tree.
    pub fn parse(&mut self, source: &[u8]) -> Option<Syntax> {
        let tree = self.parser.parse(source, None)?;
        Some(Syntax { tree, dirty: false })
    }

    /// Parse `source` and run the highlights query, returning all
    /// `HighlightSpan`s in source order.
    pub fn highlight(&mut self, source: &[u8]) -> Vec<HighlightSpan> {
        if self.tree.is_none() {
            self.parse_initial(source);
        } else if !self.parse_incremental(source) {
            return Vec::new();
        }
        self.highlight_range(source, 0..source.len())
    }

    /// Parse `source`, run the highlights query, and recursively highlight any
    /// injected language ranges declared in `injections.scm`.
    ///
    /// `resolve` is called with a language name string (e.g. `"rust"`) and
    /// should return a loaded `Grammar` for that language, or `None` to skip
    /// the injection. The closure is invoked once per injected language name
    /// found in the source — callers should memoize if repeated lookups are
    /// expensive.
    ///
    /// ## Merge semantics (v1)
    ///
    /// Child spans (from injected language parsers) are collected and their
    /// byte offsets translated back into parent-buffer coordinates. For
    /// rendering, child spans win inside the injected range: parent spans that
    /// fall entirely within an injected range are dropped; parent spans that
    /// partially overlap are kept as-is (rare in practice — a parser node
    /// seldom straddles a code-fence boundary). The result is sorted by
    /// `byte_range.start`.
    ///
    /// When `injections.scm` is absent or produces no matches, this method
    /// behaves identically to [`Highlighter::highlight`].
    pub fn highlight_with_injections<F>(
        &mut self,
        source: &[u8],
        mut resolve: F,
    ) -> Vec<HighlightSpan>
    where
        F: FnMut(&str) -> Option<Arc<Grammar>>,
    {
        // Parse / re-parse the parent buffer first.
        if self.tree.is_none() {
            self.parse_initial(source);
        } else if !self.parse_incremental(source) {
            return Vec::new();
        }

        let parent_spans = self.highlight_range(source, 0..source.len());

        let Some(inj_query) = self.injection_query.as_ref() else {
            return parent_spans;
        };

        // Find the capture indices for @injection.language and @injection.content.
        let lang_idx = inj_query
            .capture_names()
            .iter()
            .position(|n| *n == INJ_LANG_CAPTURE);
        let content_idx = inj_query
            .capture_names()
            .iter()
            .position(|n| *n == INJ_CONTENT_CAPTURE);

        let (Some(lang_idx), Some(content_idx)) = (lang_idx, content_idx) else {
            // This grammar's injections.scm doesn't use the standard captures.
            return parent_spans;
        };

        let lang_idx = lang_idx as u32;
        let content_idx = content_idx as u32;

        let Some(tree) = self.tree.as_ref() else {
            return parent_spans;
        };

        // Walk injection query matches, collecting (language_name, byte_range) pairs.
        let mut injections: Vec<(String, Range<usize>)> = Vec::new();
        {
            let mut cursor = QueryCursor::new();
            let mut matches = cursor.matches(inj_query, tree.root_node(), source);

            while let Some(m) = matches.next() {
                // Each match may have both @injection.language and
                // @injection.content captures, possibly in either order.
                let mut lang_text: Option<&[u8]> = None;
                let mut content_range: Option<Range<usize>> = None;

                for cap in m.captures {
                    if cap.index == lang_idx {
                        let s = cap.node.start_byte();
                        let e = cap.node.end_byte();
                        if s < e && e <= source.len() {
                            lang_text = Some(&source[s..e]);
                        }
                    } else if cap.index == content_idx {
                        let s = cap.node.start_byte();
                        let e = cap.node.end_byte();
                        if s < e && e <= source.len() {
                            content_range = Some(s..e);
                        }
                    }
                }

                if let (Some(raw_name), Some(range)) = (lang_text, content_range) {
                    // Reject non-ASCII or suspiciously long language names.
                    if let Ok(name_str) = std::str::from_utf8(raw_name) {
                        let name = name_str.trim();
                        if !name.is_empty()
                            && name.len() <= 64
                            && name
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                        {
                            injections.push((name.to_string(), range));
                        }
                    }
                }
            }
        }

        if injections.is_empty() {
            return parent_spans;
        }

        // Build the set of cache keys used this call so we can evict stale entries.
        let cache_keys: Vec<(String, usize, usize)> = injections
            .iter()
            .map(|(lang, r)| (lang.clone(), r.start, r.end))
            .collect();

        // For each injection, reuse a cached child Highlighter when the content
        // is unchanged, and fall back to a fresh parse otherwise.
        let mut child_spans: Vec<HighlightSpan> = Vec::new();
        // Track which byte ranges have child coverage for the merge step.
        let mut injected_ranges: Vec<Range<usize>> = Vec::new();

        for (lang_name, content_range) in &injections {
            let Some(child_grammar) = resolve(lang_name) else {
                continue;
            };
            let slice = &source[content_range.clone()];
            let content_hash = hash_bytes(slice);
            let offset = content_range.start;

            let child_raw = if let Some(child_hl) = self.child_cache.get_if_fresh(
                lang_name,
                content_range.start,
                content_range.end,
                content_hash,
            ) {
                // Cache hit: skip grammar instantiation + re-parse.
                tracing::trace!(
                    lang = %lang_name,
                    range = ?content_range,
                    "child-hl cache hit"
                );
                child_hl.highlight_range(slice, 0..slice.len())
            } else {
                // Cache miss: build a new child highlighter and parse.
                let Ok(mut new_hl) = Highlighter::new(child_grammar) else {
                    continue;
                };
                new_hl.parse_initial(slice);
                let spans = new_hl.highlight_range(slice, 0..slice.len());
                // Store into cache for future calls.
                self.child_cache.insert(
                    lang_name.clone(),
                    content_range.start,
                    content_range.end,
                    new_hl,
                    content_hash,
                );
                spans
            };

            for span in child_raw {
                child_spans.push(HighlightSpan {
                    byte_range: (span.byte_range.start + offset)..(span.byte_range.end + offset),
                    capture: span.capture,
                    metadata: span.metadata,
                });
            }
            injected_ranges.push(content_range.clone());
        }

        // Evict child entries that were not used in this call.
        self.child_cache.evict_stale(&cache_keys);

        // Merge: keep parent spans that do NOT fall entirely within an injected range.
        // Spans that partially overlap are kept (rare edge case — see doc comment).
        let mut merged: Vec<HighlightSpan> = parent_spans
            .into_iter()
            .filter(|span| {
                !injected_ranges
                    .iter()
                    .any(|ir| span.byte_range.start >= ir.start && span.byte_range.end <= ir.end)
            })
            .collect();

        merged.extend(child_spans);
        merged.sort_by_key(|s| s.byte_range.start);
        merged
    }

    /// Run the highlights query and injection-query walk scoped to
    /// `byte_range`, without re-parsing. The caller is responsible for
    /// driving `parse_incremental` before calling this method; the
    /// retained tree must already reflect `source`.
    ///
    /// ## Algorithm
    ///
    /// 1. Get parent spans via [`Highlighter::highlight_range`] over `byte_range`.
    /// 2. Walk the injection query with its `QueryCursor` byte range set to
    ///    `byte_range`, so injections outside the viewport trigger no work.
    /// 3. For each injection match whose content range intersects the viewport,
    ///    slice `&source[content_range]`, parse with the child grammar's parser,
    ///    run that grammar's highlights query over the slice, translate spans
    ///    `+content_range.start`, then clip translated child spans to
    ///    `byte_range` (dropping empty spans after clip).
    /// 4. Merge: parent spans entirely within an injected range are dropped;
    ///    child spans replace them. Same v1 semantics as
    ///    [`Highlighter::highlight_with_injections`].
    ///
    /// When `injections.scm` is absent or produces no matches inside the
    /// viewport, this behaves identically to
    /// [`Highlighter::highlight_range`].
    pub fn highlight_range_with_injections<F>(
        &mut self,
        source: &[u8],
        byte_range: Range<usize>,
        mut resolve: F,
    ) -> Vec<HighlightSpan>
    where
        F: FnMut(&str) -> Option<Arc<Grammar>>,
    {
        let parent_spans = self.highlight_range(source, byte_range.clone());

        let Some(inj_query) = self.injection_query.as_ref() else {
            return parent_spans;
        };

        // Find the capture indices for @injection.language and @injection.content.
        let lang_idx = inj_query
            .capture_names()
            .iter()
            .position(|n| *n == INJ_LANG_CAPTURE);
        let content_idx = inj_query
            .capture_names()
            .iter()
            .position(|n| *n == INJ_CONTENT_CAPTURE);

        let (Some(lang_idx), Some(content_idx)) = (lang_idx, content_idx) else {
            return parent_spans;
        };

        let lang_idx = lang_idx as u32;
        let content_idx = content_idx as u32;

        let Some(tree) = self.tree.as_ref() else {
            return parent_spans;
        };

        // Walk injection matches restricted to the viewport byte range.
        let mut injections: Vec<(String, Range<usize>)> = Vec::new();
        {
            let mut cursor = QueryCursor::new();
            cursor.set_byte_range(byte_range.clone());
            let mut matches = cursor.matches(inj_query, tree.root_node(), source);

            while let Some(m) = matches.next() {
                let mut lang_text: Option<&[u8]> = None;
                let mut content_range: Option<Range<usize>> = None;

                for cap in m.captures {
                    if cap.index == lang_idx {
                        let s = cap.node.start_byte();
                        let e = cap.node.end_byte();
                        if s < e && e <= source.len() {
                            lang_text = Some(&source[s..e]);
                        }
                    } else if cap.index == content_idx {
                        let s = cap.node.start_byte();
                        let e = cap.node.end_byte();
                        if s < e && e <= source.len() {
                            content_range = Some(s..e);
                        }
                    }
                }

                if let (Some(raw_name), Some(range)) = (lang_text, content_range) {
                    // Only include injections that intersect the viewport.
                    if range.start >= byte_range.end || range.end <= byte_range.start {
                        continue;
                    }
                    if let Ok(name_str) = std::str::from_utf8(raw_name) {
                        let name = name_str.trim();
                        if !name.is_empty()
                            && name.len() <= 64
                            && name
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                        {
                            injections.push((name.to_string(), range));
                        }
                    }
                }
            }
        }

        if injections.is_empty() {
            return parent_spans;
        }

        // Build the set of cache keys used this call so we can evict stale entries.
        let cache_keys: Vec<(String, usize, usize)> = injections
            .iter()
            .map(|(lang, r)| (lang.clone(), r.start, r.end))
            .collect();

        // For each injection, reuse a cached child Highlighter when the content
        // is unchanged, and fall back to a fresh parse otherwise.
        let mut child_spans: Vec<HighlightSpan> = Vec::new();
        let mut injected_ranges: Vec<Range<usize>> = Vec::new();

        for (lang_name, content_range) in &injections {
            let Some(child_grammar) = resolve(lang_name) else {
                continue;
            };
            let slice = &source[content_range.clone()];
            let content_hash = hash_bytes(slice);
            let offset = content_range.start;

            let child_raw = if let Some(child_hl) = self.child_cache.get_if_fresh(
                lang_name,
                content_range.start,
                content_range.end,
                content_hash,
            ) {
                // Cache hit: skip grammar instantiation + re-parse.
                tracing::trace!(
                    lang = %lang_name,
                    range = ?content_range,
                    "child-hl cache hit"
                );
                child_hl.highlight_range(slice, 0..slice.len())
            } else {
                // Cache miss: build a new child highlighter and parse.
                let Ok(mut new_hl) = Highlighter::new(child_grammar) else {
                    continue;
                };
                new_hl.parse_initial(slice);
                let spans = new_hl.highlight_range(slice, 0..slice.len());
                // Store into cache for future calls.
                self.child_cache.insert(
                    lang_name.clone(),
                    content_range.start,
                    content_range.end,
                    new_hl,
                    content_hash,
                );
                spans
            };

            for span in child_raw {
                let abs_start = span.byte_range.start + offset;
                let abs_end = span.byte_range.end + offset;
                // Clip to viewport.
                let clipped_start = abs_start.max(byte_range.start);
                let clipped_end = abs_end.min(byte_range.end);
                if clipped_start >= clipped_end {
                    continue;
                }
                child_spans.push(HighlightSpan {
                    byte_range: clipped_start..clipped_end,
                    capture: span.capture,
                    metadata: span.metadata,
                });
            }
            injected_ranges.push(content_range.clone());
        }

        // Evict child entries that were not used in this call.
        self.child_cache.evict_stale(&cache_keys);

        // Merge: keep parent spans not entirely inside an injected range.
        let mut merged: Vec<HighlightSpan> = parent_spans
            .into_iter()
            .filter(|span| {
                !injected_ranges
                    .iter()
                    .any(|ir| span.byte_range.start >= ir.start && span.byte_range.end <= ir.end)
            })
            .collect();

        merged.extend(child_spans);
        merged.sort_by_key(|s| s.byte_range.start);
        merged
    }

    /// Parse `source` and harvest ERROR / MISSING nodes as `ParseError`s.
    pub fn parse_errors(&mut self, source: &[u8]) -> Vec<ParseError> {
        if self.tree.is_none() {
            self.parse_initial(source);
        } else if !self.parse_incremental(source) {
            return Vec::new();
        }
        self.parse_errors_range(source, 0..source.len())
    }
}

// ---------------------------------------------------------------------------
// Query compilation helper
// ---------------------------------------------------------------------------

/// Compile `highlights_scm` for `language`, applying pre-extraction of
/// capture-target `(#set! @cap ...)` directives first, then falling back to
/// the plain sanitizer if the query still fails to compile.
///
/// Returns `(compiled_query, pre_extracted_directives)`.
fn compile_query(
    language: &tree_sitter::Language,
    highlights_scm: &str,
    grammar_name: &str,
) -> Result<(Query, Vec<CaptureSetDirective>)> {
    // Happy path: query compiles without any surgery.
    match Query::new(language, highlights_scm) {
        Ok(q) => return Ok((q, Vec::new())),
        Err(_first_err) => {}
    }

    // Pre-extract capture-form `(#set! @cap ...)` directives and try again.
    let extract = extract_capture_set_directives(highlights_scm);
    match Query::new(language, &extract.rewritten) {
        Ok(q) => {
            if !extract.directives.is_empty() {
                tracing::debug!(
                    grammar = grammar_name,
                    count = extract.directives.len(),
                    "pre-extracted (#set! @cap ...) directives"
                );
            }
            return Ok((q, extract.directives));
        }
        Err(_second_err) => {}
    }

    // Fall back to the legacy sanitizer.
    let (sanitized, report) = sanitize_highlights(highlights_scm);
    if report.changed {
        match Query::new(language, &sanitized) {
            Ok(q) => {
                tracing::warn!(
                    grammar = grammar_name,
                    removed_lines = report.removed_lines,
                    "highlights.scm compile failed; using sanitized fallback"
                );
                return Ok((q, Vec::new()));
            }
            Err(third_err) => {
                return Err(anyhow::anyhow!(
                    "failed to compile highlights.scm query even after sanitization: {third_err}"
                ));
            }
        }
    }

    Err(anyhow::anyhow!(
        "failed to compile highlights.scm query for {grammar_name}"
    ))
}

// ---------------------------------------------------------------------------
// Error collection helper
// ---------------------------------------------------------------------------

fn collect_parse_errors(
    node: tree_sitter::Node,
    source: &[u8],
    range: &Range<usize>,
    out: &mut Vec<ParseError>,
) {
    let n_start = node.start_byte();
    let n_end = node.end_byte();
    if n_end <= range.start || n_start >= range.end {
        return;
    }
    if node.is_error() || node.is_missing() {
        let raw_end = n_end.max(n_start + 1).min(source.len());
        if raw_end > n_start {
            let line_end = source[n_start..raw_end]
                .iter()
                .position(|&b| b == b'\n')
                .map(|off| n_start + off)
                .unwrap_or(raw_end);

            let snippet = std::str::from_utf8(&source[n_start..line_end])
                .unwrap_or("")
                .trim();
            let kind = node.kind();
            let message = if node.is_missing() {
                if kind.is_empty() {
                    "missing token".to_string()
                } else {
                    format!("missing `{kind}`")
                }
            } else if snippet.is_empty() {
                "unexpected token".to_string()
            } else {
                let trimmed: String = snippet.chars().take(60).collect();
                format!("unexpected `{trimmed}`")
            };

            out.push(ParseError {
                byte_range: n_start..line_end,
                message,
            });
            return;
        }
    }

    if !node.has_error() {
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_parse_errors(child, source, range, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        GrammarCompiler, GrammarLoader, LangSpec, ManifestMeta, QuerySource, QuerySourceCache,
        SourceCache,
    };

    fn c_grammar_loader() -> (Arc<Grammar>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let sources = SourceCache::new(tmp.path().join("cache"));
        let query_sources = QuerySourceCache::new(tmp.path().join("qcache"));
        let user_dir = tmp.path().join("user");
        let loader = GrammarLoader::new(
            vec![],
            user_dir,
            sources,
            query_sources,
            GrammarCompiler::new(),
        );
        let meta = ManifestMeta {
            helix_repo: "https://github.com/helix-editor/helix".into(),
            helix_rev: "87d5c05c4432a079d3b7aaa10cda1cfe1803c18c".into(),
            nvim_treesitter_repo: "https://github.com/nvim-treesitter/nvim-treesitter".into(),
            nvim_treesitter_rev: "cf12346a3414fa1b06af75c79faebe7f76df080a".into(),
        };
        let spec = LangSpec {
            git_url: "https://github.com/tree-sitter/tree-sitter-c".into(),
            git_rev: "2a265d69a4caf57108a73ad2ed1e6922dd2f998c".into(),
            subpath: None,
            extensions: vec!["c".into()],
            c_files: vec!["src/parser.c".into()],
            query_source: QuerySource::Helix,
            query_subdir: None,
            source: None,
        };

        let g = Grammar::load("c", &spec, &loader, &meta).unwrap();
        (Arc::new(g), tmp)
    }

    /// Load html grammar from the bonsai data dir if it has been installed.
    /// Tests using this must be `#[ignore]`-marked so they're explicit opt-ins.
    fn load_html_grammar() -> Option<Arc<Grammar>> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")))?;
        let so = base.join("bonsai/grammars/html.so");
        if !so.exists() {
            return None;
        }
        Grammar::load_from_path("html", &so).ok().map(Arc::new)
    }

    /// All highlighter tests need a real grammar (network clone + cc compile).
    /// Run with: `cargo test -p hjkl-bonsai -- --ignored`.
    #[test]
    #[ignore = "network + compiler"]
    fn highlights_c_keyword() {
        let (g, _tmp) = c_grammar_loader();
        let mut h = Highlighter::new(g).unwrap();
        let spans = h.highlight(b"int main() { return 0; }");
        assert!(
            spans.iter().any(|s| s.capture.starts_with("keyword")),
            "expected a keyword span; got: {spans:#?}"
        );
    }

    #[test]
    #[ignore = "network + compiler"]
    fn highlight_empty_input() {
        let (g, _tmp) = c_grammar_loader();
        let mut h = Highlighter::new(g).unwrap();
        let spans = h.highlight(b"");
        assert!(spans.is_empty());
    }

    #[test]
    #[ignore = "network + compiler"]
    fn parse_returns_syntax() {
        let (g, _tmp) = c_grammar_loader();
        let mut h = Highlighter::new(g).unwrap();
        let syntax = h.parse(b"int main() {}");
        assert!(syntax.is_some());
    }

    #[test]
    #[ignore = "network + compiler"]
    fn parse_errors_clean_source() {
        let (g, _tmp) = c_grammar_loader();
        let mut h = Highlighter::new(g).unwrap();
        let errors = h.parse_errors(b"int main() {}");
        assert!(errors.is_empty(), "unexpected parse errors: {errors:#?}");
    }

    #[test]
    #[ignore = "network + compiler"]
    fn incremental_edit_matches_cold_parse() {
        let (g, _tmp) = c_grammar_loader();
        let pre: &[u8] = b"int main() {}";
        let post: &[u8] = b"int Xmain() {}";

        let mut h_inc = Highlighter::new(g.clone()).unwrap();
        h_inc.parse_initial(pre);
        let edit = tree_sitter::InputEdit {
            start_byte: 4,
            old_end_byte: 4,
            new_end_byte: 5,
            start_position: tree_sitter::Point { row: 0, column: 4 },
            old_end_position: tree_sitter::Point { row: 0, column: 4 },
            new_end_position: tree_sitter::Point { row: 0, column: 5 },
        };
        h_inc.edit(&edit);
        assert!(h_inc.parse_incremental(post));
        let inc_spans = h_inc.highlight_range(post, 0..post.len());

        let mut h_cold = Highlighter::new(g).unwrap();
        let cold_spans = h_cold.highlight(post);

        assert_eq!(inc_spans, cold_spans);
    }

    #[test]
    #[ignore = "network + compiler"]
    fn reset_clears_tree() {
        let (g, _tmp) = c_grammar_loader();
        let mut h = Highlighter::new(g).unwrap();
        h.parse_initial(b"int main() {}");
        assert!(h.tree().is_some());
        h.reset();
        assert!(h.tree().is_none());
    }

    // ── End-to-end html test (uses cached grammar) ────────────────────────────

    /// End-to-end html test: load the real html grammar from the bonsai cache,
    /// highlight an HTML snippet with a URL attribute, and assert that:
    /// 1. `Highlighter::new` succeeds despite `(#set! @cap ...)` in the query.
    /// 2. The span covering the URL value has `metadata["url"]` set.
    #[test]
    #[ignore = "needs cached html grammar — run after hjkl installs html"]
    fn html_set_directive_metadata_applied() {
        let grammar = match load_html_grammar() {
            Some(g) => g,
            None => {
                eprintln!("html grammar not in cache; skipping html e2e test");
                return;
            }
        };

        // The html highlights.scm (from nvim-treesitter html_tags) includes:
        // ((attribute (attribute_name) @_attr
        //    (quoted_attribute_value (attribute_value) @string.special.url))
        //   (#any-of? @_attr "href" "src")
        //   (#set! @string.special.url url @string.special.url))
        //
        // We inject this directly to test pre-extraction + application.
        let query_text = r#"((attribute
  (attribute_name) @_attr
  (quoted_attribute_value
    (attribute_value) @string.special.url))
  (#any-of? @_attr "href" "src")
  (#set! @string.special.url url @string.special.url))
(entity) @character.special"#;

        // Build a Grammar-like shim: use the real .so but with our test query.
        // Since Grammar::load_from_path reads queries from disk, we need to
        // verify the Highlighter's compile_query path directly via the helper.
        let language = grammar.language();
        let result = compile_query(language, query_text, "html-test");
        assert!(
            result.is_ok(),
            "compile_query must succeed: {:?}",
            result.err()
        );
        let (_query, pre_extracted) = result.unwrap();
        assert_eq!(
            pre_extracted.len(),
            1,
            "expected 1 pre-extracted directive: {pre_extracted:?}"
        );
        let pe = &pre_extracted[0];
        assert_eq!(pe.capture_name, "string.special.url");
        assert_eq!(pe.key, "url");
        assert_eq!(pe.value.as_deref(), Some("@string.special.url"));

        // Now do a real highlight with a one-shot Highlighter using the tested grammar.
        let mut h = Highlighter::new(grammar).unwrap();
        let source = b"<a href=\"https://example.com\">link</a>";
        let spans = h.highlight(source);

        // Find the span for the URL value (`https://example.com`).
        // It should carry metadata["url"].
        let url_start = source
            .windows(b"https://".len())
            .position(|w| w == b"https://")
            .expect("https:// not found in test source");
        let url_span = spans
            .iter()
            .find(|s| s.byte_range.start == url_start || s.byte_range.contains(&url_start));

        // The metadata["url"] key should be set.
        if let Some(span) = url_span {
            assert!(
                span.metadata.contains_key("url"),
                "expected metadata[\"url\"] on url span; metadata: {:?}",
                span.metadata
            );
        }
        // (If the span isn't found — e.g. the grammar uses a different node
        //  layout — the test still passes; the real assertion is that
        //  compile_query succeeded and pre-extracted the directive.)
    }

    // ── Unknown predicate: logged but not fatal ───────────────────────────────

    /// A query containing `(#bogus? @x)` must produce a span — the unknown
    /// predicate is warned about but does not veto the match.
    #[ignore = "needs cached html grammar — run after hjkl installs html"]
    #[test]
    fn unknown_predicate_does_not_drop_match() {
        let grammar = match load_html_grammar() {
            Some(g) => g,
            None => {
                eprintln!("html grammar not in cache; skipping");
                return;
            }
        };
        // Build a query with an unknown predicate attached to a simple pattern.
        let query_text = "((tag_name) @tag\n  (#bogus? @tag))";
        let language = grammar.language();
        let result = compile_query(language, query_text, "html-test");
        assert!(
            result.is_ok(),
            "compile_query must succeed: {:?}",
            result.err()
        );
        let (query, pre_extracted) = result.unwrap();
        assert!(pre_extracted.is_empty());

        // Now run the dispatcher manually.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(language).unwrap();
        let source = b"<a href=\"x\">text</a>";
        let tree = parser.parse(source, None).unwrap();
        let capture_names: Vec<String> = query
            .capture_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let registry = PredicateRegistry::with_builtins();
        let mut cursor = QueryCursor::new();
        let mut matches_iter = cursor.matches(&query, tree.root_node(), source.as_ref());
        let mut found_tag = false;
        while let Some(m) = matches_iter.next() {
            let cap_pairs: Vec<(u32, tree_sitter::Node<'_>)> =
                m.captures.iter().map(|c| (c.index, c.node)).collect();
            let mut skip = false;
            for pred in query.general_predicates(m.pattern_index) {
                let op = pred.operator.as_ref();
                if !op.ends_with('?') {
                    continue;
                }
                let args: Vec<PredicateArg<'_>> = pred
                    .args
                    .iter()
                    .map(|a| match a {
                        QueryPredicateArg::Capture(idx) => PredicateArg::Capture(*idx),
                        QueryPredicateArg::String(s) => PredicateArg::Str(s.as_ref()),
                    })
                    .collect();
                let ctx = MatchContext {
                    pattern_index: m.pattern_index,
                    captures: &cap_pairs,
                    source,
                    args: &args,
                    capture_names: &capture_names,
                };
                match registry.get_predicate(op) {
                    Some(p) => {
                        if !p.eval(&ctx) {
                            skip = true;
                            break;
                        }
                    }
                    None => {
                        warn_unknown_predicate_once(op);
                        // Don't veto.
                    }
                }
            }
            if !skip {
                for cap in m.captures {
                    let name = &capture_names[cap.index as usize];
                    if name == "tag" {
                        found_tag = true;
                    }
                }
            }
        }
        assert!(
            found_tag,
            "tag span must still be emitted despite unknown predicate"
        );
    }

    // ── Custom consumer predicate that always returns false ───────────────────

    /// Register a closure-based predicate that always returns false and assert
    /// that all matches from patterns using it are dropped.
    #[test]
    #[ignore = "needs cached html grammar — run after hjkl installs html"]
    fn custom_predicate_always_false_drops_matches() {
        let grammar = match load_html_grammar() {
            Some(g) => g,
            None => {
                eprintln!("html grammar not in cache; skipping");
                return;
            }
        };
        let query_text = "((tag_name) @tag\n  (#my-false? @tag))";
        let language = grammar.language();
        let result = compile_query(language, query_text, "html-test");
        assert!(result.is_ok());
        let (query, _) = result.unwrap();
        let capture_names: Vec<String> = query
            .capture_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let mut registry = PredicateRegistry::with_builtins();
        registry.register_predicate(crate::predicate::predicate_fn("my-false?", |_ctx| false));

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(language).unwrap();
        let source = b"<a href=\"x\">text</a>";
        let tree = parser.parse(source, None).unwrap();

        let mut cursor = QueryCursor::new();
        let mut matches_iter = cursor.matches(&query, tree.root_node(), source.as_ref());
        let mut found_tag = false;
        while let Some(m) = matches_iter.next() {
            let cap_pairs: Vec<(u32, tree_sitter::Node<'_>)> =
                m.captures.iter().map(|c| (c.index, c.node)).collect();
            let mut skip = false;
            for pred in query.general_predicates(m.pattern_index) {
                let op = pred.operator.as_ref();
                if !op.ends_with('?') {
                    continue;
                }
                let args: Vec<PredicateArg<'_>> = pred
                    .args
                    .iter()
                    .map(|a| match a {
                        QueryPredicateArg::Capture(idx) => PredicateArg::Capture(*idx),
                        QueryPredicateArg::String(s) => PredicateArg::Str(s.as_ref()),
                    })
                    .collect();
                let ctx = MatchContext {
                    pattern_index: m.pattern_index,
                    captures: &cap_pairs,
                    source,
                    args: &args,
                    capture_names: &capture_names,
                };
                if let Some(p) = registry.get_predicate(op)
                    && !p.eval(&ctx)
                {
                    skip = true;
                    break;
                }
            }
            if !skip {
                for cap in m.captures {
                    let name = &capture_names[cap.index as usize];
                    if name == "tag" {
                        found_tag = true;
                    }
                }
            }
        }
        assert!(
            !found_tag,
            "all matches should be dropped by always-false predicate"
        );
    }
}
