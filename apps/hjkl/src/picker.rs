//! Modal fuzzy picker — popup overlay over the editor pane.
//!
//! Generic over a [`PickerSource`] so the same UI hosts file pickers,
//! buffer pickers, grep pickers, etc. The current concrete source is
//! [`FileSource`] (gitignore-aware cwd walk).
//!
//! Triggered by `<leader><space>` / `<leader>f`, the `:picker` ex
//! command, or the `+picker` startup arg. Uses
//! [`hjkl_form::TextFieldEditor`] for the query input (vim grammar
//! inside the prompt) and a background thread (when the source needs
//! one) to stream candidates in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hjkl_buffer::{Buffer, Span as BufferSpan};
use hjkl_form::{Input as EngineInput, Key as EngineKey, TextFieldEditor};
use hjkl_tree_sitter::{DotFallbackTheme, HighlightSpan, Highlighter, LanguageRegistry, Theme};
use ratatui::style::Style as RatStyle;

/// Cap preview reads at this many lines so giant files don't stall the
/// render path.
const PREVIEW_MAX_LINES: usize = 200;
/// Skip preview entirely past this byte count — likely a binary or
/// large generated artefact that wouldn't render usefully anyway.
const PREVIEW_MAX_BYTES: u64 = 1_000_000;

/// Action emitted when the user picks an item. The App dispatches each
/// variant to the right machinery.
pub enum PickerAction {
    /// Open the path in the editor (routes through `do_edit`).
    OpenPath(PathBuf),
    /// Switch to an already-open buffer slot by index.
    SwitchBuffer(usize),
}

/// Per-row span table + style table for the preview pane. The
/// `BufferView` consumer takes `Vec<Vec<Span>>` plus a resolver
/// closure mapping `style: u32` → ratatui `Style`; both live here so
/// the renderer can wire them together cheaply.
#[derive(Default)]
pub struct PreviewSpans {
    /// One vec per buffer row, each entry covering a half-open byte
    /// range with an opaque style id.
    pub by_row: Vec<Vec<BufferSpan>>,
    /// Style id → ratatui style. Index with the `style` field of each
    /// `BufferSpan`.
    pub styles: Vec<RatStyle>,
}

/// Source-side abstraction for one kind of picker. Implementations
/// stream items into the picker's `ItemSink` (synchronously or via a
/// background thread), describe each item for the list and the match
/// scorer, build a preview, and convert a selected item into a
/// [`PickerAction`].
pub trait PickerSource: Send + Sync + 'static {
    /// Per-row payload. `Send + Sync` so the scan can run on a worker;
    /// `Clone` so the picker can hand selected items back to the app
    /// without lock-spanning.
    type Item: Clone + Send + Sync + 'static;

    /// Title shown above the input row (e.g. "files", "buffers").
    fn title(&self) -> &'static str;

    /// Display label for the list row.
    fn label(&self, item: &Self::Item) -> String;

    /// Text used for fuzzy match scoring. Often the same as `label`,
    /// but a buffer source might want to score against the path while
    /// labelling with extra metadata (`● src/main.rs (modified)`).
    fn match_text(&self, item: &Self::Item) -> String;

    /// Whether this source wants the preview pane. Sources without
    /// useful per-item content (command palettes, simple action lists)
    /// override to `false` and skip implementing `preview`.
    fn has_preview(&self) -> bool {
        true
    }

    /// Build the preview-pane content. Returns `(buffer, status,
    /// spans)`. Status is empty for normal content; non-empty is a
    /// placeholder reason ("binary", "1.0MB — too large", I/O error
    /// message). Spans are an empty `PreviewSpans` when the source
    /// doesn't tokenise (in which case the preview renders unstyled).
    /// Only called when `has_preview()` returns `true`.
    fn preview(&self, _item: &Self::Item) -> (Buffer, String, PreviewSpans) {
        (Buffer::new(), String::new(), PreviewSpans::default())
    }

    /// Translate the highlighted item into an action when the user
    /// presses Enter.
    fn select(&self, item: &Self::Item) -> PickerAction;

    /// Stream items into `sink`. Sources with synchronous data should
    /// push everything and call `sink.finish()` inline (returning
    /// `None`); sources doing I/O should spawn a thread and return its
    /// handle so the picker can hold liveness for as long as it's
    /// open.
    fn enumerate(self: Arc<Self>, sink: ItemSink<Self::Item>) -> Option<JoinHandle<()>>;
}

/// Channel-like sink into the picker's candidate buffer. Sources push
/// items here from `enumerate`; the picker reads via its own
/// `Arc<Mutex<Vec<Item>>>` snapshot.
pub struct ItemSink<I> {
    items: Arc<Mutex<Vec<I>>>,
    done: Arc<AtomicBool>,
}

impl<I> ItemSink<I> {
    /// Append a single item.
    #[allow(dead_code)] // Future sources may push one-at-a-time.
    pub fn push(&self, item: I) {
        if let Ok(mut g) = self.items.lock() {
            g.push(item);
        }
    }

    /// Append an iterator of items in one lock acquisition. Preferred
    /// over per-item `push` when the source can batch.
    pub fn extend(&self, items: impl IntoIterator<Item = I>) {
        if let Ok(mut g) = self.items.lock() {
            g.extend(items);
        }
    }

    /// Mark the scan as complete. The picker shows a "scanning…"
    /// indicator until this is called.
    pub fn finish(&self) {
        self.done.store(true, Ordering::Release);
    }
}

/// Outcome of routing one key event into the picker.
pub enum PickerEvent {
    /// Key consumed; picker stays open.
    None,
    /// User dismissed the picker.
    Cancel,
    /// User picked an item — dispatch this action.
    Select(PickerAction),
}

/// Generic picker state. Lives in `App::picker` while open.
pub struct Picker<S: PickerSource> {
    /// Query input — vim modal text field. Lands in Insert at open so
    /// the user types immediately.
    pub query: TextFieldEditor,
    /// Source providing the items, labels, preview, and select action.
    source: Arc<S>,
    /// Discovered items (source's `enumerate` appends; picker reads).
    items: Arc<Mutex<Vec<S::Item>>>,
    /// Indices into `items` ranked by score for the current query.
    filtered: Vec<usize>,
    /// Selection index into `filtered`.
    pub selected: usize,
    /// Set to `true` by the source when its enumeration finishes.
    scan_done: Arc<AtomicBool>,
    /// Last query string the filter ran against.
    last_query: String,
    /// Last `items.len()` the filter ran against.
    last_seen_count: usize,
    /// Background scan thread (when the source spawned one). Held for
    /// liveness only.
    _scan: Option<JoinHandle<()>>,
    /// Index into `items` whose preview is currently cached.
    preview_idx: Option<usize>,
    /// Cached preview content. Empty when nothing is selected.
    preview_buffer: Buffer,
    /// Status tag for the preview pane title.
    preview_status: String,
    /// Cached label for the preview header (saves a clone-from-Item
    /// each render).
    preview_label: Option<String>,
    /// Per-row spans + style table for the preview buffer. Empty when
    /// the source doesn't tokenise.
    preview_spans: PreviewSpans,
}

impl<S: PickerSource> Picker<S> {
    /// Build a new picker over `source`. Kicks off enumeration
    /// immediately so candidates start streaming in before the user
    /// types their first character.
    pub fn new(source: S) -> Self {
        let source = Arc::new(source);
        let items = Arc::new(Mutex::new(Vec::<S::Item>::new()));
        let scan_done = Arc::new(AtomicBool::new(false));
        let sink = ItemSink {
            items: Arc::clone(&items),
            done: Arc::clone(&scan_done),
        };
        let handle = Arc::clone(&source).enumerate(sink);

        let mut query = TextFieldEditor::new(true);
        query.enter_insert_at_end();

        let mut me = Self {
            query,
            source,
            items,
            filtered: Vec::new(),
            selected: 0,
            scan_done,
            last_query: String::new(),
            last_seen_count: 0,
            _scan: handle,
            preview_idx: None,
            preview_buffer: Buffer::new(),
            preview_status: String::new(),
            preview_label: None,
            preview_spans: PreviewSpans::default(),
        };
        // Block briefly for the first batch of items so the first
        // render already has a populated list and a loaded preview —
        // otherwise the preview pane stays blank until the next event-
        // loop tick (~120ms) catches up to the streaming scan.
        me.wait_for_items(std::time::Duration::from_millis(30));
        me.refresh();
        me.refresh_preview();
        me
    }

    /// Spin up to `timeout` waiting for the source to push at least
    /// one item. Cheap polling — short enough that even synchronous
    /// sources (which call `finish()` before returning from
    /// `enumerate`) just see the first poll return.
    fn wait_for_items(&self, timeout: std::time::Duration) {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Ok(g) = self.items.lock()
                && !g.is_empty()
            {
                return;
            }
            if self.scan_done.load(Ordering::Acquire) {
                return;
            }
            if std::time::Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Title from the source (e.g. "files").
    pub fn title(&self) -> &'static str {
        self.source.title()
    }

    /// Whether the source wants a preview pane rendered.
    pub fn has_preview(&self) -> bool {
        self.source.has_preview()
    }

    /// True once the source has signalled `finish()`.
    pub fn scan_done(&self) -> bool {
        self.scan_done.load(Ordering::Acquire)
    }

    /// Total candidate count (regardless of filter).
    pub fn total(&self) -> usize {
        self.items.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Number of candidates currently passing the query filter.
    pub fn matched(&self) -> usize {
        self.filtered.len()
    }

    /// Re-run the filter if the query or candidate count changed.
    /// Returns `true` when `filtered` was rebuilt.
    pub fn refresh(&mut self) -> bool {
        let items = match self.items.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let q = self.query.text();
        let q_changed = q != self.last_query;
        let count_changed = items.len() != self.last_seen_count;
        if !q_changed && !count_changed {
            return false;
        }
        self.last_query.clone_from(&q);
        self.last_seen_count = items.len();

        let q_lower = q.to_lowercase();
        let mut scored: Vec<(i64, usize, String)> = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let m = self.source.match_text(item);
            let m_lower = m.to_lowercase();
            let sc = if q.is_empty() {
                0
            } else {
                match score(&m_lower, &q_lower) {
                    Some(v) => v,
                    None => continue,
                }
            };
            scored.push((sc, i, m_lower));
        }
        // Score desc; ties broken by lowercased match text asc so the
        // ordering is stable across renders even when the source's
        // emission order changes.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
        scored.truncate(500);
        self.filtered = scored.into_iter().map(|(_, i, _)| i).collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
        true
    }

    /// Refresh the preview if the selection now points at a different
    /// item than the cached one. No-op when the source opted out of
    /// previews via `has_preview() == false`.
    pub fn refresh_preview(&mut self) {
        if !self.source.has_preview() {
            return;
        }
        let target_idx = self.filtered.get(self.selected).copied();
        if target_idx == self.preview_idx {
            return;
        }
        self.preview_idx = target_idx;
        let Some(idx) = target_idx else {
            self.preview_buffer = Buffer::new();
            self.preview_status.clear();
            self.preview_label = None;
            self.preview_spans = PreviewSpans::default();
            return;
        };
        // Snapshot the item so we can drop the lock before calling
        // `source.preview` (which may do disk I/O).
        let item = match self.items.lock() {
            Ok(g) => match g.get(idx) {
                Some(i) => i.clone(),
                None => return,
            },
            Err(_) => return,
        };
        let label = self.source.label(&item);
        let (buf, status, spans) = self.source.preview(&item);
        self.preview_buffer = buf;
        self.preview_status = status;
        self.preview_label = Some(label);
        self.preview_spans = spans;
    }

    /// Per-row spans + style table for the preview pane.
    pub fn preview_spans(&self) -> &PreviewSpans {
        &self.preview_spans
    }

    /// Borrow the preview buffer for `BufferView` rendering.
    pub fn preview_buffer(&self) -> &Buffer {
        &self.preview_buffer
    }

    /// Status tag (`"binary"`, `"1.2MB — too large"`, …). Empty when
    /// the preview is normal content.
    pub fn preview_status(&self) -> &str {
        &self.preview_status
    }

    /// Label of the item currently in the preview (for the header).
    pub fn preview_label(&self) -> Option<&str> {
        self.preview_label.as_deref()
    }

    /// Labels for the first `n` filtered items — what the renderer
    /// puts in the list rows. Avoids exposing `S::Item` to render
    /// code.
    pub fn visible_labels(&self, n: usize) -> Vec<String> {
        let items = match self.items.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        self.filtered
            .iter()
            .take(n)
            .filter_map(|&i| items.get(i).map(|it| self.source.label(it)))
            .collect()
    }

    /// Action for the currently highlighted item, if any.
    fn selected_action(&self) -> Option<PickerAction> {
        let idx = *self.filtered.get(self.selected)?;
        let items = self.items.lock().ok()?;
        let item = items.get(idx)?;
        Some(self.source.select(item))
    }

    /// Route a key event. Special keys (Esc / Enter / C-n / C-p / Up /
    /// Down) drive picker navigation; everything else forwards to the
    /// query field's vim FSM.
    pub fn handle_key(&mut self, key: KeyEvent) -> PickerEvent {
        if key.code == KeyCode::Esc {
            return PickerEvent::Cancel;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return PickerEvent::Cancel;
        }

        if key.code == KeyCode::Enter {
            return match self.selected_action() {
                Some(a) => PickerEvent::Select(a),
                None => PickerEvent::None,
            };
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Down => {
                self.move_selection(1);
                return PickerEvent::None;
            }
            KeyCode::Up => {
                self.move_selection(-1);
                return PickerEvent::None;
            }
            KeyCode::Char('n') if ctrl => {
                self.move_selection(1);
                return PickerEvent::None;
            }
            KeyCode::Char('p') if ctrl => {
                self.move_selection(-1);
                return PickerEvent::None;
            }
            _ => {}
        }

        let input: EngineInput = key.into();
        if input.key == EngineKey::Enter || input.key == EngineKey::Esc {
            return PickerEvent::None;
        }
        self.query.handle_input(input);
        PickerEvent::None
    }

    fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            self.selected = 0;
            return;
        }
        let len = self.filtered.len() as i32;
        let next = self.selected as i32 + delta;
        let wrapped = next.rem_euclid(len);
        self.selected = wrapped as usize;
    }
}

/// Convenience alias for the file-picker form, since it's currently
/// the only concrete source. New sources get their own alias next to
/// `FileSource`.
pub type FilePicker = Picker<FileSource>;

/// Convenience alias for the buffer-picker.
pub type BufferPicker = Picker<BufferSource>;

/// Snapshot of one open buffer slot for use in the buffer picker.
#[derive(Clone)]
pub struct BufferEntry {
    /// Index into `App::slots`.
    pub idx: usize,
    /// Display name (filename or `[No Name]`).
    pub name: String,
    /// `true` when the buffer has unsaved changes.
    pub dirty: bool,
}

/// Source for the buffer picker. Enumerates all open slots at the
/// moment the picker is opened — the picker is modal so this snapshot
/// is fine.
pub struct BufferSource {
    entries: Vec<BufferEntry>,
}

impl BufferSource {
    /// Build from a slice of open `BufferSlot`s.
    pub fn new<S>(
        slots: &[S],
        name_of: impl Fn(&S) -> String,
        dirty_of: impl Fn(&S) -> bool,
    ) -> Self {
        let entries = slots
            .iter()
            .enumerate()
            .map(|(idx, s)| BufferEntry {
                idx,
                name: name_of(s),
                dirty: dirty_of(s),
            })
            .collect();
        Self { entries }
    }
}

impl PickerSource for BufferSource {
    type Item = BufferEntry;

    fn title(&self) -> &'static str {
        "buffers"
    }

    fn label(&self, item: &BufferEntry) -> String {
        if item.dirty {
            format!("● {}", item.name)
        } else {
            format!("  {}", item.name)
        }
    }

    fn match_text(&self, item: &BufferEntry) -> String {
        item.name.clone()
    }

    /// Buffer picker has no per-item disk content to preview.
    /// TODO: add a live-buffer snapshot preview in a future iteration.
    fn has_preview(&self) -> bool {
        false
    }

    fn select(&self, item: &BufferEntry) -> PickerAction {
        PickerAction::SwitchBuffer(item.idx)
    }

    fn enumerate(self: Arc<Self>, sink: ItemSink<Self::Item>) -> Option<JoinHandle<()>> {
        // All entries are in memory — push synchronously and finish.
        sink.extend(self.entries.iter().cloned());
        sink.finish();
        None
    }
}

/// File-source: gitignore-aware cwd walker. Items are paths relative
/// to `root`, preview reads from disk capped at `PREVIEW_MAX_LINES` /
/// `PREVIEW_MAX_BYTES` with a binary-byte heuristic.
pub struct FileSource {
    root: PathBuf,
    /// Tree-sitter language registry; preview detects language by
    /// extension here.
    registry: LanguageRegistry,
    /// Theme used to resolve capture names to ratatui styles.
    theme: Arc<dyn Theme + Send + Sync>,
    /// Per-language `Highlighter` cache keyed by `LanguageConfig.name`.
    /// Compiling a `Query` is the costly part (~10-20ms for Rust); we
    /// pay that once per language and reuse the highlighter for every
    /// subsequent preview of the same file type.
    highlighters: Mutex<HashMap<&'static str, Highlighter>>,
}

impl FileSource {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            registry: LanguageRegistry::new(),
            theme: Arc::new(DotFallbackTheme::dark()),
            highlighters: Mutex::new(HashMap::new()),
        }
    }
}

impl PickerSource for FileSource {
    type Item = PathBuf;

    fn title(&self) -> &'static str {
        "files"
    }

    fn label(&self, item: &PathBuf) -> String {
        item.to_string_lossy().into_owned()
    }

    fn match_text(&self, item: &PathBuf) -> String {
        item.to_string_lossy().into_owned()
    }

    fn preview(&self, item: &PathBuf) -> (Buffer, String, PreviewSpans) {
        let abs = self.root.join(item);
        let (content, status) = load_preview(&abs);
        if !status.is_empty() {
            return (Buffer::from_str(&content), status, PreviewSpans::default());
        }
        let spans = self.highlight(&abs, &content);
        (Buffer::from_str(&content), status, spans)
    }

    fn select(&self, item: &PathBuf) -> PickerAction {
        PickerAction::OpenPath(item.clone())
    }

    fn enumerate(self: Arc<Self>, sink: ItemSink<Self::Item>) -> Option<JoinHandle<()>> {
        let me = self;
        thread::Builder::new()
            .name("hjkl-picker-scan".into())
            .spawn(move || scan_walk(me.root.as_path(), &sink))
            .ok()
    }
}

impl FileSource {
    /// Highlight the preview content. Detects language from the path,
    /// gets/compiles a `Highlighter` for it, parses the (already
    /// truncated) source, and builds per-row spans plus a style table
    /// the renderer can index into via the `Span.style` field.
    ///
    /// Returns an empty `PreviewSpans` for unknown languages or on any
    /// parse / lock error — preview still renders, just unstyled.
    fn highlight(&self, abs: &Path, content: &str) -> PreviewSpans {
        let Some(cfg) = self.registry.detect_for_path(abs) else {
            return PreviewSpans::default();
        };
        let mut hl_cache = match self.highlighters.lock() {
            Ok(g) => g,
            Err(_) => return PreviewSpans::default(),
        };
        let h = match hl_cache.entry(cfg.name) {
            std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => match Highlighter::new(cfg) {
                Ok(h) => v.insert(h),
                Err(_) => return PreviewSpans::default(),
            },
        };
        h.reset();
        let bytes = content.as_bytes();
        h.parse_initial(bytes);
        let flat: Vec<HighlightSpan> = h.highlight_range(bytes, 0..bytes.len());

        // Row starts (offset of the first byte of each row). Mirrors
        // the helper in `syntax.rs`.
        let mut row_starts: Vec<usize> = vec![0];
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                row_starts.push(i + 1);
            }
        }
        let row_count = row_starts.len();

        // Intern each unique ratatui style into `styles`; record the
        // index so multiple spans sharing a style refer to the same id.
        let mut styles: Vec<RatStyle> = Vec::new();
        let mut by_row: Vec<Vec<BufferSpan>> = vec![Vec::new(); row_count];
        for span in &flat {
            let Some(rat) = self.theme.style(span.capture()).map(|s| s.to_ratatui()) else {
                continue;
            };
            let style_id = match styles.iter().position(|s| *s == rat) {
                Some(i) => i,
                None => {
                    styles.push(rat);
                    styles.len() - 1
                }
            } as u32;
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
                    by_row[row].push(BufferSpan::new(local_start, local_end, style_id));
                }
                row += 1;
            }
        }
        PreviewSpans { by_row, styles }
    }
}

/// Load a single file for the preview pane. Returns `(content,
/// status)`: when `status` is non-empty the file was skipped (binary /
/// oversized / I/O error) and `content` is empty.
fn load_preview(abs: &Path) -> (String, String) {
    let meta = match std::fs::metadata(abs) {
        Ok(m) => m,
        Err(e) => return (String::new(), format!("{e}")),
    };
    if meta.len() > PREVIEW_MAX_BYTES {
        let mb = meta.len() as f64 / 1_048_576.0;
        return (String::new(), format!("{mb:.1}MB — too large"));
    }
    let bytes = match std::fs::read(abs) {
        Ok(b) => b,
        Err(e) => return (String::new(), format!("{e}")),
    };
    let scan_end = bytes.len().min(8192);
    if bytes[..scan_end].contains(&0u8) {
        return (String::new(), "binary".into());
    }
    let text = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => return (String::new(), "non-utf8".into()),
    };
    let truncated: String = text
        .lines()
        .take(PREVIEW_MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    (truncated, String::new())
}

/// Background walker — streams `is_file()` entries into `sink`,
/// gitignore-aware via `ignore::WalkBuilder`. Calls `sink.finish()`
/// on completion so the picker can stop showing "scanning…".
fn scan_walk(root: &Path, sink: &ItemSink<PathBuf>) {
    let walk = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .parents(true)
        .build();
    let mut batch: Vec<PathBuf> = Vec::with_capacity(256);
    let mut total = 0usize;
    const HARD_CAP: usize = 50_000;
    for entry in walk {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_file() {
            continue;
        }
        let path = entry.into_path();
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_path_buf())
            .unwrap_or(path);
        batch.push(rel);
        total += 1;
        if batch.len() >= 256 {
            sink.extend(batch.drain(..));
        }
        if total >= HARD_CAP {
            break;
        }
    }
    sink.extend(batch.drain(..));
    sink.finish();
}

/// Subsequence-based fuzzy score. Returns `None` when not all needle
/// characters appear (in order) in the haystack.
///
/// Bonuses:
/// - `+8` per match at a word boundary (start, after `/`, `_`, `-`,
///   `.`, ` `).
/// - `+5` per consecutive match (run of adjacent matches).
/// - `+1` base hit per matched char.
///
/// Penalty: `-len(haystack)/8` so shorter overall paths win on ties.
fn score(haystack: &str, needle: &str) -> Option<i64> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    let mut hi = 0usize;
    let mut ni = 0usize;
    let mut total: i64 = 0;
    let mut prev_match = false;
    while ni < n.len() && hi < h.len() {
        if h[hi] == n[ni] {
            if prev_match {
                total += 5;
            }
            let at_boundary = hi == 0 || matches!(h[hi - 1], b'/' | b'_' | b'-' | b'.' | b' ');
            if at_boundary {
                total += 8;
            }
            total += 1;
            prev_match = true;
            ni += 1;
        } else {
            prev_match = false;
        }
        hi += 1;
    }
    if ni < n.len() {
        return None;
    }
    total -= h.len() as i64 / 8;
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_subsequence_match() {
        assert!(score("src/main.rs", "main").is_some());
        assert!(score("src/main.rs", "smr").is_some());
        assert!(score("src/main.rs", "xyz").is_none());
    }

    #[test]
    fn score_word_boundary_beats_mid_word() {
        let a = score("src/main.rs", "main").unwrap();
        let b = score("src/domain.rs", "main").unwrap();
        assert!(a > b);
    }

    #[test]
    fn score_shorter_wins_on_ties() {
        let a = score("a/b/foo.rs", "foo").unwrap();
        let b = score("a/b/c/d/e/foo.rs", "foo").unwrap();
        assert!(a > b);
    }

    #[test]
    fn txt_preview_has_no_highlight_spans() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("notes.txt");
        std::fs::write(&path, "hello world\nthis is plain text\n").unwrap();
        let source = FileSource::new(tmp.path().to_path_buf());
        let (_buf, status, spans) = source.preview(&PathBuf::from("notes.txt"));
        assert!(status.is_empty(), "unexpected status: {status:?}");
        assert!(spans.styles.is_empty(), "got {} styles", spans.styles.len());
        for row in &spans.by_row {
            assert!(row.is_empty(), "unexpected spans on row: {row:?}");
        }
    }
}
