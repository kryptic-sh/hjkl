//! Renderer-agnostic syntax-highlighting pipeline for the hjkl editor stack.
//!
//! Owns a [`SyntaxWorker`] background thread (holding the `Highlighter`
//! and retained `tree_sitter::Tree`) plus a main-thread `RenderCache` of
//! `(source, row_starts)`. Call
//! [`SyntaxLayer::set_language_for_path`] after opening a file, then
//! [`SyntaxLayer::apply_edits`] for each frame's queued
//! [`hjkl_engine::ContentEdit`] batch and [`SyntaxLayer::submit_render`]
//! to enqueue a parse + viewport-scoped highlight on the worker. Drain
//! results via [`SyntaxLayer::take_all_results`] each frame and route them
//! to the correct per-slot cache field.
//!
//! # Design
//!
//! Output is renderer-agnostic: [`RenderOutput::spans`] carries
//! `(byte_start, byte_end, `[`StyleSpec`]`)` triples rather than
//! renderer-specific style types.  A TUI adapter ([`hjkl-syntax-tui`]) maps
//! these to `ratatui::style::Style`; a future GUI adapter will map them to
//! `cosmic_text` attributes.
//!
//! [`StyleSpec`]: hjkl_theme::StyleSpec

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use hjkl_bonsai::runtime::{Grammar, LoadHandle};
use hjkl_bonsai::{
    CommentMarkerPass, DotFallbackTheme, HEX_BG_KEY, HEX_COLOR_CAPTURE, HEX_FG_KEY, HexColorPass,
    Highlighter, InputEdit, MetaValue, Point, Theme,
};
use hjkl_engine::Query;

use hjkl_lang::{GrammarRequest, LanguageDirectory};

pub use hjkl_theme::{Color, Modifiers, StyleSpec};

/// Stable identifier for an open buffer. Assigned by the App; carried
/// through every syntax-pipeline message so the worker can multiplex
/// per-buffer tree state.
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

/// Discriminates the purpose of a parse request / result so the App can
/// route it to the correct per-slot cache field.
///
/// - `Viewport` — the current visible region (already existed; default).
/// - `Top` — rows `0..min(3*h, line_count)` pre-cached so `gg` never
///   flashes un-highlighted rows.
/// - `Bottom` — rows `line_count - min(3*h, line_count)..line_count`
///   pre-cached so `G` never flashes un-highlighted rows.
///
/// **Ordering is load-bearing for the perf invariant:** the worker queue
/// is FIFO, so submitting `Viewport` first, then `Top`, then `Bottom`
/// ensures the retained tree is built on the Viewport pass and the
/// subsequent passes ride it incrementally (~1-5 ms each).
///
/// # Examples
///
/// ```
/// use hjkl_syntax::ParseKind;
/// assert_ne!(ParseKind::Viewport, ParseKind::Top);
/// assert_ne!(ParseKind::Top, ParseKind::Bottom);
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseKind {
    /// The current visible viewport region.
    Viewport,
    /// The top of the document (rows 0..N) — pre-cached for `gg`.
    Top,
    /// The bottom of the document (rows line_count-N..line_count) — pre-cached for `G`.
    Bottom,
}

/// A single diagnostic sign emitted from the syntax pipeline.
///
/// Carries only renderer-agnostic fields: `row`, `ch`, and `priority`.
/// The TUI adapter converts these to ratatui-styled `hjkl_buffer::Sign`
/// objects using its own colour choices.
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
    /// assert_eq!(s.ch, 'E');
    /// assert_eq!(s.priority, 100);
    /// ```
    pub fn new(row: usize, ch: char, priority: u8) -> Self {
        Self { row, ch, priority }
    }
}

/// Per-call sub-step timings exposed to apps' `:perf` overlay.
/// Recorded on the worker side and shipped back inside [`RenderOutput`].
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

/// Per-frame output of the syntax worker.
///
/// Contains the styled span table (one inner `Vec` per document row), the
/// diagnostic signs for the gutter, the cache key the request was tagged
/// with, and a [`PerfBreakdown`] describing where the worker spent its time.
///
/// Spans use [`StyleSpec`] (renderer-agnostic). The TUI adapter
/// ([`hjkl-syntax-tui`]) converts these to `ratatui::style::Style`.
///
/// # Examples
///
/// ```
/// use hjkl_syntax::{RenderOutput, ParseKind, PerfBreakdown};
/// let out = RenderOutput::new(0, Vec::new(), Vec::new(), (0, 0, 0), PerfBreakdown::default(), ParseKind::Viewport);
/// assert_eq!(out.kind, ParseKind::Viewport);
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RenderOutput {
    /// Routes spans/signs back to the matching buffer slot in App::slots.
    /// Install path discards the result when this doesn't match the now-active
    /// buffer (race fix: a parse queued before a tab/buffer switch must not
    /// paint onto the new active buffer).
    pub buffer_id: BufferId,
    /// Per-row span table. Each inner `Vec` contains `(byte_start, byte_end,
    /// StyleSpec)` triples for the characters on that row. The outer index is
    /// the document row (0-indexed). The table is sized to `row_count` even
    /// when only a viewport slice was requested — rows outside the viewport
    /// have empty inner Vecs.
    pub spans: Vec<Vec<(usize, usize, StyleSpec)>>,
    /// Diagnostic signs for the gutter (one per row with a tree-sitter
    /// ERROR / MISSING node intersecting the viewport).
    pub signs: Vec<DiagSign>,
    /// `(dirty_gen, viewport_top, viewport_height)` — same shape the App
    /// uses for its own cache key. Pair the result with this on receive.
    pub key: (u64, usize, usize),
    /// Sub-step timing breakdown from the worker.
    pub perf: PerfBreakdown,
    /// Which region this result covers — used by the install path to route
    /// into the correct per-slot cache field.
    pub kind: ParseKind,
}

impl RenderOutput {
    /// Construct a new `RenderOutput`.
    ///
    /// # Examples
    ///
    /// ```
    /// use hjkl_syntax::{RenderOutput, ParseKind, PerfBreakdown};
    /// let out = RenderOutput::new(1, Vec::new(), Vec::new(), (7, 0, 30), PerfBreakdown::new(), ParseKind::Top);
    /// assert_eq!(out.buffer_id, 1);
    /// assert_eq!(out.kind, ParseKind::Top);
    /// ```
    pub fn new(
        buffer_id: BufferId,
        spans: Vec<Vec<(usize, usize, StyleSpec)>>,
        signs: Vec<DiagSign>,
        key: (u64, usize, usize),
        perf: PerfBreakdown,
        kind: ParseKind,
    ) -> Self {
        Self {
            buffer_id,
            spans,
            signs,
            key,
            perf,
            kind,
        }
    }
}

impl PartialEq for RenderOutput {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.spans == other.spans
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
/// Callers that previously tested the return value as a `bool` should use
/// `outcome.is_known()` for equivalent behaviour.
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
    /// Grammar was already cached (or found fresh on disk) — installed
    /// immediately. Buffer will highlight on the next render.
    Ready,
    /// Grammar is being fetched/compiled on the background pool. Buffer
    /// renders as plain text until [`SyntaxLayer::poll_pending_loads`] fires
    /// the `Ready` event for this buffer. The inner `String` is the language
    /// name (useful for status-line indicators).
    Loading(#[allow(dead_code)] String),
    /// Extension unrecognized. No grammar — plain text only.
    Unknown,
}

impl SetLanguageOutcome {
    /// `true` when a grammar was found (either already cached or now in
    /// flight). Drop-in replacement for the old `bool` return value.
    pub fn is_known(&self) -> bool {
        matches!(self, Self::Ready | Self::Loading(_))
    }
}

/// Event emitted by [`SyntaxLayer::poll_pending_loads`] for each handle that
/// resolved during the tick.
///
/// # Examples
///
/// ```
/// use hjkl_syntax::LoadEvent;
/// let e = LoadEvent::Ready { id: 0, name: "rust".into() };
/// match e {
///     LoadEvent::Ready { id, name } => assert_eq!(name, "rust"),
///     LoadEvent::Failed { .. } => panic!("unexpected"),
///     // LoadEvent is #[non_exhaustive] — handle future variants.
///     _ => {}
/// }
/// ```
#[non_exhaustive]
pub enum LoadEvent {
    /// Grammar installed; trigger a redraw + re-submit for `id`.
    Ready {
        /// The buffer id the grammar was loaded for.
        id: BufferId,
        /// The resolved language name (e.g. `"rust"`).
        name: String,
    },
    /// Load failed (clone/compile error); buffer stays plain text.
    Failed {
        /// The buffer id the grammar was loaded for.
        id: BufferId,
        /// The resolved language name.
        name: String,
        /// Human-readable error message.
        error: String,
    },
}

/// Exhaustive view of a [`LoadEvent`] for use in
/// [`SyntaxLayer::dispatch_load_event`] callbacks.
///
/// Unlike [`LoadEvent`] (which is `#[non_exhaustive]`), matching on this enum
/// requires no wildcard arm and produces a compile error when new variants are
/// added.
#[derive(Debug)]
pub enum LoadEventKind<'a> {
    /// Grammar installed successfully.
    Ready {
        /// The buffer id the grammar was loaded for.
        id: BufferId,
        /// The resolved language name.
        name: &'a str,
    },
    /// Grammar load failed.
    Failed {
        /// The buffer id the grammar was loaded for.
        id: BufferId,
        /// The resolved language name.
        name: &'a str,
        /// Human-readable error message.
        error: &'a str,
    },
}

/// Exhaustive view of a [`ParseKind`] for use in
/// [`SyntaxLayer::dispatch_parse_kind`] callbacks.
///
/// Unlike [`ParseKind`] (which is `#[non_exhaustive]`), matching on this enum
/// requires no wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseKindKind {
    /// The current visible viewport region.
    Viewport,
    /// The top of the document — pre-cached for `gg`.
    Top,
    /// The bottom of the document — pre-cached for `G`.
    Bottom,
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

/// Cached `(source, row_starts)` keyed off buffer identity (dirty_gen +
/// shape). Built once per buffer mutation on the **main** thread and
/// shipped to the worker as `Arc`s so the worker doesn't memcpy a 1.3 MB
/// Rust file for every parse request.
struct RenderCache {
    dirty_gen: u64,
    len_bytes: usize,
    line_count: u32,
    source: Arc<String>,
    row_starts: Arc<Vec<usize>>,
}

/// A parse + render job submitted to the worker. The worker owns the
/// retained tree, applies any queued `InputEdit`s, reparses, runs the
/// viewport highlight + error scan, builds the per-row span table, and
/// sends the result back via mpsc.
struct ParseRequest {
    buffer_id: BufferId,
    source: Arc<String>,
    row_starts: Arc<Vec<usize>>,
    edits: Vec<InputEdit>,
    viewport_byte_range: std::ops::Range<usize>,
    viewport_top: usize,
    viewport_height: usize,
    row_count: usize,
    dirty_gen: u64,
    /// When `true` the worker drops its retained tree before parsing.
    /// Used after `:e` reload / theme swap so the next parse is cold.
    reset: bool,
    kind: ParseKind,
}

/// Control + data messages the worker thread waits on.
enum Msg {
    /// Set / replace the highlighter for a buffer. `None` detaches.
    SetLanguage(BufferId, Option<Arc<Grammar>>),
    /// Remove all worker state for a buffer (highlighter, retained tree,
    /// parse-cache key). Sent on buffer close.
    Forget(BufferId),
    /// Replace the theme. Style resolution happens on the worker.
    SetTheme(Arc<dyn Theme + Send + Sync>),
    /// A parse + render job. Coalesced — only the latest pending
    /// `Parse` survives if the worker is busy.
    Parse(ParseRequest),
    /// Worker should exit. Sent on `SyntaxWorker::drop`.
    Quit,
}

/// Maximum number of parse requests allowed in the queue at once.
const PARSE_QUEUE_CAP: usize = 8;

/// Shared slot the main thread drops new requests into.
struct Pending {
    /// FIFO of parse requests. Per-(buffer_id, kind) deduped: submitting a
    /// request for buffer A + kind Viewport replaces any existing entry for
    /// that pair rather than appending. Capped at [`PARSE_QUEUE_CAP`] total entries.
    parse_queue: std::collections::VecDeque<ParseRequest>,
    /// FIFO of control messages (SetLanguage, Forget, SetTheme, Quit).
    controls: std::collections::VecDeque<Msg>,
}

impl Pending {
    fn new() -> Self {
        Self {
            parse_queue: std::collections::VecDeque::new(),
            controls: std::collections::VecDeque::new(),
        }
    }

    fn has_work(&self) -> bool {
        !self.parse_queue.is_empty() || !self.controls.is_empty()
    }

    /// Enqueue a parse request with per-`(buffer_id, kind)` deduplication.
    ///
    /// - If a request for the same `(buffer_id, kind)` pair is already in
    ///   the queue, replace it in-place (latest wins). Requests with the
    ///   same buffer_id but different kinds (Viewport / Top / Bottom)
    ///   coexist in the queue so all three regions can be pre-cached.
    /// - If the queue is at capacity and there is no existing entry for
    ///   this `(buffer_id, kind)`, evict the oldest entry before pushing.
    fn push_parse(&mut self, mut req: ParseRequest) {
        for slot in self.parse_queue.iter_mut() {
            if slot.buffer_id == req.buffer_id && slot.kind == req.kind {
                // Merge: the existing slot's edits MUST survive the replace
                // — they're tree-sitter `Tree::edit` deltas the worker still
                // needs to apply before its retained tree matches the new
                // source. Dropping them leaves the tree at a wrong byte
                // baseline and every subsequent highlight returns spans
                // with offsets matching a buffer state that no longer
                // exists, producing visibly shifted / misaligned spans
                // that don't recover until the next bulk parse (e.g.
                // forced by a `take_content_reset`).
                let mut merged = std::mem::take(&mut slot.edits);
                merged.append(&mut req.edits);
                req.edits = merged;
                *slot = req;
                return;
            }
        }
        if self.parse_queue.len() >= PARSE_QUEUE_CAP {
            self.parse_queue.pop_front();
        }
        self.parse_queue.push_back(req);
    }
}

// ---------------------------------------------------------------------------
// SyntaxWorker — background thread
// ---------------------------------------------------------------------------

/// Background worker that owns the `Highlighter` and the retained
/// tree-sitter `Tree`. Communicates with the main thread via a
/// `Mutex<Pending>` + `Condvar` for submits, and an mpsc channel for
/// rendered output.
///
/// # Examples
///
/// ```no_run
/// use std::sync::Arc;
/// use hjkl_syntax::SyntaxWorker;
/// use hjkl_bonsai::DotFallbackTheme;
/// use hjkl_lang::LanguageDirectory;
///
/// let theme = Arc::new(DotFallbackTheme::dark());
/// let dir = Arc::new(LanguageDirectory::new().unwrap());
/// let worker = SyntaxWorker::spawn(theme, dir);
/// drop(worker); // joins the thread
/// ```
pub struct SyntaxWorker {
    pending: Arc<(Mutex<Pending>, Condvar)>,
    rx: std::sync::mpsc::Receiver<RenderOutput>,
    handle: Option<JoinHandle<()>>,
}

impl SyntaxWorker {
    /// Spawn a fresh worker thread with the given theme and language directory.
    pub fn spawn(theme: Arc<dyn Theme + Send + Sync>, directory: Arc<LanguageDirectory>) -> Self {
        let pending = Arc::new((Mutex::new(Pending::new()), Condvar::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let pending_for_thread = Arc::clone(&pending);
        let handle = thread::Builder::new()
            .name("hjkl-syntax".into())
            .spawn(move || worker_loop(pending_for_thread, tx, theme, directory))
            .expect("spawn syntax worker");
        Self {
            pending,
            rx,
            handle: Some(handle),
        }
    }

    /// Send a control message. Wakes the worker.
    fn enqueue_control(&self, msg: Msg) {
        let (lock, cvar) = &*self.pending;
        let mut p = lock.lock().expect("syntax pending mutex poisoned");
        p.controls.push_back(msg);
        cvar.notify_one();
    }

    /// Set / replace the highlighter for a buffer. `None` detaches.
    pub fn set_language(&self, id: BufferId, grammar: Option<Arc<Grammar>>) {
        self.enqueue_control(Msg::SetLanguage(id, grammar));
    }

    /// Forget all worker state for a buffer (highlighter + tree).
    /// Sent on buffer close.
    pub fn forget(&self, id: BufferId) {
        self.enqueue_control(Msg::Forget(id));
    }

    /// Replace the theme used for capture → style resolution.
    pub fn set_theme(&self, theme: Arc<dyn Theme + Send + Sync>) {
        self.enqueue_control(Msg::SetTheme(theme));
    }

    /// Submit a parse job. Per-`(buffer_id, kind)` deduplication: if a
    /// request for the same pair is already pending it is replaced in-place.
    /// Across different pairs the queue is FIFO. Returns immediately.
    fn submit(&self, req: ParseRequest) {
        let (lock, cvar) = &*self.pending;
        let mut p = lock.lock().expect("syntax pending mutex poisoned");
        p.push_parse(req);
        cvar.notify_one();
    }

    /// Drain all available render results, returning the most recent
    /// one. Earlier results are discarded — they'd just be overwritten
    /// by the latest install anyway, and this keeps the install path
    /// O(1) per frame regardless of backlog depth.
    #[allow(dead_code)]
    pub fn try_recv_latest(&self) -> Option<RenderOutput> {
        let mut latest: Option<RenderOutput> = None;
        while let Ok(out) = self.rx.try_recv() {
            latest = Some(out);
        }
        latest
    }

    /// Drain all available render results, returning them all (one per
    /// `(buffer_id, kind)` pair that completed). Unlike
    /// [`Self::try_recv_latest`] this does not discard earlier results —
    /// required so pre-warmed results for non-active buffers can be routed
    /// to the right slot cache.
    pub fn try_recv_all(&self) -> Vec<RenderOutput> {
        let mut results = Vec::new();
        while let Ok(out) = self.rx.try_recv() {
            if let Some(existing) = results
                .iter_mut()
                .find(|r: &&mut RenderOutput| r.buffer_id == out.buffer_id && r.kind == out.kind)
            {
                *existing = out;
            } else {
                results.push(out);
            }
        }
        results
    }

    /// Wait up to `timeout` for the next result, then drain anything
    /// else that arrived after it and return the latest.
    pub fn wait_for_latest(&self, timeout: std::time::Duration) -> Option<RenderOutput> {
        let mut latest = self.rx.recv_timeout(timeout).ok();
        while let Ok(out) = self.rx.try_recv() {
            latest = Some(out);
        }
        latest
    }

    /// Wait up to `timeout` for the first result to arrive, then drain
    /// every additional result already in the channel. Returns ALL
    /// results in arrival order (latest per `(buffer_id, kind)` coalesced).
    ///
    /// Unlike [`Self::wait_for_latest`] this does NOT discard earlier
    /// results — required when pre-warming non-active buffers so both the
    /// active buffer's result and pre-warm results reach their slot caches.
    pub fn wait_then_recv_all(&self, timeout: std::time::Duration) -> Vec<RenderOutput> {
        let mut results: Vec<RenderOutput> = Vec::new();
        if let Ok(first) = self.rx.recv_timeout(timeout) {
            results.push(first);
        }
        while let Ok(out) = self.rx.try_recv() {
            if let Some(existing) = results
                .iter_mut()
                .find(|r: &&mut RenderOutput| r.buffer_id == out.buffer_id && r.kind == out.kind)
            {
                *existing = out;
            } else {
                results.push(out);
            }
        }
        results
    }
}

impl Drop for SyntaxWorker {
    fn drop(&mut self) {
        {
            let (lock, cvar) = &*self.pending;
            if let Ok(mut p) = lock.lock() {
                p.controls.push_back(Msg::Quit);
                cvar.notify_one();
            }
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Worker-side per-buffer state
// ---------------------------------------------------------------------------

/// Per-buffer state retained on the worker side.
struct WorkerBufferState {
    highlighter: Highlighter,
    last_parsed_dirty_gen: Option<u64>,
}

// ---------------------------------------------------------------------------
// Worker loop
// ---------------------------------------------------------------------------

fn worker_loop(
    pending: Arc<(Mutex<Pending>, Condvar)>,
    tx: std::sync::mpsc::Sender<RenderOutput>,
    initial_theme: Arc<dyn Theme + Send + Sync>,
    directory: Arc<LanguageDirectory>,
) {
    use std::time::Instant;

    let mut buffers: HashMap<BufferId, WorkerBufferState> = HashMap::new();
    let mut theme: Arc<dyn Theme + Send + Sync> = initial_theme;
    let marker_pass = CommentMarkerPass::new();
    let hex_color_pass = HexColorPass::new();

    loop {
        let msg = {
            let (lock, cvar) = &*pending;
            let mut p = lock.lock().expect("syntax pending mutex poisoned");
            while !p.has_work() {
                p = cvar.wait(p).expect("syntax pending cvar poisoned");
            }
            // Drain controls first so SetLanguage / Forget / SetTheme
            // that arrived alongside a Parse get applied before we run
            // the parse with stale state.
            if let Some(c) = p.controls.pop_front() {
                c
            } else {
                Msg::Parse(
                    p.parse_queue
                        .pop_front()
                        .expect("has_work() implies parse_queue non-empty"),
                )
            }
        };

        match msg {
            Msg::Quit => return,
            Msg::SetLanguage(id, None) => {
                buffers.remove(&id);
            }
            Msg::SetLanguage(id, Some(grammar)) => {
                let lang = grammar.name().to_string();
                match Highlighter::new(grammar) {
                    Ok(h) => {
                        buffers.insert(
                            id,
                            WorkerBufferState {
                                highlighter: h,
                                last_parsed_dirty_gen: None,
                            },
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            buffer_id = id,
                            language = %lang,
                            error = %e,
                            "failed to attach syntax highlighter"
                        );
                        buffers.remove(&id);
                    }
                }
            }
            Msg::Forget(id) => {
                buffers.remove(&id);
            }
            Msg::SetTheme(t) => {
                theme = t;
            }
            Msg::Parse(req) => {
                let Some(state) = buffers.get_mut(&req.buffer_id) else {
                    continue;
                };
                let h = &mut state.highlighter;
                let mut perf = PerfBreakdown::default();
                if req.reset {
                    h.reset();
                    state.last_parsed_dirty_gen = None;
                }
                // Only (re-)parse if the retained tree does not already
                // represent this dirty_gen. Non-empty `edits` are applied
                // below when a parse IS needed; if the tree is already current
                // they are stale (a prior request for the same dirty_gen
                // already processed them) and must be discarded — applying
                // them again would corrupt the tree's node positions.
                let needs_parse =
                    h.tree().is_none() || state.last_parsed_dirty_gen != Some(req.dirty_gen);
                if needs_parse {
                    for e in &req.edits {
                        h.edit(e);
                    }
                    let bytes = req.source.as_bytes();
                    let t = Instant::now();
                    let parsed_ok = if h.tree().is_none() {
                        h.parse_initial(bytes);
                        true
                    } else {
                        h.parse_incremental(bytes)
                    };
                    if !parsed_ok {
                        continue;
                    }
                    perf.parse_us = t.elapsed().as_micros();
                    state.last_parsed_dirty_gen = Some(req.dirty_gen);
                }
                let bytes = req.source.as_bytes();

                let t = Instant::now();
                let mut flat_spans = h.highlight_range_with_injections(
                    bytes,
                    req.viewport_byte_range.clone(),
                    |name| directory.by_name(name),
                );
                perf.highlight_us = t.elapsed().as_micros();

                // Overlay TODO/FIXME/NOTE/WARN marker spans onto comment spans.
                marker_pass.apply(&mut flat_spans, bytes);

                // Inline hex-color preview overlay (#rgb / #rrggbb).
                hex_color_pass.apply(&mut flat_spans, bytes);

                let t = Instant::now();
                let by_row = build_by_row(
                    &flat_spans,
                    bytes,
                    &req.row_starts,
                    req.row_count,
                    theme.as_ref(),
                );
                perf.by_row_us = t.elapsed().as_micros();

                let t = Instant::now();
                let signs = collect_diag_signs(h, bytes, req.viewport_byte_range, &req.row_starts);
                perf.diag_us = t.elapsed().as_micros();

                let key = (req.dirty_gen, req.viewport_top, req.viewport_height);
                let _ = tx.send(RenderOutput {
                    buffer_id: req.buffer_id,
                    spans: by_row,
                    signs,
                    key,
                    perf,
                    kind: req.kind,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: build per-row span table (renderer-agnostic StyleSpec output)
// ---------------------------------------------------------------------------

/// Resolve flat highlight spans into a per-row span table sized to
/// `row_count`. Pulled out so the worker can call it in isolation and tests
/// can exercise it without a running thread.
///
/// Output: `Vec<Vec<(byte_start, byte_end, StyleSpec)>>` indexed by row.
pub fn build_by_row(
    flat_spans: &[hjkl_bonsai::HighlightSpan],
    bytes: &[u8],
    row_starts: &[usize],
    row_count: usize,
    theme: &dyn Theme,
) -> Vec<Vec<(usize, usize, StyleSpec)>> {
    let mut by_row: Vec<Vec<(usize, usize, StyleSpec)>> = vec![Vec::new(); row_count];

    for span in flat_spans {
        // Hex-color preview overlay: bypass the theme and build a
        // StyleSpec directly from the metadata that HexColorPass
        // attached. `hex_color` is intentionally NOT a theme key —
        // the colour comes from the source literal itself.
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

/// Collect diagnostic [`DiagSign`]s from tree-sitter ERROR / MISSING nodes
/// intersecting the viewport, deduped to one per row.
fn collect_diag_signs(
    h: &mut Highlighter,
    bytes: &[u8],
    viewport_byte_range: std::ops::Range<usize>,
    row_starts: &[usize],
) -> Vec<DiagSign> {
    let errors = h.parse_errors_range(bytes, viewport_byte_range);
    let mut signs: Vec<DiagSign> = Vec::new();
    let mut last_row: Option<usize> = None;
    for err in &errors {
        let r = row_starts
            .partition_point(|&rs| rs <= err.byte_range.start)
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
// Per-buffer client state (main thread)
// ---------------------------------------------------------------------------

/// Per-buffer client-side state. One of these per open buffer in
/// `SyntaxLayer.clients`. Mirrors the worker's `WorkerBufferState` but
/// holds the source-cache + edit queue, which live on the main thread.
#[derive(Default)]
struct BufferClient {
    has_language: bool,
    current_lang: Option<Arc<Grammar>>,
    cache: Option<RenderCache>,
    pending_edits: Vec<InputEdit>,
    pending_reset: bool,
    last_submitted_dirty_gen: Option<u64>,
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
// SyntaxLayer — main-thread facade
// ---------------------------------------------------------------------------

/// Per-App syntax highlighting layer. Multiplexes per-buffer state
/// (helix-style): each open buffer carries its own retained tree
/// (worker-side) plus source-cache and edit queue (here). One worker
/// thread serves all buffers.
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
    /// Active theme.
    theme: Arc<dyn Theme + Send + Sync>,
    worker: SyntaxWorker,
    clients: HashMap<BufferId, BufferClient>,
    pending_loads: Vec<PendingLoad>,
    /// Per-grammar synchronous `Highlighter` cache used by [`Self::preview_render`].
    preview_highlighters: Mutex<HashMap<String, Highlighter>>,
    /// Last perf breakdown received via `take_all_results`. Updated on every
    /// successful drain.
    pub last_perf: PerfBreakdown,
}

impl SyntaxLayer {
    /// Create a new layer with no buffers attached, the given theme, and
    /// the given language directory.
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
    /// Borrow the shared language directory. Useful for cheap
    /// path-to-language-name lookups (e.g. `name_for_path`) without
    /// triggering any grammar load.
    pub fn directory(&self) -> &Arc<LanguageDirectory> {
        &self.directory
    }

    pub fn new(theme: Arc<dyn Theme + Send + Sync>, directory: Arc<LanguageDirectory>) -> Self {
        let worker = SyntaxWorker::spawn(Arc::clone(&theme), Arc::clone(&directory));
        Self {
            directory,
            theme,
            worker,
            clients: HashMap::new(),
            pending_loads: Vec::new(),
            preview_highlighters: Mutex::new(HashMap::new()),
            last_perf: PerfBreakdown::default(),
        }
    }

    fn client_mut(&mut self, id: BufferId) -> &mut BufferClient {
        self.clients.entry(id).or_default()
    }

    /// Detect the language for `path` and ship it to the worker.
    ///
    /// Non-blocking: uses the async grammar loader so opening a file with an
    /// uninstalled grammar no longer freezes the UI.
    ///
    /// - `Ready`   — grammar cached/found on disk; installed immediately.
    /// - `Loading` — clone+compile kicked off; buffer renders as plain text
    ///   until `poll_pending_loads` fires the companion `LoadEvent::Ready`.
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
                self.worker.set_language(id, Some(grammar.clone()));
                let c = self.client_mut(id);
                c.current_lang = Some(grammar);
                c.has_language = true;
                SetLanguageOutcome::Ready
            }
            GrammarRequest::Loading { name, handle } => {
                self.worker.set_language(id, None);
                let c = self.client_mut(id);
                c.current_lang = None;
                c.has_language = false;
                self.pending_loads.push(PendingLoad {
                    id,
                    name: name.clone(),
                    handle,
                });
                SetLanguageOutcome::Loading(name)
            }
            GrammarRequest::Unknown => {
                self.worker.set_language(id, None);
                let c = self.client_mut(id);
                c.current_lang = None;
                c.has_language = false;
                SetLanguageOutcome::Unknown
            }
            _ => {
                // Future GrammarRequest variants — treat as Unknown.
                self.worker.set_language(id, None);
                let c = self.client_mut(id);
                c.current_lang = None;
                c.has_language = false;
                SetLanguageOutcome::Unknown
            }
        }
    }

    /// Poll all in-flight grammar loads. Call this once per tick from the
    /// main loop (alongside `take_all_results`) so completed loads install
    /// immediately without waiting for the next file open.
    ///
    /// Returns one `LoadEvent` per handle that resolved during this tick.
    /// Non-empty results should trigger a redraw and re-submit render.
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
                            self.worker.set_language(bid, Some(grammar.clone()));
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
        self.worker.forget(id);
    }

    /// Swap the active theme. Next render call will use the new theme.
    pub fn set_theme(&mut self, theme: Arc<dyn Theme + Send + Sync>) {
        self.theme = Arc::clone(&theme);
        self.worker.set_theme(theme);
    }

    /// Synchronous viewport-only preview render. Builds a `String`
    /// containing **only** the visible rows, parses it from scratch with a
    /// one-shot `Highlighter`, runs `highlight_range` over the slice, and
    /// returns a `RenderOutput` whose `spans` table is padded with empty rows
    /// above the viewport so the install path indexes the right rows.
    ///
    /// Returns `None` when no language is attached or when the viewport is empty.
    pub fn preview_render(
        &self,
        id: BufferId,
        buffer: &impl Query,
        viewport_top: usize,
        viewport_height: usize,
    ) -> Option<RenderOutput> {
        let grammar = self.clients.get(&id).and_then(|c| c.current_lang.clone())?;
        let row_count = buffer.line_count() as usize;
        if row_count == 0 || viewport_height == 0 {
            return None;
        }
        let vp_top = viewport_top.min(row_count);
        let vp_end_row = (vp_top + viewport_height).min(row_count);
        if vp_end_row <= vp_top {
            return None;
        }

        let mut source = String::new();
        for r in vp_top..vp_end_row {
            if r > vp_top {
                source.push('\n');
            }
            source.push_str(&buffer.line(r as u32));
        }
        let bytes = source.as_bytes();
        let mut row_starts: Vec<usize> = vec![0];
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                row_starts.push(i + 1);
            }
        }
        let local_row_count = vp_end_row - vp_top;

        let grammar_name = grammar.name().to_string();
        let mut cache = self.preview_highlighters.lock().ok()?;
        let h = match cache.entry(grammar_name) {
            std::collections::hash_map::Entry::Occupied(o) => {
                let h = o.into_mut();
                h.reset();
                h
            }
            std::collections::hash_map::Entry::Vacant(v) => match Highlighter::new(grammar) {
                Ok(h) => v.insert(h),
                Err(_) => return None,
            },
        };
        let mut flat_spans =
            h.highlight_with_injections(bytes, |name| self.directory.by_name(name));
        drop(cache);

        let marker_pass = CommentMarkerPass::new();
        marker_pass.apply(&mut flat_spans, bytes);
        let hex_color_pass = HexColorPass::new();
        hex_color_pass.apply(&mut flat_spans, bytes);

        let local_by_row = build_by_row(
            &flat_spans,
            bytes,
            &row_starts,
            local_row_count,
            self.theme.as_ref(),
        );

        let mut spans: Vec<Vec<(usize, usize, StyleSpec)>> = vec![Vec::new(); vp_top];
        spans.extend(local_by_row);

        Some(RenderOutput {
            buffer_id: id,
            spans,
            signs: Vec::new(),
            key: (buffer.dirty_gen(), viewport_top, viewport_height),
            perf: PerfBreakdown::default(),
            kind: ParseKind::Viewport,
        })
    }

    /// Ask the worker to drop this buffer's retained tree on the next
    /// parse so the next submission is cold.
    pub fn reset(&mut self, id: BufferId) {
        self.client_mut(id).pending_reset = true;
    }

    /// Buffer a batch of engine `ContentEdit`s to be shipped to the
    /// worker on the next `submit_render`. Translates the engine's
    /// position pairs into `tree_sitter::InputEdit`s up front.
    pub fn apply_edits(&mut self, id: BufferId, edits: &[hjkl_engine::ContentEdit]) {
        let c = self.client_mut(id);
        if !c.has_language {
            return;
        }
        for e in edits {
            c.pending_edits.push(InputEdit {
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
    }

    /// Build (or reuse) the cached `(source, row_starts)` for the
    /// current buffer state and submit a parse + render job to the worker.
    /// Returns immediately. Drain the result with [`Self::take_all_results`].
    ///
    /// `kind` tags the request so the App can route the result to the correct
    /// per-slot cache field. Pass [`ParseKind::Viewport`] for normal
    /// scroll-driven parses.
    ///
    /// Returns `None` and submits nothing when no language is attached.
    /// Returns `Some(source_build_us)` when a request was submitted — `0`
    /// means the cache was reused.
    pub fn submit_render(
        &mut self,
        id: BufferId,
        buffer: &impl Query,
        viewport_top: usize,
        viewport_height: usize,
        kind: ParseKind,
    ) -> Option<u128> {
        use std::time::Instant;
        let c = self.client_mut(id);
        if !c.has_language {
            return None;
        }

        let dg = buffer.dirty_gen();
        let lb = buffer.len_bytes();
        let lc = buffer.line_count();
        let row_count = lc as usize;

        // Rebuild source + row_starts only when the buffer has changed.
        let needs_rebuild = match &c.cache {
            Some(rc) => rc.dirty_gen != dg || rc.len_bytes != lb || rc.line_count != lc,
            None => true,
        };
        let mut source_build_us = 0u128;
        if needs_rebuild {
            let t = Instant::now();
            let mut source = String::with_capacity(lb);
            for r in 0..row_count {
                if r > 0 {
                    source.push('\n');
                }
                source.push_str(&buffer.line(r as u32));
            }
            let mut row_starts: Vec<usize> = vec![0];
            for (i, &b) in source.as_bytes().iter().enumerate() {
                if b == b'\n' {
                    row_starts.push(i + 1);
                }
            }
            c.cache = Some(RenderCache {
                dirty_gen: dg,
                len_bytes: lb,
                line_count: lc,
                source: Arc::new(source),
                row_starts: Arc::new(row_starts),
            });
            source_build_us = t.elapsed().as_micros();
        }
        let cache = c.cache.as_ref().expect("cache populated above");

        let bytes_len = cache.source.len();
        let vp_start = buffer.byte_of_row(viewport_top);
        let vp_end_row = viewport_top + viewport_height + 1;
        let vp_end = buffer.byte_of_row(vp_end_row).min(bytes_len);
        let vp_end = vp_end.max(vp_start);

        let edits = std::mem::take(&mut c.pending_edits);
        let reset = std::mem::replace(&mut c.pending_reset, false);
        c.last_submitted_dirty_gen = Some(dg);
        let source_arc = Arc::clone(&cache.source);
        let row_starts_arc = Arc::clone(&cache.row_starts);

        self.worker.submit(ParseRequest {
            buffer_id: id,
            source: source_arc,
            row_starts: row_starts_arc,
            edits,
            viewport_byte_range: vp_start..vp_end,
            viewport_top,
            viewport_height,
            row_count,
            dirty_gen: dg,
            reset,
            kind,
        });

        Some(source_build_us)
    }

    /// Drain the most recent render result the worker has produced (if any).
    /// Older results are discarded — only the latest matters for install.
    /// Updates `last_perf` as a side effect.
    ///
    /// Kept for use in perf-smoke tests; production code drains via
    /// [`Self::take_all_results`] so multi-buffer results are routed.
    #[allow(dead_code)]
    pub fn take_result(&mut self) -> Option<RenderOutput> {
        let out = self.worker.try_recv_latest()?;
        self.last_perf = out.perf;
        Some(out)
    }

    /// Drain all render results the worker has produced since the last drain
    /// (one per `(buffer_id, kind)` that completed). Updates `last_perf` from
    /// the last result if any are present.
    pub fn take_all_results(&mut self) -> Vec<RenderOutput> {
        let results = self.worker.try_recv_all();
        if let Some(last) = results.last() {
            self.last_perf = last.perf;
        }
        results
    }

    /// Block up to `timeout` for the worker's next result, then drain any
    /// others that arrived after it.
    pub fn wait_result(&mut self, timeout: std::time::Duration) -> Option<RenderOutput> {
        let out = self.worker.wait_for_latest(timeout)?;
        self.last_perf = out.perf;
        Some(out)
    }

    /// Block up to `timeout` for the first result, then drain ALL available
    /// results in arrival order (per-`(buffer_id, kind)` coalesced). Used for
    /// big-jump paths that submit the active buffer's parse AND pre-warms for
    /// other open buffers in the same tick.
    pub fn wait_all_results(&mut self, timeout: std::time::Duration) -> Vec<RenderOutput> {
        let results = self.worker.wait_then_recv_all(timeout);
        if let Some(last) = results.last() {
            self.last_perf = last.perf;
        }
        results
    }

    /// Synchronously drain the next result, blocking up to `timeout`.
    /// Returns `None` on timeout. Used at startup so the very first frame
    /// can paint with highlights when the worker is fast enough.
    pub fn wait_for_initial_result(
        &mut self,
        timeout: std::time::Duration,
    ) -> Option<RenderOutput> {
        self.wait_result(timeout)
    }

    /// Test-only alias for [`Self::wait_for_initial_result`].
    #[cfg(test)]
    pub fn wait_for_result(&mut self, timeout: std::time::Duration) -> Option<RenderOutput> {
        self.wait_for_initial_result(timeout)
    }

    /// Returns `true` if a client is tracked for the given buffer id.
    /// Exposed for tests in consumer crates that wrap `SyntaxLayer`.
    #[doc(hidden)]
    pub fn has_client(&self, id: BufferId) -> bool {
        self.clients.contains_key(&id)
    }

    /// Returns the `pending_reset` flag for the given buffer id, or `false`
    /// if no client is tracked.
    /// Exposed for tests in consumer crates that wrap `SyntaxLayer`.
    #[doc(hidden)]
    pub fn client_pending_reset(&self, id: BufferId) -> bool {
        self.clients
            .get(&id)
            .map(|c| c.pending_reset)
            .unwrap_or(false)
    }

    /// Dispatch a [`LoadEvent`] through a caller-supplied handler.
    ///
    /// The handler receives each known variant as an exhaustive inner enum so
    /// consumers never need a `_ => {}` wildcard arm for `LoadEvent`'s
    /// `#[non_exhaustive]` restriction.  Unknown future variants are silently
    /// ignored (this method is updated when new variants land).
    ///
    /// Returns `true` when the event was dispatched to a known variant,
    /// `false` when it was an unknown future variant and the handler was not
    /// called.
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
        // `#[allow(unreachable_patterns)]` because from inside this crate all
        // LoadEvent variants are known; the wildcard exists so this helper
        // stays future-proof for external consumers when new variants land.
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
            // Unknown future variant — ignore gracefully.
            _ => false,
        }
    }

    /// Dispatch a [`ParseKind`] value through a caller-supplied handler.
    ///
    /// Eliminates `_ => {}` wildcards in consumer match arms by providing an
    /// exhaustive inner enum.  Unknown future variants fall back to the
    /// `ParseKind::Viewport` path (conservative: treat unknown as viewport).
    ///
    /// Returns `true` for known variants, `false` for unknown ones (and calls
    /// the handler with `ParseKindKind::Viewport` as the fallback).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use hjkl_syntax::{ParseKind, ParseKindKind, SyntaxLayer};
    ///
    /// let known = SyntaxLayer::dispatch_parse_kind(ParseKind::Top, |k| {
    ///     assert_eq!(k, ParseKindKind::Top);
    /// });
    /// assert!(known);
    /// ```
    pub fn dispatch_parse_kind(kind: ParseKind, mut handler: impl FnMut(ParseKindKind)) -> bool {
        // `#[allow(unreachable_patterns)]` because from inside this crate all
        // ParseKind variants are known; the wildcard exists so this helper
        // stays future-proof for external consumers when new variants land.
        #[allow(unreachable_patterns)]
        match kind {
            ParseKind::Viewport => {
                handler(ParseKindKind::Viewport);
                true
            }
            ParseKind::Top => {
                handler(ParseKindKind::Top);
                true
            }
            ParseKind::Bottom => {
                handler(ParseKindKind::Bottom);
                true
            }
            // Unknown future variant — fall back to Viewport so the caller
            // still gets a sensible route.
            _ => {
                handler(ParseKindKind::Viewport);
                false
            }
        }
    }
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
/// Used by tests; the production app constructs via [`layer_with_theme`].
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
    use std::time::Duration;

    fn submit_and_wait(
        layer: &mut SyntaxLayer,
        buf: &Buffer,
        top: usize,
        height: usize,
    ) -> Option<RenderOutput> {
        layer.submit_render(TID, buf, top, height, ParseKind::Viewport)?;
        layer.wait_for_result(Duration::from_secs(5))
    }

    const TID: BufferId = 0;

    // --- ParseKind ordering ---

    #[test]
    fn parse_kind_ordering_is_distinct() {
        // Perf invariant: the three variants must be distinct so the queue
        // deduplication does not accidentally coalesce different region requests.
        assert_ne!(ParseKind::Viewport, ParseKind::Top);
        assert_ne!(ParseKind::Viewport, ParseKind::Bottom);
        assert_ne!(ParseKind::Top, ParseKind::Bottom);
    }

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
            ParseKind::Bottom,
        );
        assert_eq!(out.buffer_id, 99);
        assert_eq!(out.kind, ParseKind::Bottom);
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
            ParseKind::Viewport,
        );
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn render_output_partial_eq_different_kind() {
        let a = RenderOutput::new(
            0,
            vec![],
            vec![],
            (0, 0, 10),
            PerfBreakdown::default(),
            ParseKind::Viewport,
        );
        let b = RenderOutput::new(
            0,
            vec![],
            vec![],
            (0, 0, 10),
            PerfBreakdown::default(),
            ParseKind::Top,
        );
        assert_ne!(a, b);
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

    /// `hex_color` capture spans must build a StyleSpec from their
    /// metadata (`hex.bg` / `hex.fg`) instead of going through the
    /// theme — that's the whole point of the inline-preview overlay.
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

    /// `hex_color` spans without metadata fall back to skipping (no
    /// theme key registered) instead of panicking — defensive guard
    /// against a future caller that emits the capture without metadata.
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

    // --- Pending queue deduplication ---

    #[test]
    fn pending_push_parse_replaces_same_buffer_kind() {
        let mut p = Pending::new();
        let make_req = |kind: ParseKind, dirty_gen: u64| ParseRequest {
            buffer_id: 0,
            source: Arc::new(String::new()),
            row_starts: Arc::new(vec![]),
            edits: vec![],
            viewport_byte_range: 0..0,
            viewport_top: 0,
            viewport_height: 10,
            row_count: 0,
            dirty_gen,
            reset: false,
            kind,
        };
        p.push_parse(make_req(ParseKind::Viewport, 1));
        p.push_parse(make_req(ParseKind::Viewport, 2));
        // Same (buffer_id=0, kind=Viewport) — should replace, not append.
        assert_eq!(p.parse_queue.len(), 1);
        assert_eq!(p.parse_queue[0].dirty_gen, 2);
    }

    #[test]
    fn pending_push_parse_merges_edits_on_replace() {
        let mut p = Pending::new();
        let mk_edit = |start_byte: usize| InputEdit {
            start_byte,
            old_end_byte: start_byte,
            new_end_byte: start_byte + 1,
            start_position: Point { row: 0, column: 0 },
            old_end_position: Point { row: 0, column: 0 },
            new_end_position: Point { row: 0, column: 1 },
        };
        let make_req = |dirty_gen: u64, edits: Vec<InputEdit>| ParseRequest {
            buffer_id: 0,
            source: Arc::new(String::new()),
            row_starts: Arc::new(vec![]),
            edits,
            viewport_byte_range: 0..0,
            viewport_top: 0,
            viewport_height: 10,
            row_count: 0,
            dirty_gen,
            reset: false,
            kind: ParseKind::Viewport,
        };
        p.push_parse(make_req(1, vec![mk_edit(0)]));
        p.push_parse(make_req(2, vec![mk_edit(10)]));
        // Replace merged: edits from BOTH requests must survive — dropping
        // the earlier ones leaves tree-sitter's retained tree at a stale
        // byte baseline and produces visibly misaligned spans afterward.
        assert_eq!(p.parse_queue.len(), 1);
        assert_eq!(p.parse_queue[0].edits.len(), 2);
        assert_eq!(p.parse_queue[0].edits[0].start_byte, 0);
        assert_eq!(p.parse_queue[0].edits[1].start_byte, 10);
        // The replacing request's dirty_gen is the latest.
        assert_eq!(p.parse_queue[0].dirty_gen, 2);
    }

    #[test]
    fn pending_push_parse_keeps_different_kinds() {
        let mut p = Pending::new();
        let make_req = |kind: ParseKind| ParseRequest {
            buffer_id: 0,
            source: Arc::new(String::new()),
            row_starts: Arc::new(vec![]),
            edits: vec![],
            viewport_byte_range: 0..0,
            viewport_top: 0,
            viewport_height: 10,
            row_count: 0,
            dirty_gen: 1,
            reset: false,
            kind,
        };
        p.push_parse(make_req(ParseKind::Viewport));
        p.push_parse(make_req(ParseKind::Top));
        p.push_parse(make_req(ParseKind::Bottom));
        // All three kinds for the same buffer must coexist.
        assert_eq!(p.parse_queue.len(), 3);
    }

    #[test]
    fn pending_push_parse_evicts_oldest_at_cap() {
        let mut p = Pending::new();
        // Fill past capacity with distinct (buffer_id, kind) pairs.
        for i in 0..(PARSE_QUEUE_CAP + 2) {
            p.push_parse(ParseRequest {
                buffer_id: i as BufferId,
                source: Arc::new(String::new()),
                row_starts: Arc::new(vec![]),
                edits: vec![],
                viewport_byte_range: 0..0,
                viewport_top: 0,
                viewport_height: 10,
                row_count: 0,
                dirty_gen: i as u64,
                reset: false,
                kind: ParseKind::Viewport,
            });
        }
        // Queue must not grow past cap.
        assert!(p.parse_queue.len() <= PARSE_QUEUE_CAP);
    }

    // --- SyntaxLayer basics (no network required) ---

    #[test]
    fn submit_with_no_language_returns_none() {
        let buf = Buffer::from_str("hello world");
        let mut layer = default_layer();
        assert!(
            !layer
                .set_language_for_path(TID, Path::new("a.unknownext"))
                .is_known()
        );
        assert!(
            layer
                .submit_render(TID, &buf, 0, 10, ParseKind::Viewport)
                .is_none()
        );
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
        assert!(
            layer
                .clients
                .get(&TID)
                .map(|c| c.pending_edits.is_empty())
                .unwrap_or(true)
        );
    }

    #[test]
    fn worker_handles_quit_cleanly() {
        let layer = default_layer();
        drop(layer);
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
        // Trigger client entry creation.
        layer.set_language_for_path(TID, Path::new("a.zzz_unknown"));
        // Even if no client was inserted (Unknown path), forget must not panic.
        layer.forget(TID);
        assert!(!layer.clients.contains_key(&TID));
    }

    #[test]
    fn take_all_results_empty_when_nothing_submitted() {
        let mut layer = default_layer();
        let results = layer.take_all_results();
        assert!(results.is_empty());
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
        let out = submit_and_wait(&mut layer, &buf, 0, 10).expect("worker output");
        assert_eq!(out.spans.len(), buf.row_count());
        assert!(
            out.spans.iter().any(|r| !r.is_empty()),
            "expected at least one styled span"
        );
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn first_load_highlights_entire_viewport() {
        let mut content = String::new();
        for i in 0..50 {
            content.push_str(&format!("fn f{i}() {{ let x = {i}; }}\n"));
        }
        let buf = Buffer::from_str(content.strip_suffix('\n').unwrap_or(&content));
        let mut layer = default_layer();
        assert!(
            layer
                .set_language_for_path(TID, Path::new("a.rs"))
                .is_known()
        );
        let out = submit_and_wait(&mut layer, &buf, 0, 30).unwrap();
        for (r, row) in out.spans.iter().take(30).enumerate() {
            assert!(
                !row.is_empty(),
                "row {r} has no highlight spans on first load"
            );
        }
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn diagnostics_emit_sign_for_syntax_error() {
        let buf = Buffer::from_str("fn main() {\nlet x = ;\n}\n");
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.rs"));
        let out = submit_and_wait(&mut layer, &buf, 0, 10).unwrap();
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
        let _ = submit_and_wait(&mut layer, &pre, 0, 10).unwrap();
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
        let inc = submit_and_wait(&mut layer, &post, 0, 10).unwrap();
        let mut cold_layer = default_layer();
        cold_layer.set_language_for_path(TID, Path::new("a.rs"));
        let cold = submit_and_wait(&mut cold_layer, &post, 0, 10).unwrap();
        assert_eq!(inc.spans, cold.spans);
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn reset_pending_request_is_consumed_once() {
        let buf = Buffer::from_str("fn main() {}");
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.rs"));
        layer.reset(TID);
        assert!(layer.clients.get(&TID).unwrap().pending_reset);
        let _ = submit_and_wait(&mut layer, &buf, 0, 10).unwrap();
        assert!(
            !layer.clients.get(&TID).unwrap().pending_reset,
            "pending_reset should clear after submit"
        );
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn forget_drops_buffer_state() {
        let buf = Buffer::from_str("fn main() {}");
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.rs"));
        let _ = submit_and_wait(&mut layer, &buf, 0, 10).unwrap();
        assert!(layer.clients.contains_key(&TID));
        layer.forget(TID);
        assert!(!layer.clients.contains_key(&TID));
    }
}

#[cfg(test)]
mod perf_smoke {
    use super::*;
    use hjkl_buffer::Buffer;
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn big_rs_smoke() {
        let path = Path::new("/tmp/big.rs");
        if !path.exists() {
            eprintln!("/tmp/big.rs not present; skipping perf smoke");
            return;
        }
        let content = std::fs::read_to_string(path).unwrap();
        let buf = Buffer::from_str(content.strip_suffix('\n').unwrap_or(&content));
        let mut layer = default_layer();
        const TID: BufferId = 0;
        assert!(layer.set_language_for_path(TID, path).is_known());

        let t0 = Instant::now();
        layer.submit_render(TID, &buf, 0, 50, ParseKind::Viewport);
        let main_t = t0.elapsed();
        let out = layer.wait_for_result(Duration::from_secs(10));
        eprintln!(
            "first submit_render main-thread: {:?}, worker turnaround total: {:?}",
            main_t,
            t0.elapsed()
        );
        assert!(out.is_some(), "first parse should produce output");

        let t0 = Instant::now();
        let mut main_total = Duration::ZERO;
        for top in 0..100 {
            let s = Instant::now();
            layer.submit_render(TID, &buf, top * 100, 50, ParseKind::Viewport);
            main_total += s.elapsed();
        }
        while layer.take_result().is_some() {}
        eprintln!(
            "100 viewport scrolls: total wall {:?}, main-thread total {:?} (avg {:?}/submit)",
            t0.elapsed(),
            main_total,
            main_total / 100
        );
    }
}
