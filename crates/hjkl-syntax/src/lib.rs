//! Renderer-agnostic syntax-highlighting pipeline for the hjkl editor stack.
//!
//! Fully synchronous: parse and highlight run on the main thread.
//! Call [`SyntaxLayer::set_language_for_path`] after opening a file,
//! [`SyntaxLayer::apply_edits`] after each batch of [`hjkl_engine::ContentEdit`]s,
//! and [`SyntaxLayer::render_viewport`] to get styled spans for the visible rows.
//!
//! Output is renderer-agnostic: [`RenderOutput::spans`] carries
//! `(byte_start, byte_end, [`StyleSpec`])` triples.
//! A TUI adapter ([`hjkl-syntax-tui`]) maps these to `ratatui::style::Style`.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use hjkl_bonsai::runtime::{Grammar, LoadHandle};
use hjkl_bonsai::{
    CommentMarkerPass, DotFallbackTheme, HEX_BG_KEY, HEX_COLOR_CAPTURE, HEX_FG_KEY, HexColorPass,
    Highlighter, InputEdit, MetaValue, Point, RAINBOW_BRACKET_CAPTURE, RAINBOW_DEPTH_KEY, Theme,
    rainbow_spans_rope,
};
use hjkl_engine::Query;
use hjkl_lang::{GrammarRequest, LanguageDirectory};

pub use hjkl_theme::{Color, Modifiers, StyleSpec};

/// Stable identifier for an open buffer.
///
/// # Examples
///
/// ```
/// use hjkl_syntax::BufferId;
/// let id: BufferId = 42;
/// assert_eq!(id, 42);
/// ```
pub use hjkl_buffer::BufferId;

// ---------------------------------------------------------------------------
// Public output types
// ---------------------------------------------------------------------------

/// A single diagnostic sign emitted from the syntax pipeline.
///
/// # Examples
///
/// ```
/// use hjkl_syntax::DiagSign;
/// let s = DiagSign::new(3, 'E', 100);
/// assert_eq!(s.row, 3);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DiagSign {
    /// Document row (0-indexed).
    pub row: usize,
    /// Gutter character (e.g. `'E'` for a syntax error).
    pub ch: char,
    /// Gutter priority — higher wins when multiple signs land on the same row.
    pub priority: u8,
}

impl Default for DiagSign {
    fn default() -> Self {
        Self {
            row: 0,
            ch: 'E',
            priority: 0,
        }
    }
}

impl DiagSign {
    /// Create a new diagnostic sign.
    ///
    /// # Examples
    ///
    /// ```
    /// use hjkl_syntax::DiagSign;
    /// let s = DiagSign::new(1, 'E', 100);
    /// assert_eq!(s.row, 1);
    /// ```
    pub fn new(row: usize, ch: char, priority: u8) -> Self {
        Self { row, ch, priority }
    }
}

/// Per-call sub-step timings. Kept for API compat (PerfBreakdown is re-exported
/// in the TUI shim and referenced from `:perf` overlay code).
///
/// # Examples
///
/// ```
/// use hjkl_syntax::PerfBreakdown;
/// let p = PerfBreakdown::default();
/// assert_eq!(p.parse_us, 0);
/// ```
#[derive(Default, Debug, Clone, Copy)]
#[non_exhaustive]
pub struct PerfBreakdown {
    /// Microseconds spent building the source string + row_starts table.
    pub source_build_us: u128,
    /// Microseconds spent in `tree_sitter::Parser::parse`.
    pub parse_us: u128,
    /// Microseconds spent in `hjkl_bonsai::Highlighter::highlight_range_*`.
    pub highlight_us: u128,
    /// Microseconds spent building the per-row span table from flat spans.
    pub by_row_us: u128,
    /// Microseconds spent scanning for diagnostic ERROR/MISSING nodes.
    pub diag_us: u128,
}

impl PerfBreakdown {
    /// Construct a zeroed breakdown.
    ///
    /// # Examples
    ///
    /// ```
    /// use hjkl_syntax::PerfBreakdown;
    /// let p = PerfBreakdown::new();
    /// assert_eq!(p.highlight_us, 0);
    /// ```
    pub fn new() -> Self {
        Self::default()
    }
}

/// Per-frame output of the syntax pipeline.
///
/// Contains the styled span table (one inner `Vec` per document row) and the
/// diagnostic signs for the gutter.
///
/// # Examples
///
/// ```
/// use hjkl_syntax::{RenderOutput, PerfBreakdown};
/// let out = RenderOutput::new(0, Vec::new(), Vec::new(), (0, 0, 0), PerfBreakdown::default());
/// assert_eq!(out.buffer_id, 0);
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RenderOutput {
    /// Routes spans/signs back to the matching buffer slot.
    pub buffer_id: BufferId,
    /// Per-row span table.
    pub spans: Vec<Vec<(usize, usize, StyleSpec)>>,
    /// Diagnostic signs for the gutter.
    pub signs: Vec<DiagSign>,
    /// `(dirty_gen, viewport_top, viewport_height)` cache key.
    pub key: (u64, usize, usize),
    /// Sub-step timing breakdown (zeroed in fully-sync path).
    pub perf: PerfBreakdown,
}

impl RenderOutput {
    /// Construct a new `RenderOutput`.
    ///
    /// # Examples
    ///
    /// ```
    /// use hjkl_syntax::{RenderOutput, PerfBreakdown};
    /// let out = RenderOutput::new(1, Vec::new(), Vec::new(), (7, 0, 30), PerfBreakdown::new());
    /// assert_eq!(out.buffer_id, 1);
    /// ```
    pub fn new(
        buffer_id: BufferId,
        spans: Vec<Vec<(usize, usize, StyleSpec)>>,
        signs: Vec<DiagSign>,
        key: (u64, usize, usize),
        perf: PerfBreakdown,
    ) -> Self {
        Self {
            buffer_id,
            spans,
            signs,
            key,
            perf,
        }
    }
}

impl PartialEq for RenderOutput {
    fn eq(&self, other: &Self) -> bool {
        self.spans == other.spans
            && self.signs.len() == other.signs.len()
            && self
                .signs
                .iter()
                .zip(other.signs.iter())
                .all(|(a, b)| a.row == b.row && a.ch == b.ch && a.priority == b.priority)
    }
}

// ---------------------------------------------------------------------------
// Public outcome types for set_language_for_path / poll_pending_loads
// ---------------------------------------------------------------------------

/// Outcome of [`SyntaxLayer::set_language_for_path`].
///
/// # Examples
///
/// ```
/// use hjkl_syntax::SetLanguageOutcome;
/// assert!(SetLanguageOutcome::Ready.is_known());
/// assert!(SetLanguageOutcome::Loading("rust".to_string()).is_known());
/// assert!(!SetLanguageOutcome::Unknown.is_known());
/// ```
#[non_exhaustive]
pub enum SetLanguageOutcome {
    /// Grammar was already cached — installed immediately.
    Ready,
    /// Grammar is being fetched/compiled on the background pool.
    Loading(#[allow(dead_code)] String),
    /// Extension unrecognized. No grammar — plain text only.
    Unknown,
}

impl SetLanguageOutcome {
    /// `true` when a grammar was found (either already cached or now in flight).
    pub fn is_known(&self) -> bool {
        matches!(self, Self::Ready | Self::Loading(_))
    }
}

/// Event emitted by [`SyntaxLayer::poll_pending_loads`].
///
/// # Examples
///
/// ```
/// use hjkl_syntax::LoadEvent;
/// let e = LoadEvent::Ready { id: 0, name: "rust".into() };
/// match e {
///     LoadEvent::Ready { id, name } => assert_eq!(name, "rust"),
///     LoadEvent::Failed { .. } => panic!("unexpected"),
///     _ => {}
/// }
/// ```
#[non_exhaustive]
pub enum LoadEvent {
    /// Grammar installed; trigger a redraw + re-render for `id`.
    Ready { id: BufferId, name: String },
    /// Load failed; buffer stays plain text.
    Failed {
        id: BufferId,
        name: String,
        error: String,
    },
}

/// Exhaustive view of a [`LoadEvent`] for dispatch callbacks.
#[derive(Debug)]
pub enum LoadEventKind<'a> {
    /// Grammar installed successfully.
    Ready { id: BufferId, name: &'a str },
    /// Grammar load failed.
    Failed {
        id: BufferId,
        name: &'a str,
        error: &'a str,
    },
}

// ---------------------------------------------------------------------------
// In-flight grammar load tracking
// ---------------------------------------------------------------------------

struct PendingLoad {
    id: BufferId,
    name: String,
    handle: LoadHandle,
}

// ---------------------------------------------------------------------------
// Per-buffer client state (main thread)
// ---------------------------------------------------------------------------

/// Per-buffer state owned by the main-thread [`SyntaxLayer`].
struct BufferClient {
    has_language: bool,
    current_lang: Option<Arc<Grammar>>,
    /// Owns Parser + Tree for this buffer.
    highlighter: Option<Highlighter>,
    /// dirty_gen the cache was built at (None = cache absent).
    cache_dirty_gen: Option<u64>,
    /// Contiguous row range covered by `cache_spans`.
    cache_rows: Range<usize>,
    /// Per-row span table for `cache_rows`.
    cache_spans: Vec<Vec<(usize, usize, StyleSpec)>>,
    /// `(dirty_gen, row_starts)` — rebuilt only when dirty_gen changes.
    cache_row_starts: Option<(u64, Arc<Vec<usize>>)>,
    /// dirty_gen of the most recent successful parse. Gate reparsing.
    parsed_dirty_gen: Option<u64>,
    /// Cached diag signs keyed by `(dirty_gen, vp_top, vp_end)`.
    cache_signs: Option<(u64, usize, usize, Vec<DiagSign>)>,
}

impl Default for BufferClient {
    fn default() -> Self {
        Self {
            has_language: false,
            current_lang: None,
            highlighter: None,
            cache_dirty_gen: None,
            cache_rows: 0..0,
            cache_spans: Vec::new(),
            cache_row_starts: None,
            parsed_dirty_gen: None,
            cache_signs: None,
        }
    }
}

impl BufferClient {
    fn invalidate_cache(&mut self) {
        self.cache_dirty_gen = None;
        self.cache_rows = 0..0;
        self.cache_spans.clear();
        self.cache_row_starts = None;
        self.parsed_dirty_gen = None;
        self.cache_signs = None;
    }
}

// ---------------------------------------------------------------------------
// SyntaxLayer — main-thread, fully synchronous
// ---------------------------------------------------------------------------

/// Per-App syntax highlighting layer. Multiplexes per-buffer state.
/// Fully synchronous — no background thread.
///
/// # Examples
///
/// ```no_run
/// use std::sync::Arc;
/// use hjkl_syntax::SyntaxLayer;
/// use hjkl_bonsai::DotFallbackTheme;
/// use hjkl_lang::LanguageDirectory;
///
/// let theme = Arc::new(DotFallbackTheme::dark());
/// let dir = Arc::new(LanguageDirectory::new().unwrap());
/// let layer = SyntaxLayer::new(theme, dir);
/// ```
pub struct SyntaxLayer {
    /// Shared grammar resolver.
    pub directory: Arc<LanguageDirectory>,
    theme: Arc<dyn Theme + Send + Sync>,
    clients: HashMap<BufferId, BufferClient>,
    pending_loads: Vec<PendingLoad>,
    /// When `false`, `HexColorPass` is skipped for all buffers.
    colorizer: bool,
    /// Filetype allowlist for the colorizer. Empty = allow all.
    colorizer_filetypes: Vec<String>,
    /// When `true`, rainbow bracket overlay is applied. Default `true`.
    rainbow_brackets: bool,
}

impl SyntaxLayer {
    /// Create a new layer with no buffers attached.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::sync::Arc;
    /// use hjkl_syntax::SyntaxLayer;
    /// use hjkl_bonsai::DotFallbackTheme;
    /// use hjkl_lang::LanguageDirectory;
    ///
    /// let theme = Arc::new(DotFallbackTheme::dark());
    /// let dir = Arc::new(LanguageDirectory::new().unwrap());
    /// let layer = SyntaxLayer::new(theme, dir);
    /// ```
    pub fn new(theme: Arc<dyn Theme + Send + Sync>, directory: Arc<LanguageDirectory>) -> Self {
        Self {
            directory,
            theme,
            clients: HashMap::new(),
            pending_loads: Vec::new(),
            colorizer: true,
            colorizer_filetypes: vec![
                "css".to_string(),
                "scss".to_string(),
                "sass".to_string(),
                "less".to_string(),
                "html".to_string(),
                "vue".to_string(),
                "svelte".to_string(),
                "tailwindcss".to_string(),
                "toml".to_string(),
                "lua".to_string(),
                "vim".to_string(),
            ],
            rainbow_brackets: true,
        }
    }

    /// Update rainbow bracket settings. Pass `enabled = false` to disable the
    /// rainbow overlay globally. No-op when the value is unchanged so per-frame
    /// pushes from the app stay cheap. Caches invalidate only on actual change.
    pub fn set_rainbow_brackets(&mut self, enabled: bool) {
        if self.rainbow_brackets == enabled {
            return;
        }
        self.rainbow_brackets = enabled;
        for client in self.clients.values_mut() {
            client.invalidate_cache();
        }
    }

    /// Update colorizer settings. Pass `enabled = false` to disable
    /// the color-literal overlay globally. `filetypes` is the allowlist
    /// of language names (e.g. `"css"`, `"toml"`); an empty slice means
    /// no filetype is allowed (same effect as `enabled = false`).
    ///
    /// No-op when the values are unchanged so per-frame pushes from the
    /// app stay cheap. Caches invalidate only on actual change.
    pub fn set_colorizer(&mut self, enabled: bool, filetypes: Vec<String>) {
        if self.colorizer == enabled && self.colorizer_filetypes == filetypes {
            return;
        }
        self.colorizer = enabled;
        self.colorizer_filetypes = filetypes;
        for client in self.clients.values_mut() {
            client.invalidate_cache();
        }
    }

    /// Borrow the shared language directory.
    pub fn directory(&self) -> &Arc<LanguageDirectory> {
        &self.directory
    }

    fn client_mut(&mut self, id: BufferId) -> &mut BufferClient {
        self.clients.entry(id).or_default()
    }

    /// Detect the language for `path` and attach a grammar.
    ///
    /// - `Ready`   — grammar cached; highlighter installed immediately.
    /// - `Loading` — grammar compiling; renders as plain text until
    ///   `poll_pending_loads` fires `LoadEvent::Ready`.
    /// - `Unknown` — unrecognized extension; plain text only.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::sync::Arc;
    /// use std::path::Path;
    /// use hjkl_syntax::{SyntaxLayer, SetLanguageOutcome};
    /// use hjkl_bonsai::DotFallbackTheme;
    /// use hjkl_lang::LanguageDirectory;
    ///
    /// let theme = Arc::new(DotFallbackTheme::dark());
    /// let dir = Arc::new(LanguageDirectory::new().unwrap());
    /// let mut layer = SyntaxLayer::new(theme, dir);
    /// let outcome = layer.set_language_for_path(0, Path::new("a.zzz_not_real"));
    /// assert!(!outcome.is_known());
    /// ```
    pub fn set_language_for_path(&mut self, id: BufferId, path: &Path) -> SetLanguageOutcome {
        match self.directory.request_for_path(path) {
            GrammarRequest::Cached(grammar) => {
                self.attach_grammar(id, grammar.clone());
                let c = self.client_mut(id);
                c.current_lang = Some(grammar);
                c.has_language = true;
                SetLanguageOutcome::Ready
            }
            GrammarRequest::Loading { name, handle } => {
                let c = self.client_mut(id);
                c.current_lang = None;
                c.has_language = false;
                c.highlighter = None;
                c.invalidate_cache();
                self.pending_loads.push(PendingLoad {
                    id,
                    name: name.clone(),
                    handle,
                });
                SetLanguageOutcome::Loading(name)
            }
            GrammarRequest::Unknown | _ => {
                let c = self.client_mut(id);
                c.current_lang = None;
                c.has_language = false;
                c.highlighter = None;
                c.invalidate_cache();
                SetLanguageOutcome::Unknown
            }
        }
    }

    /// Attach a grammar to a buffer, creating/replacing the Highlighter.
    fn attach_grammar(&mut self, id: BufferId, grammar: Arc<Grammar>) {
        let c = self.clients.entry(id).or_default();
        c.invalidate_cache();
        match Highlighter::new(grammar) {
            Ok(h) => {
                c.highlighter = Some(h);
            }
            Err(e) => {
                tracing::error!(buffer_id = id, error = %e, "failed to attach highlighter");
                c.highlighter = None;
            }
        }
    }

    /// Poll all in-flight grammar loads. Call once per tick.
    ///
    /// Returns one `LoadEvent` per handle that resolved during this tick.
    pub fn poll_pending_loads(&mut self) -> Vec<LoadEvent> {
        let mut events = Vec::new();
        let mut i = 0;
        while i < self.pending_loads.len() {
            match self.pending_loads[i].handle.try_recv() {
                None => {
                    i += 1;
                }
                Some(Ok(lib_path)) => {
                    let name = self.pending_loads[i].name.clone();
                    let bid = self.pending_loads[i].id;
                    self.pending_loads.swap_remove(i);
                    match self.directory.complete_load(&name, lib_path) {
                        Ok(grammar) => {
                            self.attach_grammar(bid, grammar.clone());
                            let c = self.client_mut(bid);
                            c.current_lang = Some(grammar);
                            c.has_language = true;
                            events.push(LoadEvent::Ready { id: bid, name });
                        }
                        Err(e) => {
                            events.push(LoadEvent::Failed {
                                id: bid,
                                name,
                                error: format!("{e:#}"),
                            });
                        }
                    }
                }
                Some(Err(err)) => {
                    let name = self.pending_loads[i].name.clone();
                    let bid = self.pending_loads[i].id;
                    self.pending_loads.swap_remove(i);
                    events.push(LoadEvent::Failed {
                        id: bid,
                        name,
                        error: err.to_string(),
                    });
                }
            }
        }
        events
    }

    /// Drop all state for a buffer. Call on close.
    pub fn forget(&mut self, id: BufferId) {
        self.clients.remove(&id);
    }

    /// Swap the active theme. Next `render_viewport` call uses the new theme.
    pub fn set_theme(&mut self, theme: Arc<dyn Theme + Send + Sync>) {
        self.theme = theme;
        // Invalidate all per-buffer caches so they repaint with the new theme.
        for c in self.clients.values_mut() {
            c.invalidate_cache();
        }
    }

    /// Apply a batch of engine `ContentEdit`s to the buffer's retained tree
    /// synchronously. The cache will be invalidated on the next `render_viewport`
    /// call via dirty_gen mismatch.
    ///
    /// No-op when no grammar is attached.
    pub fn apply_edits(&mut self, id: BufferId, edits: &[hjkl_engine::ContentEdit]) {
        let c = match self.clients.get_mut(&id) {
            Some(c) if c.has_language => c,
            _ => return,
        };
        let h = match c.highlighter.as_mut() {
            Some(h) => h,
            None => return,
        };
        for e in edits {
            h.edit(&InputEdit {
                start_byte: e.start_byte,
                old_end_byte: e.old_end_byte,
                new_end_byte: e.new_end_byte,
                start_position: Point {
                    row: e.start_position.0 as usize,
                    column: e.start_position.1 as usize,
                },
                old_end_position: Point {
                    row: e.old_end_position.0 as usize,
                    column: e.old_end_position.1 as usize,
                },
                new_end_position: Point {
                    row: e.new_end_position.0 as usize,
                    column: e.new_end_position.1 as usize,
                },
            });
        }
        // dirty_gen will advance — invalidate parse + row_starts + sign caches.
        // cache_spans / cache_rows are dropped on dirty_gen mismatch in render_viewport.
        c.parsed_dirty_gen = None;
        c.cache_row_starts = None;
        c.cache_signs = None;
    }

    /// Drop the buffer's retained tree. Next `render_viewport` reparses from scratch.
    ///
    /// Call on `:e!` / content reset.
    pub fn reset(&mut self, id: BufferId) {
        if let Some(c) = self.clients.get_mut(&id) {
            if let Some(h) = c.highlighter.as_mut() {
                h.reset();
            }
            c.invalidate_cache();
        }
    }

    /// Render spans for the visible viewport. Fully synchronous.
    ///
    /// 1. Returns `None` when no grammar is attached.
    /// 2. Clears the cache when `buffer.dirty_gen()` has advanced.
    /// 3. Returns cached rows when the request is fully inside the cached range.
    /// 4. Walks only rows outside the cache (extend prefix/suffix), splices into
    ///    `cache_spans`, extends `cache_rows`.
    pub fn render_viewport(
        &mut self,
        id: BufferId,
        buffer: &impl Query,
        viewport_top: usize,
        viewport_height: usize,
    ) -> Option<RenderOutput> {
        let client = self.clients.get_mut(&id)?;
        if !client.has_language {
            return None;
        }
        let dg = buffer.dirty_gen();
        let row_count = buffer.line_count() as usize;
        if row_count == 0 || viewport_height == 0 {
            return None;
        }

        let vp_top = viewport_top.min(row_count);
        let vp_end = (vp_top + viewport_height).min(row_count);
        if vp_end <= vp_top {
            return None;
        }

        // Single dirty_gen invalidation point.
        if client.cache_dirty_gen != Some(dg) {
            client.invalidate_cache();
        }

        // Get a rope snapshot — O(1) Arc-clone from hjkl_buffer::Buffer.
        // All downstream consumers (parse, highlight, row_starts, diag signs)
        // now read directly from the rope: no full-document String allocation.
        let rope = buffer.rope();

        // Get or build row_starts, cached per dirty_gen.
        // Scan newlines chunk-by-chunk from the rope so we never materialise
        // the full document as a contiguous byte slice.
        let row_starts: Arc<Vec<usize>> = if client
            .cache_row_starts
            .as_ref()
            .is_some_and(|(g, _)| *g == dg)
        {
            Arc::clone(&client.cache_row_starts.as_ref().unwrap().1)
        } else {
            // SIMD-vectorised newline scan via memchr — measurably faster than
            // a per-byte loop. Pre-sized to row_count + 1 to avoid realloc churn.
            let mut rs: Vec<usize> = Vec::with_capacity(row_count + 1);
            rs.push(0);
            let mut chunk_pos = 0usize;
            for chunk in rope.chunks() {
                for nl in memchr::memchr_iter(b'\n', chunk.as_bytes()) {
                    rs.push(chunk_pos + nl + 1);
                }
                chunk_pos += chunk.len();
            }
            let arc = Arc::new(rs);
            client.cache_row_starts = Some((dg, Arc::clone(&arc)));
            arc
        };

        // Reparse only when needed. Use rope-streaming parse to avoid passing
        // the full bytes slice into the parser (tree-sitter reads chunk-by-chunk
        // via the closure; no contiguous copy required for the parse step).
        let needs_reparse = client.parsed_dirty_gen != Some(dg);
        {
            let highlighter = client.highlighter.as_mut()?;
            if highlighter.tree().is_none() {
                highlighter.parse_initial_rope(&rope);
                if highlighter.tree().is_some() {
                    client.parsed_dirty_gen = Some(dg);
                }
            } else if needs_reparse {
                // No-diff incremental: we discard the changed-byte ranges
                // (cache is keyed by dirty_gen + viewport, not by edit
                // ranges). Computing `old.changed_ranges(&new)` walks both
                // trees and was ~54 % of per-keystroke CPU on a 1.86 M-line
                // file.
                let ok = highlighter.parse_incremental_rope(&rope);
                if ok && highlighter.tree().is_some() {
                    client.parsed_dirty_gen = Some(dg);
                }
            }
        }

        // Compute colorizer gate before re-borrowing client mutably.
        // Effective = global flag AND current language is in the allowlist.
        let colorizer_enabled = {
            let c = self.clients.get(&id)?;
            let lang_name = c.current_lang.as_ref().map(|g| g.name()).unwrap_or("");
            self.colorizer
                && (self.colorizer_filetypes.is_empty()
                    || self.colorizer_filetypes.iter().any(|ft| ft == lang_name))
        };
        let rainbow_brackets_enabled = self.rainbow_brackets;

        // Re-borrow after parse.
        let client = self.clients.get_mut(&id)?;
        let highlighter = client.highlighter.as_mut()?;

        // If still no tree (parse failed), give up.
        highlighter.tree()?;

        let theme = self.theme.as_ref();
        let directory = Arc::clone(&self.directory);

        // Extend cache to cover [vp_top, vp_end).
        if client.cache_rows.is_empty() {
            // Case A: empty cache — walk full range.
            client.cache_spans = walk_rows(
                highlighter,
                &rope,
                &row_starts,
                row_count,
                vp_top,
                vp_end,
                theme,
                &directory,
                colorizer_enabled,
                rainbow_brackets_enabled,
            );
            client.cache_rows = vp_top..vp_end;
            client.cache_dirty_gen = Some(dg);
        } else {
            let cache_covers_overlap =
                vp_top < client.cache_rows.end && vp_end > client.cache_rows.start;
            if !cache_covers_overlap {
                // Disjoint — just rebuild the whole viewport.
                client.cache_spans = walk_rows(
                    highlighter,
                    &rope,
                    &row_starts,
                    row_count,
                    vp_top,
                    vp_end,
                    theme,
                    &directory,
                    colorizer_enabled,
                    rainbow_brackets_enabled,
                );
                client.cache_rows = vp_top..vp_end;
            } else {
                // Case B: extend prefix if needed.
                if vp_top < client.cache_rows.start {
                    let new_rows = walk_rows(
                        highlighter,
                        &rope,
                        &row_starts,
                        row_count,
                        vp_top,
                        client.cache_rows.start,
                        theme,
                        &directory,
                        colorizer_enabled,
                        rainbow_brackets_enabled,
                    );
                    let mut combined = new_rows;
                    combined.append(&mut client.cache_spans);
                    client.cache_spans = combined;
                    client.cache_rows.start = vp_top;
                }
                // Case C: extend suffix if needed.
                if vp_end > client.cache_rows.end {
                    let new_rows = walk_rows(
                        highlighter,
                        &rope,
                        &row_starts,
                        row_count,
                        client.cache_rows.end,
                        vp_end,
                        theme,
                        &directory,
                        colorizer_enabled,
                        rainbow_brackets_enabled,
                    );
                    client.cache_spans.extend(new_rows);
                    client.cache_rows.end = vp_end;
                }
            }
            client.cache_dirty_gen = Some(dg);
        }

        // Slice the requested viewport from the cache.
        let offset = vp_top - client.cache_rows.start;
        let len = vp_end - vp_top;
        let spans: Vec<Vec<(usize, usize, StyleSpec)>> =
            client.cache_spans[offset..offset + len].to_vec();

        // Get or build signs, cached per (dirty_gen, vp_top, vp_end).
        let signs = if client
            .cache_signs
            .as_ref()
            .is_some_and(|(g, t, e, _)| *g == dg && *t == vp_top && *e == vp_end)
        {
            client.cache_signs.as_ref().unwrap().3.clone()
        } else {
            let s = collect_diag_signs_range(highlighter, &rope, &row_starts, vp_top, vp_end);
            client.cache_signs = Some((dg, vp_top, vp_end, s.clone()));
            s
        };

        Some(RenderOutput {
            buffer_id: id,
            spans,
            signs,
            key: (dg, vp_top, viewport_height),
            perf: PerfBreakdown::default(),
        })
    }

    /// Resolve a path to its language name without loading a grammar.
    pub fn name_for_path(&self, path: &Path) -> Option<String> {
        self.directory.name_for_path(path)
    }

    /// Returns `true` if a client is tracked for the given buffer id.
    #[doc(hidden)]
    pub fn has_client(&self, id: BufferId) -> bool {
        self.clients.contains_key(&id)
    }

    /// Dispatch a [`LoadEvent`] through a caller-supplied handler.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use hjkl_syntax::{LoadEvent, SyntaxLayer};
    ///
    /// let event = LoadEvent::Ready { id: 0, name: "rust".into() };
    /// let mut got_ready = false;
    /// let handled = SyntaxLayer::dispatch_load_event(&event, |ev| {
    ///     use hjkl_syntax::LoadEventKind;
    ///     match ev {
    ///         LoadEventKind::Ready { id, name } => { got_ready = true; }
    ///         LoadEventKind::Failed { .. } => {}
    ///     }
    /// });
    /// assert!(handled);
    /// assert!(got_ready);
    /// ```
    pub fn dispatch_load_event(
        event: &LoadEvent,
        mut handler: impl FnMut(LoadEventKind<'_>),
    ) -> bool {
        #[allow(unreachable_patterns)]
        match event {
            LoadEvent::Ready { id, name } => {
                handler(LoadEventKind::Ready { id: *id, name });
                true
            }
            LoadEvent::Failed { id, name, error } => {
                handler(LoadEventKind::Failed {
                    id: *id,
                    name,
                    error,
                });
                true
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Rainbow palette
// ---------------------------------------------------------------------------

/// 7-colour rainbow palette for bracket depth coloring (dark-bg readable).
/// Depth 0 → index 0, depth N → RAINBOW_PALETTE[N % RAINBOW_PALETTE.len()].
const RAINBOW_PALETTE: [Color; 7] = [
    Color::rgb(255, 100, 100), // red
    Color::rgb(255, 175, 80),  // orange
    Color::rgb(255, 230, 80),  // yellow
    Color::rgb(100, 220, 100), // green
    Color::rgb(80, 210, 220),  // cyan
    Color::rgb(100, 140, 255), // blue
    Color::rgb(190, 120, 255), // violet
];

// ---------------------------------------------------------------------------
// Helper: walk a row range against the retained tree
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn walk_rows(
    highlighter: &mut Highlighter,
    rope: &ropey::Rope,
    row_starts: &[usize],
    row_count: usize,
    seg_start: usize,
    seg_end: usize,
    theme: &dyn Theme,
    directory: &Arc<LanguageDirectory>,
    colorizer: bool,
    rainbow_brackets: bool,
) -> Vec<Vec<(usize, usize, StyleSpec)>> {
    let rope_len = rope.len_bytes();
    let byte_start = row_starts.get(seg_start).copied().unwrap_or(rope_len);
    let byte_end = row_starts
        .get(seg_end)
        .copied()
        .unwrap_or(rope_len)
        .min(rope_len)
        .max(byte_start);

    let mut flat_spans =
        highlighter.highlight_range_with_injections_rope(rope, byte_start..byte_end, |name| {
            directory.by_name(name)
        });

    let marker_pass = CommentMarkerPass::new();
    marker_pass.apply_rope(&mut flat_spans, rope);
    if colorizer {
        let hex_color_pass = HexColorPass::new();
        hex_color_pass.apply_range_rope(&mut flat_spans, rope, byte_start..byte_end);
    }
    if rainbow_brackets
        && let (Some(tree), Some(grammar)) = (highlighter.tree(), highlighter.grammar())
    {
        let rb_spans = rainbow_spans_rope(tree, grammar, rope, byte_start..byte_end);
        flat_spans.extend(rb_spans);
    }

    // Bucket spans into ONLY the viewport row range. The prior version
    // called `build_by_row(..., row_count, ...)` and sliced the result,
    // which allocated `row_count` empty inner Vecs (8.58 M on a huge
    // file) just to throw away all but ~50 of them — that single line
    // was ~24 % of per-keystroke CPU during a paste burst.
    let _ = row_count; // kept in signature for the public build_by_row tests
    build_by_row_range(&flat_spans, rope_len, row_starts, seg_start..seg_end, theme)
}

/// Viewport-bounded variant of [`build_by_row`]. Allocates exactly
/// `row_range.len()` inner Vecs instead of one per document row. Spans
/// whose byte range falls entirely outside `row_range` are skipped; spans
/// that overlap have their per-row slices recorded with positions local
/// to the viewport (so row `row_range.start` lands at index 0).
fn build_by_row_range(
    flat_spans: &[hjkl_bonsai::HighlightSpan],
    source_len: usize,
    row_starts: &[usize],
    row_range: Range<usize>,
    theme: &dyn Theme,
) -> Vec<Vec<(usize, usize, StyleSpec)>> {
    let seg_start = row_range.start;
    let seg_end = row_range.end.min(row_starts.len());
    if seg_end <= seg_start {
        return Vec::new();
    }
    let mut by_row: Vec<Vec<(usize, usize, StyleSpec)>> = vec![Vec::new(); seg_end - seg_start];

    for span in flat_spans {
        let hex_style: Option<StyleSpec> = if span.capture() == HEX_COLOR_CAPTURE {
            let bg = match span.metadata.get(HEX_BG_KEY) {
                Some(MetaValue::Str(s)) => hjkl_theme::Color::from_hex_str(s).ok(),
                _ => None,
            };
            let fg = match span.metadata.get(HEX_FG_KEY) {
                Some(MetaValue::Str(s)) => hjkl_theme::Color::from_hex_str(s).ok(),
                _ => None,
            };
            bg.map(|bg| StyleSpec {
                fg,
                bg: Some(bg),
                modifiers: hjkl_theme::Modifiers::default(),
            })
        } else if span.capture() == RAINBOW_BRACKET_CAPTURE {
            let depth = match span.metadata.get(RAINBOW_DEPTH_KEY) {
                Some(MetaValue::Int(d)) => *d as usize,
                _ => 0,
            };
            let fg = RAINBOW_PALETTE[depth % RAINBOW_PALETTE.len()];
            Some(StyleSpec {
                fg: Some(fg),
                bg: None,
                modifiers: hjkl_theme::Modifiers::default(),
            })
        } else {
            None
        };

        let style: StyleSpec = if let Some(s) = hex_style {
            s
        } else {
            match theme.style(span.capture()) {
                Some(s) => *s,
                None => continue,
            }
        };

        let span_start = span.byte_range.start;
        let span_end = span.byte_range.end;

        let start_row = row_starts
            .partition_point(|&rs| rs <= span_start)
            .saturating_sub(1);

        let mut row = start_row.max(seg_start);
        while row < seg_end {
            let row_byte_start = row_starts[row];
            let row_byte_end = row_starts
                .get(row + 1)
                .map(|&s| s.saturating_sub(1))
                .unwrap_or(source_len);

            if row_byte_start >= span_end {
                break;
            }

            let local_start = span_start.saturating_sub(row_byte_start);
            let local_end = span_end.min(row_byte_end) - row_byte_start;

            if local_end > local_start {
                by_row[row - seg_start].push((local_start, local_end, style));
            }

            row += 1;
        }
    }

    by_row
}

// ---------------------------------------------------------------------------
// Helper: build per-row span table (renderer-agnostic StyleSpec output)
// ---------------------------------------------------------------------------

/// Resolve flat highlight spans into a per-row span table sized to `row_count`.
pub fn build_by_row(
    flat_spans: &[hjkl_bonsai::HighlightSpan],
    bytes: &[u8],
    row_starts: &[usize],
    row_count: usize,
    theme: &dyn Theme,
) -> Vec<Vec<(usize, usize, StyleSpec)>> {
    let mut by_row: Vec<Vec<(usize, usize, StyleSpec)>> = vec![Vec::new(); row_count];

    for span in flat_spans {
        let hex_style: Option<StyleSpec> = if span.capture() == HEX_COLOR_CAPTURE {
            let bg = match span.metadata.get(HEX_BG_KEY) {
                Some(MetaValue::Str(s)) => hjkl_theme::Color::from_hex_str(s).ok(),
                _ => None,
            };
            let fg = match span.metadata.get(HEX_FG_KEY) {
                Some(MetaValue::Str(s)) => hjkl_theme::Color::from_hex_str(s).ok(),
                _ => None,
            };
            bg.map(|bg| StyleSpec {
                fg,
                bg: Some(bg),
                modifiers: hjkl_theme::Modifiers::default(),
            })
        } else if span.capture() == RAINBOW_BRACKET_CAPTURE {
            let depth = match span.metadata.get(RAINBOW_DEPTH_KEY) {
                Some(MetaValue::Int(d)) => *d as usize,
                _ => 0,
            };
            let fg = RAINBOW_PALETTE[depth % RAINBOW_PALETTE.len()];
            Some(StyleSpec {
                fg: Some(fg),
                bg: None,
                modifiers: hjkl_theme::Modifiers::default(),
            })
        } else {
            None
        };

        let style: StyleSpec = if let Some(s) = hex_style {
            s
        } else {
            match theme.style(span.capture()) {
                Some(s) => *s,
                None => continue,
            }
        };
        let style = &style;

        let span_start = span.byte_range.start;
        let span_end = span.byte_range.end;

        let start_row = row_starts
            .partition_point(|&rs| rs <= span_start)
            .saturating_sub(1);

        let mut row = start_row;
        while row < row_count {
            let row_byte_start = row_starts[row];
            let row_byte_end = row_starts
                .get(row + 1)
                .map(|&s| s.saturating_sub(1))
                .unwrap_or(bytes.len());

            if row_byte_start >= span_end {
                break;
            }

            let local_start = span_start.saturating_sub(row_byte_start);
            let local_end = span_end.min(row_byte_end) - row_byte_start;

            if local_end > local_start {
                by_row[row].push((local_start, local_end, *style));
            }

            row += 1;
        }
    }

    by_row
}

// ---------------------------------------------------------------------------
// Helper: collect diagnostic signs
// ---------------------------------------------------------------------------

fn collect_diag_signs_range(
    h: &mut Highlighter,
    rope: &ropey::Rope,
    row_starts: &[usize],
    vp_top: usize,
    vp_end: usize,
) -> Vec<DiagSign> {
    let rope_len = rope.len_bytes();
    let byte_start = row_starts.get(vp_top).copied().unwrap_or(rope_len);
    let byte_end = row_starts.get(vp_end).copied().unwrap_or(rope_len);
    // parse_errors_range only needs the source bytes for harvesting error
    // node snippets in the message string. Materialise just the viewport
    // window (typically ≪ 100 KB) rather than the whole document.
    let window: String = if byte_start < byte_end && byte_end <= rope_len {
        rope.byte_slice(byte_start..byte_end).to_string()
    } else {
        String::new()
    };
    // Translate byte range into window-relative for parse_errors_range.
    let errors = h.parse_errors_range(window.as_bytes(), 0..(byte_end - byte_start));
    let mut signs: Vec<DiagSign> = Vec::new();
    let mut last_row: Option<usize> = None;
    for err in &errors {
        // Translate window-relative back to absolute.
        let abs_start = err.byte_range.start + byte_start;
        let r = row_starts
            .partition_point(|&rs| rs <= abs_start)
            .saturating_sub(1);
        if last_row == Some(r) {
            continue;
        }
        last_row = Some(r);
        signs.push(DiagSign::new(r, 'E', 100));
    }
    signs
}

// ---------------------------------------------------------------------------
// Factory helpers
// ---------------------------------------------------------------------------

/// Build a `SyntaxLayer` using the given theme + language directory.
pub fn layer_with_theme(
    theme: Arc<DotFallbackTheme>,
    directory: Arc<LanguageDirectory>,
) -> SyntaxLayer {
    SyntaxLayer::new(theme, directory)
}

/// Build a `SyntaxLayer` with hjkl-bonsai's bundled dark theme.
#[cfg(test)]
pub fn default_layer() -> SyntaxLayer {
    let directory = Arc::new(LanguageDirectory::new().expect("language directory"));
    SyntaxLayer::new(Arc::new(DotFallbackTheme::dark()), directory)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_buffer::Buffer;
    use std::path::Path;

    const TID: BufferId = 0;

    // --- DiagSign ---

    #[test]
    fn diag_sign_new_roundtrip() {
        let s = DiagSign::new(7, 'W', 50);
        assert_eq!(s.row, 7);
        assert_eq!(s.ch, 'W');
        assert_eq!(s.priority, 50);
    }

    #[test]
    fn diag_sign_default_is_sensible() {
        let s = DiagSign::default();
        assert_eq!(s.row, 0);
        assert_eq!(s.ch, 'E');
        assert_eq!(s.priority, 0);
    }

    // --- PerfBreakdown ---

    #[test]
    fn perf_breakdown_default_zeros() {
        let p = PerfBreakdown::new();
        assert_eq!(p.source_build_us, 0);
        assert_eq!(p.parse_us, 0);
        assert_eq!(p.highlight_us, 0);
        assert_eq!(p.by_row_us, 0);
        assert_eq!(p.diag_us, 0);
    }

    // --- SetLanguageOutcome ---

    #[test]
    fn set_language_outcome_is_known() {
        assert!(SetLanguageOutcome::Ready.is_known());
        assert!(SetLanguageOutcome::Loading("rust".to_string()).is_known());
        assert!(!SetLanguageOutcome::Unknown.is_known());
    }

    // --- RenderOutput ---

    #[test]
    fn render_output_new_roundtrip() {
        let out = RenderOutput::new(
            99,
            vec![vec![]],
            vec![DiagSign::new(0, 'E', 100)],
            (7, 0, 30),
            PerfBreakdown::new(),
        );
        assert_eq!(out.buffer_id, 99);
        assert_eq!(out.key, (7, 0, 30));
        assert_eq!(out.signs.len(), 1);
    }

    #[test]
    fn render_output_partial_eq_same() {
        let a = RenderOutput::new(
            0,
            vec![vec![(0, 5, StyleSpec::default())]],
            vec![],
            (1, 0, 10),
            PerfBreakdown::default(),
        );
        let b = a.clone();
        assert_eq!(a, b);
    }

    // --- build_by_row ---

    #[test]
    fn build_by_row_empty_spans_gives_empty_rows() {
        let by_row = build_by_row(
            &[],
            b"hello\nworld\n",
            &[0, 6, 12],
            2,
            &DotFallbackTheme::dark(),
        );
        assert_eq!(by_row.len(), 2);
        assert!(by_row[0].is_empty());
        assert!(by_row[1].is_empty());
    }

    #[test]
    fn build_by_row_hex_color_uses_metadata_colors() {
        let bytes = b"--accent: #bb9af7;";
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            HEX_BG_KEY.to_string(),
            MetaValue::Str("#bb9af7".to_string()),
        );
        metadata.insert(
            HEX_FG_KEY.to_string(),
            MetaValue::Str("#ffffff".to_string()),
        );
        let span = hjkl_bonsai::HighlightSpan {
            byte_range: 10..17,
            capture: HEX_COLOR_CAPTURE.to_string(),
            metadata,
        };
        let by_row = build_by_row(&[span], bytes, &[0], 1, &DotFallbackTheme::dark());
        assert_eq!(by_row.len(), 1);
        assert_eq!(by_row[0].len(), 1);
        let (_, _, style) = by_row[0][0];
        let bg = style.bg.expect("hex color must set background");
        assert_eq!((bg.r, bg.g, bg.b), (0xbb, 0x9a, 0xf7));
        let fg = style.fg.expect("hex color must set foreground");
        assert_eq!((fg.r, fg.g, fg.b), (0xff, 0xff, 0xff));
    }

    #[test]
    fn build_by_row_hex_color_without_metadata_skips() {
        let span = hjkl_bonsai::HighlightSpan {
            byte_range: 0..3,
            capture: HEX_COLOR_CAPTURE.to_string(),
            metadata: std::collections::HashMap::new(),
        };
        let by_row = build_by_row(&[span], b"foo", &[0], 1, &DotFallbackTheme::dark());
        assert_eq!(by_row.len(), 1);
        assert!(by_row[0].is_empty());
    }

    // --- SyntaxLayer basics (no network required) ---

    #[test]
    fn render_viewport_with_no_language_returns_none() {
        let buf = Buffer::from_str("hello world");
        let mut layer = default_layer();
        assert!(
            !layer
                .set_language_for_path(TID, Path::new("a.unknownext"))
                .is_known()
        );
        assert!(layer.render_viewport(TID, &buf, 0, 10).is_none());
    }

    #[test]
    fn apply_edits_with_no_language_is_noop() {
        let mut layer = default_layer();
        let edits = vec![hjkl_engine::ContentEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 1,
            start_position: (0, 0),
            old_end_position: (0, 0),
            new_end_position: (0, 1),
        }];
        layer.apply_edits(TID, &edits);
        // No grammar attached → call must be a no-op (no panic).
    }

    #[test]
    fn set_language_for_path_returns_unknown_for_unrecognized_extension() {
        let mut layer = default_layer();
        let outcome = layer.set_language_for_path(TID, Path::new("a.zzznope_not_real"));
        assert!(!outcome.is_known());
        assert!(matches!(outcome, SetLanguageOutcome::Unknown));
    }

    #[test]
    fn poll_pending_loads_drains_ready_handles() {
        let mut layer = default_layer();
        let events = layer.poll_pending_loads();
        assert!(
            events.is_empty(),
            "expected no events with no pending loads"
        );
    }

    #[test]
    fn forget_removes_client_state() {
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.zzz_unknown"));
        layer.forget(TID);
        assert!(!layer.clients.contains_key(&TID));
    }

    // --- Network-dependent tests (grammar needed) ---

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn parse_and_render_small_rust_buffer() {
        let buf = Buffer::from_str("fn main() { let x = 1; }\n");
        let mut layer = default_layer();
        assert!(
            layer
                .set_language_for_path(TID, Path::new("a.rs"))
                .is_known()
        );
        let out = layer
            .render_viewport(TID, &buf, 0, 10)
            .expect("render output");
        assert!(
            out.spans.iter().any(|r| !r.is_empty()),
            "expected at least one styled span"
        );
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn diagnostics_emit_sign_for_syntax_error() {
        let buf = Buffer::from_str("fn main() {\nlet x = ;\n}\n");
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.rs"));
        let out = layer.render_viewport(TID, &buf, 0, 10).unwrap();
        assert!(
            !out.signs.is_empty(),
            "expected at least one diagnostic sign for `let x = ;`"
        );
        assert!(
            out.signs.iter().any(|s| s.row == 1 && s.ch == 'E'),
            "expected an 'E' sign on row 1; got {:?}",
            out.signs
        );
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn incremental_path_matches_cold_for_small_edit() {
        let pre = Buffer::from_str("fn main() { let x = 1; }");
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.rs"));
        let _ = layer.render_viewport(TID, &pre, 0, 10).unwrap();
        layer.apply_edits(
            TID,
            &[hjkl_engine::ContentEdit {
                start_byte: 3,
                old_end_byte: 3,
                new_end_byte: 4,
                start_position: (0, 3),
                old_end_position: (0, 3),
                new_end_position: (0, 4),
            }],
        );
        let post = Buffer::from_str("fn Ymain() { let x = 1; }");
        let inc = layer.render_viewport(TID, &post, 0, 10).unwrap();
        let mut cold_layer = default_layer();
        cold_layer.set_language_for_path(TID, Path::new("a.rs"));
        let cold = cold_layer.render_viewport(TID, &post, 0, 10).unwrap();
        assert_eq!(inc.spans, cold.spans);
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn forget_drops_buffer_state() {
        let buf = Buffer::from_str("fn main() {}");
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.rs"));
        let _ = layer.render_viewport(TID, &buf, 0, 10).unwrap();
        assert!(layer.clients.contains_key(&TID));
        layer.forget(TID);
        assert!(!layer.clients.contains_key(&TID));
    }
}
