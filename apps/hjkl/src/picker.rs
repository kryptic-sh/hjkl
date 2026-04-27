//! Modal fuzzy picker — popup overlay over the editor pane.
//!
//! Non-generic: the picker holds a `Box<dyn PickerLogic>` so new sources
//! (file, buffer, grep, …) can be added without touching any enum or match
//! arm elsewhere in the codebase.
//!
//! Triggered by `<leader><space>` / `<leader>f`, `:picker`, `<Space>/`,
//! `:rg <pattern>`, etc.  Uses [`hjkl_form::TextFieldEditor`] for the query
//! input (vim grammar inside the prompt) and a background thread (when the
//! source spawns one) to stream candidates in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

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

/// Debounce delay for `RequeryMode::Spawn` sources (milliseconds).
const REQUERY_DEBOUNCE_MS: u64 = 150;

/// Action emitted when the user picks an item. The App dispatches each
/// variant to the right machinery.
pub enum PickerAction {
    /// Open the path in the editor (routes through `do_edit`).
    OpenPath(PathBuf),
    /// Switch to an already-open buffer slot by index.
    SwitchBuffer(usize),
    /// Open the path at a specific 1-based line number.
    OpenPathAtLine(PathBuf, u32),
    /// No-op action (used for error sentinel items).
    None,
}

/// How the picker reacts when the query string changes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RequeryMode {
    /// Filter the existing in-memory item vec. `enumerate` is called once
    /// at open with `query = None`; subsequent query changes just re-score.
    FilterInMemory,
    /// Re-spawn the source for every debounced query change. `enumerate` is
    /// called with `query = Some(q)` after each debounce interval; the
    /// source resets its item vec each time.
    Spawn,
}

/// Fully-erased source for one kind of picker. The picker only talks to
/// the source via opaque `usize` indices into the source's internal item vec.
pub trait PickerLogic: Send + 'static {
    /// Title shown above the input row (e.g. "files", "buffers", "grep").
    fn title(&self) -> &str;

    /// Number of items currently available (grows as enumeration progresses).
    fn item_count(&self) -> usize;

    /// Display label for the row at `idx`.
    fn label(&self, idx: usize) -> String;

    /// Text the fuzzy scorer scores against. May equal `label`.
    fn match_text(&self, idx: usize) -> String;

    /// Whether this source wants the preview pane.
    fn has_preview(&self) -> bool {
        true
    }

    /// Build the preview pane for the row. Default: empty buffer.
    fn preview(&self, idx: usize) -> (Buffer, String, PreviewSpans) {
        let _ = idx;
        (Buffer::new(), String::new(), PreviewSpans::default())
    }

    /// Translate the picked row into an action.
    fn select(&self, idx: usize) -> PickerAction;

    /// How the picker should react when the query changes.
    fn requery_mode(&self) -> RequeryMode {
        RequeryMode::FilterInMemory
    }

    /// Re-enumerate items.
    ///
    /// - `FilterInMemory` sources: called once at open with `query = None`.
    /// - `Spawn` sources: called on every debounced query change with
    ///   `query = Some(q)`. Must reset the internal item vec before pushing
    ///   new items.
    ///
    /// `cancel` is set to `true` by the picker when a newer requery
    /// supersedes this one — long-running threads should poll it and bail.
    fn enumerate(&mut self, query: Option<&str>, cancel: Arc<AtomicBool>)
    -> Option<JoinHandle<()>>;
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

/// Outcome of routing one key event into the picker.
pub enum PickerEvent {
    /// Key consumed; picker stays open.
    None,
    /// User dismissed the picker.
    Cancel,
    /// User picked an item — dispatch this action.
    Select(PickerAction),
}

/// One entry in the filtered/ranked list. Stores the index into the
/// source item vec together with the char positions that satisfied the
/// fuzzy match (used by the renderer to highlight matched chars).
pub(crate) struct FilteredEntry {
    /// Index into the source's internal item vec.
    pub idx: usize,
    /// Char-indices in the item's label where needle chars matched.
    /// Empty when the query is empty (no highlight needed).
    pub matches: Vec<usize>,
}

/// Non-generic picker state. Lives in `App::picker` while open.
pub struct Picker {
    /// Query input — vim modal text field. Lands in Insert at open so
    /// the user types immediately.
    pub query: TextFieldEditor,
    /// Source providing the items, labels, preview, and select action.
    source: Box<dyn PickerLogic>,
    /// Ranked filtered entries for the current query.
    filtered: Vec<FilteredEntry>,
    /// Selection index into `filtered`.
    pub selected: usize,
    /// Last query string the filter ran against.
    last_query: String,
    /// Last item count the filter ran against.
    last_seen_count: usize,
    /// Cancel flag for the current background scan.
    cancel: Arc<AtomicBool>,
    /// Background scan thread (when the source spawned one). Held for
    /// liveness only.
    _scan: Option<JoinHandle<()>>,
    /// Debounce timestamp: fire requery when `Instant::now() >= requery_at`.
    requery_at: Option<Instant>,
    /// Index into the source whose preview is currently cached.
    preview_idx: Option<usize>,
    /// Cached preview content. Empty when nothing is selected.
    preview_buffer: Buffer,
    /// Status tag for the preview pane title.
    preview_status: String,
    /// Cached label for the preview header.
    preview_label: Option<String>,
    /// Per-row spans + style table for the preview buffer.
    preview_spans: PreviewSpans,
}

impl Picker {
    /// Build a new picker over `source`. Kicks off enumeration immediately
    /// so candidates start streaming in before the user types their first
    /// character.
    pub fn new(mut source: Box<dyn PickerLogic>) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = source.enumerate(None, Arc::clone(&cancel));

        let mut query = TextFieldEditor::new(true);
        query.enter_insert_at_end();

        let mut me = Self {
            query,
            source,
            filtered: Vec::new(),
            selected: 0,
            last_query: String::new(),
            last_seen_count: 0,
            cancel,
            _scan: handle,
            requery_at: None,
            preview_idx: None,
            preview_buffer: Buffer::new(),
            preview_status: String::new(),
            preview_label: None,
            preview_spans: PreviewSpans::default(),
        };
        // Block briefly for the first batch of items so the first
        // render already has a populated list and a loaded preview.
        me.wait_for_items(Duration::from_millis(30));
        me.refresh();
        me.refresh_preview();
        me
    }

    /// Build a new picker with a pre-populated query string.
    pub fn new_with_query(source: Box<dyn PickerLogic>, initial_query: &str) -> Self {
        let mut me = Self::new(source);
        me.query.set_text(initial_query);
        me.refresh();
        me.refresh_preview();
        me
    }

    /// Spin up to `timeout` waiting for the source to push at least one item.
    fn wait_for_items(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if self.source.item_count() > 0 {
                return;
            }
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Title from the source.
    pub fn title(&self) -> &str {
        self.source.title()
    }

    /// Whether the source wants a preview pane rendered.
    pub fn has_preview(&self) -> bool {
        self.source.has_preview()
    }

    /// True once the background thread has finished (or none was started).
    pub fn scan_done(&self) -> bool {
        self._scan.as_ref().map(|h| h.is_finished()).unwrap_or(true)
    }

    /// Total candidate count (regardless of filter).
    pub fn total(&self) -> usize {
        self.source.item_count()
    }

    /// Number of candidates currently passing the query filter.
    pub fn matched(&self) -> usize {
        self.filtered.len()
    }

    /// Tick — called each render frame. Handles debounce expiry for
    /// `RequeryMode::Spawn` sources.
    pub fn tick(&mut self, now: Instant) {
        if self.source.requery_mode() != RequeryMode::Spawn {
            return;
        }
        let Some(at) = self.requery_at else { return };
        if now < at {
            return;
        }
        self.requery_at = None;
        // Signal previous scan to stop.
        self.cancel.store(true, Ordering::Release);
        let new_cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Arc::clone(&new_cancel);
        let q = self.query.text();
        let handle = self.source.enumerate(Some(&q), new_cancel);
        self._scan = handle;
        // Reset filter state so refresh re-scores against new items.
        self.filtered.clear();
        self.selected = 0;
        self.last_query.clear();
        self.last_seen_count = 0;
        self.preview_idx = None;
    }

    /// Re-run the filter if the query or candidate count changed.
    /// Returns `true` when `filtered` was rebuilt.
    pub fn refresh(&mut self) -> bool {
        let count = self.source.item_count();
        let q = self.query.text();
        let q_changed = q != self.last_query;
        let count_changed = count != self.last_seen_count;
        if !q_changed && !count_changed {
            return false;
        }

        // For Spawn sources, a query change schedules a requery rather than
        // filtering in-memory.
        if self.source.requery_mode() == RequeryMode::Spawn && q_changed {
            self.requery_at = Some(Instant::now() + Duration::from_millis(REQUERY_DEBOUNCE_MS));
        }

        self.last_query.clone_from(&q);
        self.last_seen_count = count;

        let q_lower = q.to_lowercase();
        let mut scored: Vec<(i64, usize, String, Vec<usize>)> = Vec::new();
        for i in 0..count {
            let m = self.source.match_text(i);
            let m_lower = m.to_lowercase();
            let (sc, positions) = if q.is_empty() {
                (0i64, Vec::new())
            } else {
                match score(&m_lower, &q_lower) {
                    Some(v) => v,
                    None => continue,
                }
            };
            scored.push((sc, i, m_lower, positions));
        }
        // Score desc; ties broken by lowercased match text asc.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
        scored.truncate(500);
        self.filtered = scored
            .into_iter()
            .map(|(_, idx, _, matches)| FilteredEntry { idx, matches })
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
        true
    }

    /// Refresh the preview if the selection now points at a different item
    /// than the cached one.
    pub fn refresh_preview(&mut self) {
        if !self.source.has_preview() {
            return;
        }
        let target_idx = self.filtered.get(self.selected).map(|e| e.idx);
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
        let label = self.source.label(idx);
        let (buf, status, spans) = self.source.preview(idx);
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

    /// Status tag. Empty when the preview is normal content.
    pub fn preview_status(&self) -> &str {
        &self.preview_status
    }

    /// Label of the item currently in the preview (for the header).
    pub fn preview_label(&self) -> Option<&str> {
        self.preview_label.as_deref()
    }

    /// Labels and fuzzy-match char positions for the first `n` filtered items.
    pub fn visible_entries(&self, n: usize) -> Vec<(String, Vec<usize>)> {
        self.filtered
            .iter()
            .take(n)
            .map(|e| (self.source.label(e.idx), e.matches.clone()))
            .collect()
    }

    /// Action for the currently highlighted item, if any.
    fn selected_action(&self) -> Option<PickerAction> {
        let idx = self.filtered.get(self.selected)?.idx;
        Some(self.source.select(idx))
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

// ── BufferSource ─────────────────────────────────────────────────────────────

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
/// moment the picker is opened.
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

impl PickerLogic for BufferSource {
    fn title(&self) -> &str {
        "buffers"
    }

    fn item_count(&self) -> usize {
        self.entries.len()
    }

    fn label(&self, idx: usize) -> String {
        match self.entries.get(idx) {
            Some(e) => {
                if e.dirty {
                    format!("● {}", e.name)
                } else {
                    format!("  {}", e.name)
                }
            }
            None => String::new(),
        }
    }

    fn match_text(&self, idx: usize) -> String {
        // Must match `label` so scorer char positions align with what the
        // renderer highlights.
        self.label(idx)
    }

    fn has_preview(&self) -> bool {
        false
    }

    fn select(&self, idx: usize) -> PickerAction {
        match self.entries.get(idx) {
            Some(e) => PickerAction::SwitchBuffer(e.idx),
            None => PickerAction::None,
        }
    }

    fn enumerate(
        &mut self,
        _query: Option<&str>,
        _cancel: Arc<AtomicBool>,
    ) -> Option<JoinHandle<()>> {
        // All entries already in memory — nothing to do.
        None
    }
}

// ── FileSource ───────────────────────────────────────────────────────────────

/// File-source: gitignore-aware cwd walker. Items are paths relative to
/// `root`, preview reads from disk capped at `PREVIEW_MAX_LINES` /
/// `PREVIEW_MAX_BYTES` with a binary-byte heuristic.
pub struct FileSource {
    root: PathBuf,
    items: Arc<Mutex<Vec<PathBuf>>>,
    scan_done: Arc<AtomicBool>,
    registry: LanguageRegistry,
    theme: Arc<dyn Theme + Send + Sync>,
    highlighters: Mutex<HashMap<&'static str, Highlighter>>,
}

impl FileSource {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            items: Arc::new(Mutex::new(Vec::new())),
            scan_done: Arc::new(AtomicBool::new(false)),
            registry: LanguageRegistry::new(),
            theme: Arc::new(DotFallbackTheme::dark()),
            highlighters: Mutex::new(HashMap::new()),
        }
    }

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
        build_preview_spans(&flat, bytes, &*self.theme)
    }
}

impl PickerLogic for FileSource {
    fn title(&self) -> &str {
        "files"
    }

    fn item_count(&self) -> usize {
        self.items.lock().map(|g| g.len()).unwrap_or(0)
    }

    fn label(&self, idx: usize) -> String {
        self.items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|p| p.to_string_lossy().into_owned()))
            .unwrap_or_default()
    }

    fn match_text(&self, idx: usize) -> String {
        self.label(idx)
    }

    fn preview(&self, idx: usize) -> (Buffer, String, PreviewSpans) {
        let path = match self.items.lock().ok().and_then(|g| g.get(idx).cloned()) {
            Some(p) => p,
            None => return (Buffer::new(), String::new(), PreviewSpans::default()),
        };
        let abs = self.root.join(&path);
        let (content, status) = load_preview(&abs);
        if !status.is_empty() {
            return (Buffer::from_str(&content), status, PreviewSpans::default());
        }
        let spans = self.highlight(&abs, &content);
        (Buffer::from_str(&content), status, spans)
    }

    fn select(&self, idx: usize) -> PickerAction {
        match self.items.lock().ok().and_then(|g| g.get(idx).cloned()) {
            Some(p) => PickerAction::OpenPath(p),
            None => PickerAction::None,
        }
    }

    fn enumerate(
        &mut self,
        _query: Option<&str>,
        cancel: Arc<AtomicBool>,
    ) -> Option<JoinHandle<()>> {
        let items = Arc::clone(&self.items);
        let done = Arc::clone(&self.scan_done);
        let root = self.root.clone();
        // Reset for re-enumerate.
        if let Ok(mut g) = items.lock() {
            g.clear();
        }
        done.store(false, Ordering::Release);
        thread::Builder::new()
            .name("hjkl-picker-scan".into())
            .spawn(move || scan_walk(&root, &items, &done, &cancel))
            .ok()
    }
}

// ── RgSource ─────────────────────────────────────────────────────────────────

/// One ripgrep match result.
pub struct RgMatch {
    pub path: PathBuf,
    pub line: u32, // 1-based
    pub _col: u32, // 1-based, byte column (reserved for future use)
    pub text: String,
}

/// Source for the ripgrep content-search picker.
pub struct RgSource {
    root: PathBuf,
    items: Arc<Mutex<Vec<RgMatch>>>,
    registry: LanguageRegistry,
    theme: Arc<dyn Theme + Send + Sync>,
    highlighters: Mutex<HashMap<&'static str, Highlighter>>,
}

impl RgSource {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            items: Arc::new(Mutex::new(Vec::new())),
            registry: LanguageRegistry::new(),
            theme: Arc::new(DotFallbackTheme::dark()),
            highlighters: Mutex::new(HashMap::new()),
        }
    }

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
        build_preview_spans(&flat, bytes, &*self.theme)
    }
}

impl PickerLogic for RgSource {
    fn title(&self) -> &str {
        "grep"
    }

    fn requery_mode(&self) -> RequeryMode {
        RequeryMode::Spawn
    }

    fn item_count(&self) -> usize {
        self.items.lock().map(|g| g.len()).unwrap_or(0)
    }

    fn label(&self, idx: usize) -> String {
        self.items
            .lock()
            .ok()
            .and_then(|g| {
                g.get(idx).map(|m| {
                    let path = m.path.display().to_string();
                    let text = if m.text.chars().count() > 80 {
                        let cut: String = m.text.chars().take(79).collect();
                        format!("{cut}…")
                    } else {
                        m.text.clone()
                    };
                    format!("{}:{}: {}", path, m.line, text)
                })
            })
            .unwrap_or_default()
    }

    fn match_text(&self, idx: usize) -> String {
        self.label(idx)
    }

    fn has_preview(&self) -> bool {
        true
    }

    fn preview(&self, idx: usize) -> (Buffer, String, PreviewSpans) {
        let (path, line) = match self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|m| (m.path.clone(), m.line)))
        {
            Some(v) => v,
            None => return (Buffer::new(), String::new(), PreviewSpans::default()),
        };
        // Sentinel: no path means rg wasn't found.
        if path.as_os_str().is_empty() {
            return (Buffer::new(), String::new(), PreviewSpans::default());
        }
        let abs = self.root.join(&path);
        let (content, status) = load_preview(&abs);
        if !status.is_empty() {
            return (Buffer::from_str(&content), status, PreviewSpans::default());
        }
        let spans = self.highlight(&abs, &content);

        // Build a window of lines around the match line. Clamp start
        // against `all_lines.len()` because rg's match line can be
        // stale relative to the file content we just read off disk.
        let all_lines: Vec<&str> = content.lines().collect();
        let match_row = (line as usize).saturating_sub(1);
        let start = match_row.saturating_sub(2).min(all_lines.len());
        let end = (start + PREVIEW_MAX_LINES).min(all_lines.len());
        let window: String = all_lines[start..end].join("\n");

        let status_tag = format!("line {line}");
        (Buffer::from_str(&window), status_tag, spans)
    }

    fn select(&self, idx: usize) -> PickerAction {
        match self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|m| (m.path.clone(), m.line)))
        {
            Some((path, line)) if !path.as_os_str().is_empty() => {
                PickerAction::OpenPathAtLine(path, line)
            }
            _ => PickerAction::None,
        }
    }

    fn enumerate(
        &mut self,
        query: Option<&str>,
        cancel: Arc<AtomicBool>,
    ) -> Option<JoinHandle<()>> {
        // Reset items for the new query.
        if let Ok(mut g) = self.items.lock() {
            g.clear();
        }

        let q = match query {
            Some(q) if !q.trim().is_empty() => q.to_owned(),
            // Empty query → show nothing.
            _ => return None,
        };

        let items = Arc::clone(&self.items);
        let root = self.root.clone();

        thread::Builder::new()
            .name("hjkl-rg-scan".into())
            .spawn(move || {
                use std::io::{BufRead, BufReader};
                use std::process::Stdio;

                let backend = detect_grep_backend();

                match backend {
                    GrepBackend::Rg => {
                        let child = std::process::Command::new("rg")
                            .args([
                                "--json",
                                "--no-config",
                                "--smart-case",
                                "--max-count",
                                "200",
                                &q,
                                root.to_str().unwrap_or("."),
                            ])
                            .stdout(Stdio::piped())
                            .stderr(Stdio::null())
                            .spawn();

                        let mut child = match child {
                            Ok(c) => c,
                            Err(_) => return,
                        };

                        let stdout = match child.stdout.take() {
                            Some(s) => s,
                            None => return,
                        };

                        let reader = BufReader::new(stdout);
                        let mut batch: Vec<RgMatch> = Vec::with_capacity(32);

                        for line_result in reader.lines() {
                            if cancel.load(Ordering::Acquire) {
                                let _ = child.kill();
                                return;
                            }
                            let line = match line_result {
                                Ok(l) => l,
                                Err(_) => continue,
                            };
                            if let Some(rg_match) = parse_rg_json_line(&line, &root) {
                                batch.push(rg_match);
                                if batch.len() >= 32
                                    && let Ok(mut g) = items.lock()
                                {
                                    g.extend(batch.drain(..));
                                }
                            }
                            if cancel.load(Ordering::Acquire) {
                                let _ = child.kill();
                                return;
                            }
                        }
                        // Flush remaining batch.
                        if !batch.is_empty()
                            && let Ok(mut g) = items.lock()
                        {
                            g.extend(batch.drain(..));
                        }
                        let _ = child.wait();
                    }

                    GrepBackend::Grep => {
                        let child = std::process::Command::new("grep")
                            .args([
                                "-rn",
                                "-E",
                                "--color=never",
                                &q,
                                root.to_str().unwrap_or("."),
                            ])
                            .stdout(Stdio::piped())
                            .stderr(Stdio::null())
                            .spawn();

                        let mut child = match child {
                            Ok(c) => c,
                            Err(_) => return,
                        };

                        let stdout = match child.stdout.take() {
                            Some(s) => s,
                            None => return,
                        };

                        let reader = BufReader::new(stdout);
                        let mut batch: Vec<RgMatch> = Vec::with_capacity(32);
                        let mut total = 0usize;
                        const GREP_CAP: usize = 1000;

                        for line_result in reader.lines() {
                            if cancel.load(Ordering::Acquire) {
                                let _ = child.kill();
                                return;
                            }
                            let raw = match line_result {
                                Ok(l) => l,
                                Err(_) => continue,
                            };
                            if raw.is_empty() {
                                continue;
                            }
                            // Format: path:line_number:text
                            // Split on ':' from the left, first two segments
                            // are path and line number; rest is text (may
                            // contain ':'). Skip lines that don't conform
                            // (binary file warnings, etc.).
                            if let Some(m) = parse_grep_line(&raw, &root) {
                                batch.push(m);
                                total += 1;
                                if batch.len() >= 32
                                    && let Ok(mut g) = items.lock()
                                {
                                    g.extend(batch.drain(..));
                                }
                                if total >= GREP_CAP {
                                    let _ = child.kill();
                                    break;
                                }
                            }
                            if cancel.load(Ordering::Acquire) {
                                let _ = child.kill();
                                return;
                            }
                        }
                        // Flush remaining batch.
                        if !batch.is_empty()
                            && let Ok(mut g) = items.lock()
                        {
                            g.extend(batch.drain(..));
                        }
                        let _ = child.wait();
                    }

                    GrepBackend::Findstr => {
                        // Windows-native findstr: findstr /S /N /R <pattern> <root>\*
                        // Output format: path:line:text — same as grep -n, reuse parse_grep_line.
                        let search_glob = root.join("*");
                        let child = std::process::Command::new("findstr")
                            .args([
                                "/S",
                                "/N",
                                "/R",
                                &q,
                                search_glob.to_str().unwrap_or("*"),
                            ])
                            .stdout(Stdio::piped())
                            .stderr(Stdio::null())
                            .spawn();

                        let mut child = match child {
                            Ok(c) => c,
                            Err(_) => return,
                        };

                        let stdout = match child.stdout.take() {
                            Some(s) => s,
                            None => return,
                        };

                        let reader = BufReader::new(stdout);
                        let mut batch: Vec<RgMatch> = Vec::with_capacity(32);
                        let mut total = 0usize;
                        const FINDSTR_CAP: usize = 1000;

                        for line_result in reader.lines() {
                            if cancel.load(Ordering::Acquire) {
                                let _ = child.kill();
                                return;
                            }
                            let raw = match line_result {
                                Ok(l) => l,
                                Err(_) => continue,
                            };
                            if raw.is_empty() {
                                continue;
                            }
                            if let Some(m) = parse_grep_line(&raw, &root) {
                                batch.push(m);
                                total += 1;
                                if batch.len() >= 32
                                    && let Ok(mut g) = items.lock()
                                {
                                    g.extend(batch.drain(..));
                                }
                                if total >= FINDSTR_CAP {
                                    let _ = child.kill();
                                    break;
                                }
                            }
                            if cancel.load(Ordering::Acquire) {
                                let _ = child.kill();
                                return;
                            }
                        }
                        // Flush remaining batch.
                        if !batch.is_empty()
                            && let Ok(mut g) = items.lock()
                        {
                            g.extend(batch.drain(..));
                        }
                        let _ = child.wait();
                    }

                    GrepBackend::Neither => {
                        // No search tool found — push sentinel item.
                        if let Ok(mut g) = items.lock() {
                            g.push(RgMatch {
                                path: PathBuf::new(),
                                line: 0,
                                _col: 0,
                                text: "no grep tool found — install ripgrep, grep, or findstr to use :rg"
                                    .into(),
                            });
                        }
                    }
                }
            })
            .ok()
    }
}

/// Parse one JSON line from `rg --json` output. Returns `Some(RgMatch)` for
/// lines of `"type":"match"`, `None` for everything else.
fn parse_rg_json_line(line: &str, root: &Path) -> Option<RgMatch> {
    if !line.contains("\"type\":\"match\"") {
        return None;
    }

    let path_text = extract_json_string(line, "\"path\":{\"text\":")?;
    let line_number: u32 = extract_json_u32(line, "\"line_number\":")?;
    let col: u32 = extract_json_u32(line, "\"start\":").unwrap_or(0) + 1;
    let match_text = extract_json_string(line, "\"lines\":{\"text\":").unwrap_or_default();
    let match_text = match_text.trim_end_matches('\n').to_owned();

    let abs_path = PathBuf::from(&path_text);
    let rel_path = abs_path
        .strip_prefix(root)
        .map(|p| p.to_path_buf())
        .unwrap_or(abs_path);

    Some(RgMatch {
        path: rel_path,
        line: line_number,
        _col: col,
        text: match_text,
    })
}

/// Extract a JSON string value that immediately follows the given key pattern.
fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let start = json.find(key)? + key.len();
    let rest = &json[start..];
    let rest = rest.trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    let inner = &rest[1..];
    let mut result = String::new();
    let mut chars = inner.chars();
    loop {
        match chars.next()? {
            '"' => break,
            '\\' => match chars.next()? {
                '"' => result.push('"'),
                '\\' => result.push('\\'),
                'n' => result.push('\n'),
                't' => result.push('\t'),
                c => {
                    result.push('\\');
                    result.push(c);
                }
            },
            c => result.push(c),
        }
    }
    Some(result)
}

/// Extract a u32 JSON number value that immediately follows the given key pattern.
fn extract_json_u32(json: &str, key: &str) -> Option<u32> {
    let start = json.find(key)? + key.len();
    let rest = json[start..].trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

// ── Grep backend detection ────────────────────────────────────────────────────

/// Which search backend is available on this system.
enum GrepBackend {
    /// ripgrep (`rg`) — preferred; produces rich JSON output.
    Rg,
    /// POSIX `grep` — fallback when ripgrep is not installed.
    Grep,
    /// Windows-native `findstr` — fallback on vanilla Windows.
    Findstr,
    /// No supported search tool found on PATH.
    Neither,
}

/// Probe PATH once per requery to decide which backend to use.
/// The probes are cheap (`--version` exits immediately).
fn detect_grep_backend() -> GrepBackend {
    if std::process::Command::new("rg")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return GrepBackend::Rg;
    }
    if std::process::Command::new("grep")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return GrepBackend::Grep;
    }
    if std::process::Command::new("findstr")
        .arg("/?")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return GrepBackend::Findstr;
    }
    GrepBackend::Neither
}

/// Parse one line of `grep -rn` output (`path:line:text`).
///
/// Splits on `:` from the left: first segment is path, second is the 1-based
/// line number, everything after is the matched text (which may itself contain
/// `:`). Returns `None` for lines that don't conform (binary-file warnings,
/// etc.).
fn parse_grep_line(raw: &str, root: &Path) -> Option<RgMatch> {
    let mut parts = raw.splitn(3, ':');
    let path_str = parts.next()?;
    let line_str = parts.next()?;
    let text = parts.next().unwrap_or("").trim_end_matches('\n').to_owned();

    let line: u32 = line_str.parse().ok()?;

    let abs_path = PathBuf::from(path_str);
    let rel_path = abs_path
        .strip_prefix(root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| abs_path);

    Some(RgMatch {
        path: rel_path,
        line,
        _col: 1,
        text,
    })
}

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Build `PreviewSpans` from a flat list of highlight spans.
fn build_preview_spans(flat: &[HighlightSpan], bytes: &[u8], theme: &dyn Theme) -> PreviewSpans {
    let mut row_starts: Vec<usize> = vec![0];
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            row_starts.push(i + 1);
        }
    }
    let row_count = row_starts.len();

    let mut styles: Vec<RatStyle> = Vec::new();
    let mut by_row: Vec<Vec<BufferSpan>> = vec![Vec::new(); row_count];
    for span in flat {
        let Some(rat) = theme.style(span.capture()).map(|s| s.to_ratatui()) else {
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

/// Load a single file for the preview pane. Returns `(content, status)`.
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

/// Background walker — streams `is_file()` entries into `items`,
/// gitignore-aware via `ignore::WalkBuilder`.
fn scan_walk(
    root: &Path,
    items: &Arc<Mutex<Vec<PathBuf>>>,
    done: &Arc<AtomicBool>,
    cancel: &Arc<AtomicBool>,
) {
    let walk = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .parents(true)
        .build();
    let mut batch: Vec<PathBuf> = Vec::with_capacity(256);
    let mut total = 0usize;
    const HARD_CAP: usize = 50_000;
    for entry in walk {
        if cancel.load(Ordering::Acquire) {
            break;
        }
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
        if batch.len() >= 256
            && let Ok(mut g) = items.lock()
        {
            g.extend(batch.drain(..));
        }
        if total >= HARD_CAP {
            break;
        }
    }
    if let Ok(mut g) = items.lock() {
        g.extend(batch.drain(..));
    }
    done.store(true, Ordering::Release);
}

/// Subsequence-based fuzzy score. Returns `None` when not all needle
/// characters appear (in order) in the haystack.
///
/// On success returns `Some((score, positions))` where `positions` is
/// a list of **char indices** (not byte indices) in `haystack` where
/// each character of `needle` matched, in order. Char indices are used
/// so the renderer can walk `haystack.chars().enumerate()` and check
/// membership directly without any byte-to-char conversion.
///
/// Bonuses:
/// - `+8` per match at a word boundary (start, after `/`, `_`, `-`,
///   `.`, ` `).
/// - `+5` per consecutive match (run of adjacent matches).
/// - `+1` base hit per matched char.
///
/// Penalty: `-len(haystack)/8` so shorter overall paths win on ties.
fn score(haystack: &str, needle: &str) -> Option<(i64, Vec<usize>)> {
    if needle.is_empty() {
        return Some((0, Vec::new()));
    }
    let mut needle_chars = needle.chars().peekable();
    let mut total: i64 = 0;
    let mut prev_match = false;
    let mut positions: Vec<usize> = Vec::new();
    let mut prev_ch: Option<char> = None;
    for (ci, ch) in haystack.chars().enumerate() {
        if let Some(&nc) = needle_chars.peek() {
            if ch == nc {
                if prev_match {
                    total += 5;
                }
                let at_boundary = prev_ch
                    .map(|p| matches!(p, '/' | '_' | '-' | '.' | ' '))
                    .unwrap_or(true);
                if at_boundary {
                    total += 8;
                }
                total += 1;
                prev_match = true;
                positions.push(ci);
                needle_chars.next();
            } else {
                prev_match = false;
            }
        }
        prev_ch = Some(ch);
    }
    if needle_chars.peek().is_some() {
        return None;
    }
    total -= haystack.chars().count() as i64 / 8;
    Some((total, positions))
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
        let (a, _) = score("src/main.rs", "main").unwrap();
        let (b, _) = score("src/domain.rs", "main").unwrap();
        assert!(a > b);
    }

    #[test]
    fn score_shorter_wins_on_ties() {
        let (a, _) = score("a/b/foo.rs", "foo").unwrap();
        let (b, _) = score("a/b/c/d/e/foo.rs", "foo").unwrap();
        assert!(a > b);
    }

    #[test]
    fn score_returns_match_positions() {
        // 'f' is at char index 0, 'b' is at char index 4 in "foo_bar".
        let (_, positions) = score("foo_bar", "fb").unwrap();
        assert_eq!(positions, vec![0, 4]);
    }

    #[test]
    fn score_match_positions_skip_unmatched() {
        // 'h' at index 0, 'w' at index 6 in "hello world".
        let (_, positions) = score("hello world", "hw").unwrap();
        assert_eq!(positions, vec![0, 6]);
    }

    #[test]
    fn txt_preview_has_no_highlight_spans() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("notes.txt");
        std::fs::write(&path, "hello world\nthis is plain text\n").unwrap();

        let mut source = FileSource::new(tmp.path().to_path_buf());
        let cancel = Arc::new(AtomicBool::new(false));
        let _handle = source.enumerate(None, Arc::clone(&cancel));
        // Wait for the scan thread to populate items.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if source.item_count() > 0 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let count = source.item_count();
        let mut found_idx = None;
        for i in 0..count {
            if source.label(i).contains("notes.txt") {
                found_idx = Some(i);
                break;
            }
        }
        let idx = found_idx.expect("notes.txt should appear in FileSource");
        let (_buf, status, spans) = source.preview(idx);
        assert!(status.is_empty(), "unexpected status: {status:?}");
        assert!(spans.styles.is_empty(), "got {} styles", spans.styles.len());
        for row in &spans.by_row {
            assert!(row.is_empty(), "unexpected spans on row: {row:?}");
        }
    }
}
