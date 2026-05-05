//! `SyntaxLayer` — tree-sitter highlight computation for the TUI binary.
//!
//! Owns a [`SyntaxWorker`] (background thread holding the `Highlighter`
//! and retained `tree_sitter::Tree`) plus a main-thread `RenderCache`
//! of `(source, row_starts)`. Call
//! [`SyntaxLayer::set_language_for_path`] after opening a file, then
//! [`SyntaxLayer::apply_edits`] for each frame's queued
//! [`hjkl_engine::ContentEdit`] batch and [`SyntaxLayer::submit_render`]
//! to enqueue a parse + viewport-scoped highlight on the worker. Drain
//! results via [`SyntaxLayer::take_result`] each frame and install the
//! latest one onto the editor.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use hjkl_bonsai::runtime::{Grammar, LoadHandle};
use hjkl_bonsai::{CommentMarkerPass, DotFallbackTheme, Highlighter, InputEdit, Point, Theme};
use hjkl_buffer::Sign;
use hjkl_engine::Query;

use crate::lang::{GrammarRequest, LanguageDirectory};

/// Stable identifier for an open buffer. Assigned by the App; carried
/// through every syntax-pipeline message so the worker can multiplex
/// per-buffer tree state (helix-style).
pub type BufferId = u64;

/// Per-frame output of [`SyntaxLayer::take_result`]: the styled span
/// table, diagnostic signs for the gutter (one per row with a tree-sitter
/// ERROR / MISSING node intersecting the viewport), the cache key the
/// request was tagged with so the App can pair it with `last_recompute_key`,
/// and a [`PerfBreakdown`] describing where the worker spent its time.
#[derive(Debug, Clone)]
pub struct RenderOutput {
    /// Routes spans/signs back to the matching BufferSlot in App::slots.
    #[allow(dead_code)] // Routing not yet wired; field is set by the worker.
    pub buffer_id: BufferId,
    pub spans: Vec<Vec<(usize, usize, ratatui::style::Style)>>,
    pub signs: Vec<Sign>,
    /// `(dirty_gen, viewport_top, viewport_height)` — same shape the App
    /// uses for its own cache key. Pair the result with this on receive.
    pub key: (u64, usize, usize),
    pub perf: PerfBreakdown,
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

/// Per-call sub-step timings exposed to apps/hjkl's `:perf` overlay.
/// Recorded on the worker side and shipped back inside [`RenderOutput`].
#[derive(Default, Debug, Clone, Copy)]
pub struct PerfBreakdown {
    pub source_build_us: u128,
    pub parse_us: u128,
    pub highlight_us: u128,
    pub by_row_us: u128,
    pub diag_us: u128,
}

/// Cached `(source, row_starts)` keyed off buffer identity (dirty_gen +
/// shape). Built once per buffer mutation on the **main** thread and
/// shipped to the worker as `Arc`s so the worker doesn't memcpy a 1.3MB
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
    /// Buffer the request targets — selects which retained tree the
    /// worker uses.
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
}

/// Control + data messages the worker thread waits on.
enum Msg {
    /// Set / replace the highlighter for a buffer. `None` detaches (no
    /// highlighter → parse requests for this buffer are dropped).
    SetLanguage(BufferId, Option<Arc<Grammar>>),
    /// Drop the retained tree for a buffer so the next parse is cold.
    #[allow(dead_code)] // Phase B: wired via ParseRequest.reset flag for now.
    Reset(BufferId),
    /// Remove all worker state for a buffer (highlighter, retained
    /// tree, parse-cache key). Sent on buffer close.
    Forget(BufferId),
    /// Replace the theme. Style resolution happens on the worker.
    SetTheme(Arc<dyn Theme + Send + Sync>),
    /// A parse + render job. Coalesced — only the latest pending
    /// `Parse` survives if the worker is busy.
    Parse(ParseRequest),
    /// Worker should exit. Sent on `SyntaxWorker::drop`.
    Quit,
}

/// Shared slot the main thread drops new requests into. The worker
/// pulls one message at a time; if a `Parse` is already pending and a
/// new `Parse` arrives, the old one is replaced (latest-wins
/// coalescing). Control messages (`SetLanguage`, `SetTheme`, `Reset`,
/// `Quit`) are queued in `controls` so they aren't dropped.
struct Pending {
    /// `Some` when a Parse is queued. Replaced on each new submit.
    parse: Option<ParseRequest>,
    /// FIFO of control messages (everything that's not a Parse).
    controls: std::collections::VecDeque<Msg>,
}

impl Pending {
    fn new() -> Self {
        Self {
            parse: None,
            controls: std::collections::VecDeque::new(),
        }
    }

    fn has_work(&self) -> bool {
        self.parse.is_some() || !self.controls.is_empty()
    }
}

/// Background worker that owns the `Highlighter` and the retained
/// tree-sitter `Tree`. Communicates with the main thread via a
/// `Mutex<Pending>` + `Condvar` for submits, and an mpsc channel for
/// rendered output.
pub struct SyntaxWorker {
    pending: Arc<(Mutex<Pending>, Condvar)>,
    rx: std::sync::mpsc::Receiver<RenderOutput>,
    handle: Option<JoinHandle<()>>,
}

impl SyntaxWorker {
    /// Spawn a fresh worker thread with the given theme and language directory.
    /// The worker has no language attached yet — call
    /// [`SyntaxWorker::set_language`].
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

    /// Drop a buffer's retained tree so the next parse for it is cold.
    #[allow(dead_code)] // Phase B: reset is currently routed via ParseRequest.reset.
    pub fn reset(&self, id: BufferId) {
        self.enqueue_control(Msg::Reset(id));
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

    /// Submit a parse job. If a previous job is still pending, it's
    /// replaced (latest-wins). Returns immediately.
    fn submit(&self, req: ParseRequest) {
        let (lock, cvar) = &*self.pending;
        let mut p = lock.lock().expect("syntax pending mutex poisoned");
        p.parse = Some(req);
        cvar.notify_one();
    }

    /// Drain all available render results, returning the most recent
    /// one. Earlier results are discarded — they'd just be overwritten
    /// by the latest install anyway, and this keeps the install path
    /// O(1) per frame regardless of backlog depth.
    pub fn try_recv_latest(&self) -> Option<RenderOutput> {
        let mut latest: Option<RenderOutput> = None;
        while let Ok(out) = self.rx.try_recv() {
            latest = Some(out);
        }
        latest
    }

    /// Wait up to `timeout` for the next result, then drain anything
    /// else that arrived after it and return the latest. Use this after
    /// submitting a viewport-only request to avoid a blink between the
    /// jump and the worker's response — the request is cheap enough
    /// (~1 ms with the parse-skip fast path) that a few-ms block here
    /// keeps highlights coherent across `gg` / `G` jumps without
    /// noticeably stalling fast typing on real edits.
    pub fn wait_for_latest(&self, timeout: std::time::Duration) -> Option<RenderOutput> {
        let mut latest = self.rx.recv_timeout(timeout).ok();
        while let Ok(out) = self.rx.try_recv() {
            latest = Some(out);
        }
        latest
    }
}

impl Drop for SyntaxWorker {
    fn drop(&mut self) {
        // Wake the worker with a Quit; ignore the result if the receiver
        // is already gone.
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

/// Per-buffer state retained on the worker side: the `Highlighter`
/// (which owns its compiled `Query` + retained `Tree`) plus the
/// `dirty_gen` for which that tree is current. Pure-viewport requests
/// against the same `dirty_gen` skip `parse_incremental` entirely.
struct WorkerBufferState {
    highlighter: Highlighter,
    last_parsed_dirty_gen: Option<u64>,
}

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

    loop {
        let msg = {
            let (lock, cvar) = &*pending;
            let mut p = lock.lock().expect("syntax pending mutex poisoned");
            while !p.has_work() {
                p = cvar.wait(p).expect("syntax pending cvar poisoned");
            }
            // Drain controls first so a SetLanguage / Reset that
            // arrived alongside a Parse gets applied before we run the
            // parse with stale state.
            if let Some(c) = p.controls.pop_front() {
                c
            } else {
                Msg::Parse(p.parse.take().expect("has_work() implies parse present"))
            }
        };

        match msg {
            Msg::Quit => return,
            Msg::SetLanguage(id, None) => {
                buffers.remove(&id);
            }
            Msg::SetLanguage(id, Some(grammar)) => match Highlighter::new(grammar) {
                Ok(h) => {
                    buffers.insert(
                        id,
                        WorkerBufferState {
                            highlighter: h,
                            last_parsed_dirty_gen: None,
                        },
                    );
                }
                Err(_) => {
                    buffers.remove(&id);
                }
            },
            Msg::Reset(id) => {
                if let Some(s) = buffers.get_mut(&id) {
                    s.highlighter.reset();
                    s.last_parsed_dirty_gen = None;
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
                let needs_parse = !req.edits.is_empty()
                    || h.tree().is_none()
                    || state.last_parsed_dirty_gen != Some(req.dirty_gen);
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
                });
            }
        }
    }
}

/// Resolve flat highlight spans into a per-row span table sized to
/// `row_count`. Pulled out so the worker can call it in isolation.
fn build_by_row(
    flat_spans: &[hjkl_bonsai::HighlightSpan],
    bytes: &[u8],
    row_starts: &[usize],
    row_count: usize,
    theme: &dyn Theme,
) -> Vec<Vec<(usize, usize, ratatui::style::Style)>> {
    let mut by_row: Vec<Vec<(usize, usize, ratatui::style::Style)>> = vec![Vec::new(); row_count];

    for span in flat_spans {
        let style = match theme.style(span.capture()) {
            Some(s) => s.to_ratatui(),
            None => continue,
        };

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
                by_row[row].push((local_start, local_end, style));
            }

            row += 1;
        }
    }

    by_row
}

/// Collect diagnostic [`Sign`]s from tree-sitter ERROR / MISSING nodes
/// intersecting the viewport, deduped to one per row.
fn collect_diag_signs(
    h: &mut Highlighter,
    bytes: &[u8],
    viewport_byte_range: std::ops::Range<usize>,
    row_starts: &[usize],
) -> Vec<Sign> {
    let errors = h.parse_errors_range(bytes, viewport_byte_range);
    let mut signs: Vec<Sign> = Vec::new();
    let mut last_row: Option<usize> = None;
    let err_style = ratatui::style::Style::default().fg(ratatui::style::Color::Red);
    for err in &errors {
        let r = row_starts
            .partition_point(|&rs| rs <= err.byte_range.start)
            .saturating_sub(1);
        if last_row == Some(r) {
            continue;
        }
        last_row = Some(r);
        signs.push(Sign {
            row: r,
            ch: 'E',
            style: err_style,
            priority: 100,
        });
    }
    signs
}

/// Per-buffer client-side state. One of these per open buffer in
/// `SyntaxLayer.clients`. Mirrors the worker's `WorkerBufferState` but
/// holds the source-cache + edit queue, which live on the main thread.
#[derive(Default)]
struct BufferClient {
    /// `true` when a language is attached. Gates `submit_render`.
    has_language: bool,
    /// Active grammar — used by `preview_render` to spin up a one-shot
    /// `Highlighter`. Cloned-Arc so the worker doesn't race the main
    /// thread on language swaps.
    current_lang: Option<Arc<Grammar>>,
    /// Cached `(source, row_starts)` — rebuilt on `dirty_gen` change.
    cache: Option<RenderCache>,
    /// Edits queued since the last submit.
    pending_edits: Vec<InputEdit>,
    /// Set by `reset()` so the next submit asks the worker to drop its
    /// retained tree for this buffer.
    pending_reset: bool,
    /// Last `dirty_gen` shipped to the worker for this buffer.
    last_submitted_dirty_gen: Option<u64>,
}

/// Outcome of [`SyntaxLayer::set_language_for_path`].
///
/// Callers that previously tested the return value as a `bool` should use
/// `outcome.is_known()` for equivalent behaviour.
pub enum SetLanguageOutcome {
    /// Grammar was already cached (or found fresh on disk) — installed
    /// immediately.  Buffer will highlight on the next render.
    Ready,
    /// Grammar is being fetched/compiled on the background pool.  Buffer
    /// renders as plain text until [`SyntaxLayer::poll_pending_loads`] fires
    /// the `Ready` event for this buffer.  The inner `String` is the language
    /// name (useful for status-line indicators — TODO hjkl#17).
    #[allow(dead_code)]
    Loading(String),
    /// Extension unrecognized.  No grammar — plain text only.
    Unknown,
}

impl SetLanguageOutcome {
    /// `true` when a grammar was found (either already cached or now in
    /// flight).  Drop-in replacement for the old `bool` return value.
    pub fn is_known(&self) -> bool {
        matches!(self, Self::Ready | Self::Loading(_))
    }
}

/// Event emitted by [`SyntaxLayer::poll_pending_loads`] for each handle that
/// resolved during the tick.
pub enum LoadEvent {
    /// Grammar installed; trigger a redraw + re-submit for `id`.
    Ready { id: BufferId, name: String },
    /// Load failed (clone/compile error); buffer stays plain text.
    Failed {
        id: BufferId,
        name: String,
        error: String,
    },
}

/// An in-flight grammar load tracked by `SyntaxLayer`.
struct PendingLoad {
    id: BufferId,
    name: String,
    handle: LoadHandle,
}

/// Per-`App` syntax highlighting layer. Multiplexes per-buffer state
/// (helix-style): each open buffer carries its own retained tree
/// (worker-side) plus source-cache and edit queue (here). One worker
/// thread serves all buffers.
pub struct SyntaxLayer {
    /// Shared grammar resolver. `Arc` so picker sources (and any future
    /// subsystem) can hold the same in-memory `Grammar` cache.
    directory: Arc<LanguageDirectory>,
    /// Active theme — cloned to the worker on spawn / `set_theme`, kept
    /// on the layer so `preview_render` can resolve capture styles
    /// without crossing the worker boundary.
    theme: Arc<dyn Theme + Send + Sync>,
    worker: SyntaxWorker,
    /// Per-buffer client state, keyed by `BufferId`.
    clients: HashMap<BufferId, BufferClient>,
    /// In-flight async grammar loads.  Polled each tick via
    /// `poll_pending_loads`.
    pending_loads: Vec<PendingLoad>,
    /// Last perf breakdown received via `take_result`. Surfaced to the
    /// `:perf` overlay; updated on every successful drain.
    pub last_perf: PerfBreakdown,
}

impl SyntaxLayer {
    /// Create a new layer with no buffers attached, the given theme, and
    /// the given language directory. The directory is the only place
    /// language `Grammar`s live, so sharing it across subsystems
    /// (`HighlightedBufferSource`, etc.) deduplicates dlopen+query loads.
    pub fn new(theme: Arc<dyn Theme + Send + Sync>, directory: Arc<LanguageDirectory>) -> Self {
        let worker = SyntaxWorker::spawn(Arc::clone(&theme), Arc::clone(&directory));
        Self {
            directory,
            theme,
            worker,
            clients: HashMap::new(),
            pending_loads: Vec::new(),
            last_perf: PerfBreakdown::default(),
        }
    }

    /// Get or create the client state for `id`. Used by every per-
    /// buffer method below.
    fn client_mut(&mut self, id: BufferId) -> &mut BufferClient {
        self.clients.entry(id).or_default()
    }

    /// Detect the language for `path` and ship it to the worker.
    ///
    /// Non-blocking: uses the async grammar loader so opening a file with an
    /// uninstalled grammar no longer freezes the UI for 1–3 s during
    /// clone+compile (hjkl#17 follow-up).
    ///
    /// - `Ready`   — grammar cached/found on disk; installed immediately.
    /// - `Loading` — clone+compile kicked off; buffer renders as plain text
    ///   until `poll_pending_loads` fires the companion `LoadEvent::Ready`.
    /// - `Unknown` — unrecognized extension; plain text only.
    ///
    /// Callers that previously used the `bool` return value should switch to
    /// `outcome.is_known()`.
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
                // Detach for now: render as plain text while the grammar
                // is being fetched/compiled in the background.
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
        }
    }

    /// Poll all in-flight grammar loads.  Call this once per tick from the
    /// main loop (alongside `take_result`) so completed loads install
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
                    // Still in flight — leave in list.
                    i += 1;
                }
                Some(Ok(lib_path)) => {
                    let name = self.pending_loads[i].name.clone();
                    let bid = self.pending_loads[i].id;
                    // O(1) removal; order of pending list doesn't matter.
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
                    // i stays the same — swap_remove put a new element at i.
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
                    // i stays the same.
                }
            }
        }
        events
    }

    /// Lightweight read-only accessor for the renderer. Returns the name of
    /// the grammar currently being loaded for `id`, if any.
    pub fn pending_load_name_for(&self, id: BufferId) -> Option<&str> {
        self.pending_loads
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.as_str())
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

    /// Synchronous viewport-only preview render. Builds a
    /// `String` containing **only** the visible rows, parses it from
    /// scratch with a one-shot `Highlighter`, runs `highlight_range` over
    /// the slice, and returns a `RenderOutput` whose `spans` table is
    /// padded with empty rows above the viewport so the install path
    /// indexes the right rows.
    ///
    /// Cost is proportional to the visible region (a few KB for typical
    /// terminals), so this completes in well under a millisecond even
    /// when the full file would take 100ms+ to parse. Used at startup so
    /// the very first frame has highlights regardless of where in the
    /// file the viewport landed.
    ///
    /// The slice doesn't begin at a syntactically valid root for most
    /// grammars, so structural captures (function signatures, types) may
    /// not all fire — but token-level captures (keyword, identifier,
    /// string, comment, number) do, which is what the eye picks up
    /// first. The worker's full parse arrives moments later and replaces
    /// the preview with the structurally-correct spans.
    ///
    /// Returns `None` when no language is attached or when the viewport
    /// is empty.
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
            source.push_str(buffer.line(r as u32));
        }
        let bytes = source.as_bytes();
        let mut row_starts: Vec<usize> = vec![0];
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                row_starts.push(i + 1);
            }
        }
        let local_row_count = vp_end_row - vp_top;

        let mut h = Highlighter::new(grammar).ok()?;
        let mut flat_spans =
            h.highlight_with_injections(bytes, |name| self.directory.by_name(name));

        // Overlay TODO/FIXME/NOTE/WARN marker spans.
        let marker_pass = CommentMarkerPass::new();
        marker_pass.apply(&mut flat_spans, bytes);

        let local_by_row = build_by_row(
            &flat_spans,
            bytes,
            &row_starts,
            local_row_count,
            self.theme.as_ref(),
        );

        let mut spans: Vec<Vec<(usize, usize, ratatui::style::Style)>> = vec![Vec::new(); vp_top];
        spans.extend(local_by_row);

        Some(RenderOutput {
            buffer_id: id,
            spans,
            signs: Vec::new(),
            key: (buffer.dirty_gen(), viewport_top, viewport_height),
            perf: PerfBreakdown::default(),
        })
    }

    /// Ask the worker to drop this buffer's retained tree on the next
    /// parse so the next submission is cold.
    pub fn reset(&mut self, id: BufferId) {
        self.client_mut(id).pending_reset = true;
    }

    /// Buffer a batch of engine `ContentEdit`s to be shipped to the
    /// worker on the next `submit_render`. Translates the engine's
    /// position pairs into `tree_sitter::InputEdit`s up front so the
    /// worker only does the cheap `tree.edit(...)` call.
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
    /// current buffer state and submit a parse + render job to the
    /// worker. Returns immediately. Drain the result with
    /// [`Self::take_result`].
    ///
    /// Returns `None` and submits nothing when no language is attached.
    /// Returns `Some(source_build_us)` when a request was submitted —
    /// `0` means the cache was reused.
    pub fn submit_render(
        &mut self,
        id: BufferId,
        buffer: &impl Query,
        viewport_top: usize,
        viewport_height: usize,
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
        // Pure scroll frames skip the O(N) string join + newline scan
        // and reuse the previous Arc<String> directly.
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
                source.push_str(buffer.line(r as u32));
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

        // Compute viewport byte range. byte_of_row clamps past-end to
        // len_bytes so the +1 row beyond the visible range is safe.
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
        });

        Some(source_build_us)
    }

    /// Drain the most recent render result the worker has produced (if
    /// any). Older results are discarded — only the latest matters for
    /// install. Updates `last_perf` as a side effect.
    pub fn take_result(&mut self) -> Option<RenderOutput> {
        let out = self.worker.try_recv_latest()?;
        self.last_perf = out.perf;
        Some(out)
    }

    /// Block up to `timeout` for the worker's next result, then drain
    /// any others that arrived after it. Useful right after submitting
    /// a viewport-only request: the worker's parse-skip fast path
    /// returns in ~1ms, so a few-ms wait keeps `gg` / `G` jumps from
    /// flashing un-highlighted rows.
    pub fn wait_result(&mut self, timeout: std::time::Duration) -> Option<RenderOutput> {
        let out = self.worker.wait_for_latest(timeout)?;
        self.last_perf = out.perf;
        Some(out)
    }

    /// Synchronously drain the next result, blocking up to `timeout`.
    /// Returns `None` on timeout. Used at startup so the very first
    /// frame can paint with highlights when the worker is fast enough,
    /// while still capping the worst case so giant files don't stall
    /// the splash.
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
}

/// Build a `SyntaxLayer` using the given theme + language directory.
pub fn layer_with_theme(
    theme: Arc<DotFallbackTheme>,
    directory: Arc<LanguageDirectory>,
) -> SyntaxLayer {
    SyntaxLayer::new(theme, directory)
}

/// Build a `SyntaxLayer` with hjkl-bonsai's bundled dark theme.
/// Used by tests; the production app constructs via [`layer_with_theme`]
/// with the [`crate::theme::AppTheme`] override.
#[cfg(test)]
pub fn default_layer() -> SyntaxLayer {
    let directory = Arc::new(LanguageDirectory::new().expect("language directory"));
    SyntaxLayer::new(Arc::new(DotFallbackTheme::dark()), directory)
}

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
        layer.submit_render(TID, buf, top, height)?;
        layer.wait_for_result(Duration::from_secs(5))
    }

    /// Test buffer id — multiplexing is exercised end-to-end by the
    /// app, but each unit test uses a single buffer.
    const TID: BufferId = 0;

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
    fn submit_with_no_language_returns_none() {
        let buf = Buffer::from_str("hello world");
        let mut layer = default_layer();
        // Unknown extension — no language attached.
        assert!(
            !layer
                .set_language_for_path(TID, Path::new("a.unknownext"))
                .is_known()
        );
        assert!(layer.submit_render(TID, &buf, 0, 10).is_none());
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
                "row {r} has no highlight spans on first load (content: {:?})",
                buf.line(r)
            );
        }
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn first_load_full_viewport_matches_full_parse() {
        let mut content = String::new();
        for i in 0..50 {
            content.push_str(&format!("fn f{i}() {{ let x = {i}; }}\n"));
        }
        let buf = Buffer::from_str(content.strip_suffix('\n').unwrap_or(&content));

        let mut narrow = default_layer();
        narrow.set_language_for_path(TID, Path::new("a.rs"));
        let narrow_out = submit_and_wait(&mut narrow, &buf, 0, 30).unwrap();

        let mut full = default_layer();
        full.set_language_for_path(TID, Path::new("a.rs"));
        let full_out = submit_and_wait(&mut full, &buf, 0, 100).unwrap();

        for r in 0..30 {
            assert_eq!(
                narrow_out.spans[r], full_out.spans[r],
                "row {r} differs between viewport-scoped and full parse"
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
    fn diagnostics_clean_source_no_signs() {
        let buf = Buffer::from_str("fn main() { let x = 1; }\n");
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.rs"));
        let out = submit_and_wait(&mut layer, &buf, 0, 10).unwrap();
        assert!(
            out.signs.is_empty(),
            "expected no signs; got {:?}",
            out.signs
        );
    }

    #[test]
    #[ignore = "network + compiler: needs tree-sitter-rust grammar"]
    fn incremental_path_matches_cold_for_small_edit() {
        // Pre: parse a buffer through the worker.
        let pre = Buffer::from_str("fn main() { let x = 1; }");
        let mut layer = default_layer();
        layer.set_language_for_path(TID, Path::new("a.rs"));
        let _ = submit_and_wait(&mut layer, &pre, 0, 10).unwrap();

        // Apply an edit: insert "Y" at byte 3 ("fn ⎀main…").
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

        // Cold parse from a fresh layer.
        let mut cold_layer = default_layer();
        cold_layer.set_language_for_path(TID, Path::new("a.rs"));
        let cold = submit_and_wait(&mut cold_layer, &post, 0, 10).unwrap();

        assert_eq!(inc.spans, cold.spans);
    }

    #[test]
    fn worker_handles_quit_cleanly() {
        // Spawn a layer, drop it, and confirm the worker thread joined.
        // (The Drop impl unconditionally joins; if the worker hangs the
        // test would deadlock — that's the contract.)
        let layer = default_layer();
        drop(layer);
    }

    /// `set_language_for_path` on an unknown extension must return `Unknown`
    /// (not `Ready` or `Loading`), and the helper `is_known()` must be false.
    #[test]
    fn set_language_for_path_returns_unknown_for_unrecognized_extension() {
        let mut layer = default_layer();
        let outcome = layer.set_language_for_path(TID, Path::new("a.zzznope_not_real"));
        assert!(
            !outcome.is_known(),
            "expected Unknown for unrecognized extension"
        );
        assert!(matches!(outcome, SetLanguageOutcome::Unknown));
    }

    /// `poll_pending_loads` on an empty pending list must return an empty Vec
    /// without panicking.  Regression catcher for the swap_remove loop.
    #[test]
    fn poll_pending_loads_drains_ready_handles() {
        let mut layer = default_layer();
        // No pending loads — must not panic, must return empty.
        let events = layer.poll_pending_loads();
        assert!(
            events.is_empty(),
            "expected no events with no pending loads"
        );
    }

    /// For an unrecognized grammar (no network), `set_language_for_path` must
    /// return `Loading` or `Unknown` — never block the caller for seconds.
    /// The real "Loading only for true clone+compile" invariant is validated
    /// by the bonsai-side async_loader tests; here we just assert that a
    /// known language name (rust) for which no grammar is installed on CI
    /// does NOT return `Ready` (cache miss) and returns in well under 1 s.
    ///
    /// Gated `#[ignore]` because on a machine that *does* have the grammar
    /// pre-installed the result is `Ready`, which is also correct. The test
    /// is most useful as documentation of the invariant.
    #[test]
    #[ignore = "disk-state dependent: result depends on whether rust grammar is pre-installed"]
    fn set_language_for_path_returns_loading_for_uncached_grammar() {
        let mut layer = default_layer();
        let t0 = std::time::Instant::now();
        let outcome = layer.set_language_for_path(TID, Path::new("a.rs"));
        let elapsed = t0.elapsed();
        assert!(
            elapsed.as_millis() < 500,
            "set_language_for_path blocked for {}ms — must be non-blocking",
            elapsed.as_millis()
        );
        // On a cold disk the outcome must be Loading, not Ready.
        assert!(
            matches!(outcome, SetLanguageOutcome::Loading(_)),
            "expected Loading on cold disk"
        );
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

    /// Smoke perf: open /tmp/big.rs (100k stub fns, ~1.3MB), submit + drain
    /// 100 viewport scrolls + a single edit. Skipped when the file isn't
    /// present. Not a regression gate — eyeballs only via `--nocapture`.
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
        layer.submit_render(TID, &buf, 0, 50);
        let main_t = t0.elapsed();
        let out = layer.wait_for_result(Duration::from_secs(10));
        eprintln!(
            "first submit_render main-thread: {:?}, worker turnaround total: {:?}",
            main_t,
            t0.elapsed()
        );
        assert!(out.is_some(), "first parse should produce output");

        // 100 pure-scroll submits: same dirty_gen, different viewport.
        // Source/row_starts cache hits — main thread should be µs-scale.
        let t0 = Instant::now();
        let mut main_total = Duration::ZERO;
        for top in 0..100 {
            let s = Instant::now();
            layer.submit_render(TID, &buf, top * 100, 50);
            main_total += s.elapsed();
        }
        // Drain whatever the worker produced.
        while layer.take_result().is_some() {}
        eprintln!(
            "100 viewport scrolls: total wall {:?}, main-thread total {:?} (avg {:?}/submit)",
            t0.elapsed(),
            main_total,
            main_total / 100
        );

        // Simulate a single-char insert at row 50_000, col 0.
        let lines = buf.lines().to_vec();
        let mut new_lines = lines.clone();
        new_lines[50_000].insert(0, 'X');
        let post = Buffer::from_str(&new_lines.join("\n"));

        let edit_byte = (0..50_000).map(|r| lines[r].len() + 1).sum::<usize>();
        layer.apply_edits(
            TID,
            &[hjkl_engine::ContentEdit {
                start_byte: edit_byte,
                old_end_byte: edit_byte,
                new_end_byte: edit_byte + 1,
                start_position: (50_000, 0),
                old_end_position: (50_000, 0),
                new_end_position: (50_000, 1),
            }],
        );
        let t = Instant::now();
        layer.submit_render(TID, &post, 0, 50);
        let main_us = t.elapsed();
        let out = layer.wait_for_result(Duration::from_secs(10));
        eprintln!(
            "post-edit submit: main-thread {:?}, worker total {:?} (per-step: {:?})",
            main_us,
            t.elapsed(),
            out.as_ref().map(|o| o.perf),
        );
    }

    #[test]
    fn pending_load_name_for_returns_none_when_empty() {
        // Regression catcher: a fresh SyntaxLayer with no pending loads must
        // return None for any BufferId.
        let theme = crate::theme::AppTheme::default_dark();
        let directory =
            std::sync::Arc::new(crate::lang::LanguageDirectory::new().expect("directory"));
        let layer = SyntaxLayer::new(theme.syntax.clone(), directory);
        assert_eq!(layer.pending_load_name_for(0), None);
        assert_eq!(layer.pending_load_name_for(99), None);
    }
}
