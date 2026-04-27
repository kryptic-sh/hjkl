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

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use hjkl_buffer::Sign;
use hjkl_engine::Query;
use hjkl_tree_sitter::{
    DotFallbackTheme, Highlighter, InputEdit, LanguageConfig, LanguageRegistry, Point, Theme,
};

/// Per-frame output of [`SyntaxLayer::take_result`]: the styled span
/// table, diagnostic signs for the gutter (one per row with a tree-sitter
/// ERROR / MISSING node intersecting the viewport), the cache key the
/// request was tagged with so the App can pair it with `last_recompute_key`,
/// and a [`PerfBreakdown`] describing where the worker spent its time.
#[derive(Debug, Clone)]
pub struct RenderOutput {
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
    /// Replace the active highlighter for a new language config. `None`
    /// detaches (no highlighter → worker drops parse requests).
    SetLanguage(Option<&'static LanguageConfig>),
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
    /// Spawn a fresh worker thread with the given theme. The worker has
    /// no language attached yet — call [`SyntaxWorker::set_language`].
    pub fn spawn(theme: Arc<dyn Theme + Send + Sync>) -> Self {
        let pending = Arc::new((Mutex::new(Pending::new()), Condvar::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let pending_for_thread = Arc::clone(&pending);
        let handle = thread::Builder::new()
            .name("hjkl-syntax".into())
            .spawn(move || worker_loop(pending_for_thread, tx, theme))
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

    /// Switch the worker to a new language. Drops any retained tree.
    pub fn set_language(&self, config: Option<&'static LanguageConfig>) {
        self.enqueue_control(Msg::SetLanguage(config));
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

fn worker_loop(
    pending: Arc<(Mutex<Pending>, Condvar)>,
    tx: std::sync::mpsc::Sender<RenderOutput>,
    initial_theme: Arc<dyn Theme + Send + Sync>,
) {
    use std::time::Instant;

    let mut highlighter: Option<Highlighter> = None;
    let mut theme: Arc<dyn Theme + Send + Sync> = initial_theme;
    // Buffer dirty_gen for which the retained tree is current. When the
    // next Parse request has the same dirty_gen and no new edits, skip
    // parse_incremental entirely — pure-viewport changes (gg / G / Ctrl-D
    // etc.) hit this fast path and cost just highlight_range + diag.
    let mut last_parsed_dirty_gen: Option<u64> = None;

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
            Msg::SetLanguage(None) => {
                highlighter = None;
                last_parsed_dirty_gen = None;
            }
            Msg::SetLanguage(Some(cfg)) => {
                match Highlighter::new(cfg) {
                    Ok(h) => highlighter = Some(h),
                    Err(_) => highlighter = None,
                }
                last_parsed_dirty_gen = None;
            }
            Msg::SetTheme(t) => {
                theme = t;
            }
            Msg::Parse(req) => {
                let Some(h) = highlighter.as_mut() else {
                    continue;
                };
                let mut perf = PerfBreakdown::default();
                if req.reset {
                    h.reset();
                    last_parsed_dirty_gen = None;
                }
                let needs_parse = !req.edits.is_empty()
                    || h.tree().is_none()
                    || last_parsed_dirty_gen != Some(req.dirty_gen);
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
                    last_parsed_dirty_gen = Some(req.dirty_gen);
                }
                let bytes = req.source.as_bytes();

                let t = Instant::now();
                let flat_spans = h.highlight_range(bytes, req.viewport_byte_range.clone());
                perf.highlight_us = t.elapsed().as_micros();

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
    flat_spans: &[hjkl_tree_sitter::HighlightSpan],
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

/// Per-`App` syntax highlighting layer. Caches `(source, row_starts)`
/// on the main thread (so successive submits don't rebuild the source
/// for unchanged buffers) and forwards work to a [`SyntaxWorker`].
pub struct SyntaxLayer {
    registry: LanguageRegistry,
    /// `Some` only when a language is attached. Used to gate
    /// `submit_render` (no point sending a parse with no highlighter)
    /// and to know whether to ship the next request as `reset: true`.
    has_language: bool,
    worker: SyntaxWorker,
    cache: Option<RenderCache>,
    /// Edits queued since the last submit; flushed into the next
    /// `ParseRequest`.
    pending_edits: Vec<InputEdit>,
    /// Set by `reset()` so the next submitted request asks the worker
    /// to drop its retained tree.
    pending_reset: bool,
    /// Last `dirty_gen` we shipped to the worker. Skips duplicate
    /// submits when the buffer hasn't changed and only the viewport
    /// did — the worker's previous output is still right and the App
    /// has its own cache for that case anyway.
    last_submitted_dirty_gen: Option<u64>,
    /// Last perf breakdown received via `take_result`. Surfaced to the
    /// `:perf` overlay; updated on every successful drain.
    pub last_perf: PerfBreakdown,
}

impl SyntaxLayer {
    /// Create a new layer with no language attached and the given theme.
    pub fn new(theme: Arc<dyn Theme + Send + Sync>) -> Self {
        let worker = SyntaxWorker::spawn(theme);
        Self {
            registry: LanguageRegistry::new(),
            has_language: false,
            worker,
            cache: None,
            pending_edits: Vec::new(),
            pending_reset: false,
            last_submitted_dirty_gen: None,
            last_perf: PerfBreakdown::default(),
        }
    }

    /// Detect the language for `path` and ship it to the worker.
    ///
    /// Returns `true` when a language was found.
    /// Returns `false` (and detaches the worker's highlighter) for
    /// unknown extensions.
    pub fn set_language_for_path(&mut self, path: &Path) -> bool {
        match self.registry.detect_for_path(path) {
            Some(config) => {
                self.worker.set_language(Some(config));
                self.has_language = true;
                true
            }
            None => {
                self.worker.set_language(None);
                self.has_language = false;
                false
            }
        }
    }

    /// Swap the active theme. Next render call will use the new theme.
    pub fn set_theme(&mut self, theme: Arc<dyn Theme + Send + Sync>) {
        self.worker.set_theme(theme);
    }

    /// Ask the worker to drop the retained tree on the next parse so
    /// the next submission is a cold parse.
    pub fn reset(&mut self) {
        self.pending_reset = true;
    }

    /// Buffer a batch of engine `ContentEdit`s to be shipped to the
    /// worker on the next `submit_render`. Translates the engine's
    /// position pairs into `tree_sitter::InputEdit`s up front so the
    /// worker only does the cheap `tree.edit(...)` call.
    pub fn apply_edits(&mut self, edits: &[hjkl_engine::ContentEdit]) {
        if !self.has_language {
            return;
        }
        for e in edits {
            self.pending_edits.push(InputEdit {
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
        buffer: &impl Query,
        viewport_top: usize,
        viewport_height: usize,
    ) -> Option<u128> {
        use std::time::Instant;
        if !self.has_language {
            return None;
        }

        let dg = buffer.dirty_gen();
        let lb = buffer.len_bytes();
        let lc = buffer.line_count();
        let row_count = lc as usize;

        // Rebuild source + row_starts only when the buffer has changed.
        // Pure scroll frames skip the O(N) string join + newline scan
        // and reuse the previous Arc<String> directly.
        let needs_rebuild = match &self.cache {
            Some(c) => c.dirty_gen != dg || c.len_bytes != lb || c.line_count != lc,
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
            self.cache = Some(RenderCache {
                dirty_gen: dg,
                len_bytes: lb,
                line_count: lc,
                source: Arc::new(source),
                row_starts: Arc::new(row_starts),
            });
            source_build_us = t.elapsed().as_micros();
        }
        let cache = self.cache.as_ref().expect("cache populated above");

        // Compute viewport byte range. byte_of_row clamps past-end to
        // len_bytes so the +1 row beyond the visible range is safe.
        let bytes_len = cache.source.len();
        let vp_start = buffer.byte_of_row(viewport_top);
        let vp_end_row = viewport_top + viewport_height + 1;
        let vp_end = buffer.byte_of_row(vp_end_row).min(bytes_len);
        let vp_end = vp_end.max(vp_start);

        let edits = std::mem::take(&mut self.pending_edits);
        let reset = std::mem::replace(&mut self.pending_reset, false);
        self.last_submitted_dirty_gen = Some(dg);

        self.worker.submit(ParseRequest {
            source: Arc::clone(&cache.source),
            row_starts: Arc::clone(&cache.row_starts),
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

/// Build the default dark `SyntaxLayer`.
pub fn default_layer() -> SyntaxLayer {
    SyntaxLayer::new(Arc::new(DotFallbackTheme::dark()))
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
        layer.submit_render(buf, top, height)?;
        layer.wait_for_result(Duration::from_secs(5))
    }

    #[test]
    fn parse_and_render_small_rust_buffer() {
        let buf = Buffer::from_str("fn main() { let x = 1; }\n");
        let mut layer = default_layer();
        assert!(layer.set_language_for_path(Path::new("a.rs")));
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
        assert!(!layer.set_language_for_path(Path::new("a.unknownext")));
        assert!(layer.submit_render(&buf, 0, 10).is_none());
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
        layer.apply_edits(&edits);
        assert!(layer.pending_edits.is_empty());
    }

    #[test]
    fn first_load_highlights_entire_viewport() {
        let mut content = String::new();
        for i in 0..50 {
            content.push_str(&format!("fn f{i}() {{ let x = {i}; }}\n"));
        }
        let buf = Buffer::from_str(content.strip_suffix('\n').unwrap_or(&content));

        let mut layer = default_layer();
        assert!(layer.set_language_for_path(Path::new("a.rs")));
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
    fn first_load_full_viewport_matches_full_parse() {
        let mut content = String::new();
        for i in 0..50 {
            content.push_str(&format!("fn f{i}() {{ let x = {i}; }}\n"));
        }
        let buf = Buffer::from_str(content.strip_suffix('\n').unwrap_or(&content));

        let mut narrow = default_layer();
        narrow.set_language_for_path(Path::new("a.rs"));
        let narrow_out = submit_and_wait(&mut narrow, &buf, 0, 30).unwrap();

        let mut full = default_layer();
        full.set_language_for_path(Path::new("a.rs"));
        let full_out = submit_and_wait(&mut full, &buf, 0, 100).unwrap();

        for r in 0..30 {
            assert_eq!(
                narrow_out.spans[r], full_out.spans[r],
                "row {r} differs between viewport-scoped and full parse"
            );
        }
    }

    #[test]
    fn diagnostics_emit_sign_for_syntax_error() {
        let buf = Buffer::from_str("fn main() {\nlet x = ;\n}\n");
        let mut layer = default_layer();
        layer.set_language_for_path(Path::new("a.rs"));
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
    fn diagnostics_clean_source_no_signs() {
        let buf = Buffer::from_str("fn main() { let x = 1; }\n");
        let mut layer = default_layer();
        layer.set_language_for_path(Path::new("a.rs"));
        let out = submit_and_wait(&mut layer, &buf, 0, 10).unwrap();
        assert!(
            out.signs.is_empty(),
            "expected no signs; got {:?}",
            out.signs
        );
    }

    #[test]
    fn incremental_path_matches_cold_for_small_edit() {
        // Pre: parse a buffer through the worker.
        let pre = Buffer::from_str("fn main() { let x = 1; }");
        let mut layer = default_layer();
        layer.set_language_for_path(Path::new("a.rs"));
        let _ = submit_and_wait(&mut layer, &pre, 0, 10).unwrap();

        // Apply an edit: insert "Y" at byte 3 ("fn ⎀main…").
        layer.apply_edits(&[hjkl_engine::ContentEdit {
            start_byte: 3,
            old_end_byte: 3,
            new_end_byte: 4,
            start_position: (0, 3),
            old_end_position: (0, 3),
            new_end_position: (0, 4),
        }]);
        let post = Buffer::from_str("fn Ymain() { let x = 1; }");
        let inc = submit_and_wait(&mut layer, &post, 0, 10).unwrap();

        // Cold parse from a fresh layer.
        let mut cold_layer = default_layer();
        cold_layer.set_language_for_path(Path::new("a.rs"));
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

    #[test]
    fn reset_pending_request_is_consumed_once() {
        let buf = Buffer::from_str("fn main() {}");
        let mut layer = default_layer();
        layer.set_language_for_path(Path::new("a.rs"));
        layer.reset();
        assert!(layer.pending_reset);
        let _ = submit_and_wait(&mut layer, &buf, 0, 10).unwrap();
        assert!(
            !layer.pending_reset,
            "pending_reset should clear after submit"
        );
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
    fn big_rs_smoke() {
        let path = Path::new("/tmp/big.rs");
        if !path.exists() {
            eprintln!("/tmp/big.rs not present; skipping perf smoke");
            return;
        }
        let content = std::fs::read_to_string(path).unwrap();
        let buf = Buffer::from_str(content.strip_suffix('\n').unwrap_or(&content));
        let mut layer = default_layer();
        assert!(layer.set_language_for_path(path));

        let t0 = Instant::now();
        layer.submit_render(&buf, 0, 50);
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
            layer.submit_render(&buf, top * 100, 50);
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
        layer.apply_edits(&[hjkl_engine::ContentEdit {
            start_byte: edit_byte,
            old_end_byte: edit_byte,
            new_end_byte: edit_byte + 1,
            start_position: (50_000, 0),
            old_end_position: (50_000, 0),
            new_end_position: (50_000, 1),
        }]);
        let t = Instant::now();
        layer.submit_render(&post, 0, 50);
        let main_us = t.elapsed();
        let out = layer.wait_for_result(Duration::from_secs(10));
        eprintln!(
            "post-edit submit: main-thread {:?}, worker total {:?} (per-step: {:?})",
            main_us,
            t.elapsed(),
            out.as_ref().map(|o| o.perf),
        );
    }
}
