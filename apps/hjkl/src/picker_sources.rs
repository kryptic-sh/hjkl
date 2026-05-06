//! App-side picker sources.
//!
//! `BufferSource` — lists open buffer slots, no preview.
//! `HighlightedFileSource` — wraps `hjkl_picker::FileSource` and layers
//!   tree-sitter syntax highlighting onto the preview via `PreviewSpans::from_byte_ranges`.
//! `HighlightedRgSource` — same pattern over `hjkl_picker::RgSource`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use hjkl_bonsai::{CommentMarkerPass, Highlighter, Theme};

use crate::lang::LanguageDirectory;
use hjkl_buffer::Buffer;
use hjkl_picker::{FileSource, PickerAction, PickerLogic, PreviewSpans, RequeryMode, RgSource};

use crate::picker_action::AppAction;

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
    /// Snapshot of the buffer's text content at picker-open time.
    pub content: String,
    /// File path, used for tree-sitter language detection.
    pub path: Option<PathBuf>,
    /// 0-based cursor row at picker-open time, used to place the preview.
    /// In window-local coordinates (relative to the snapshot start).
    pub cursor_row: usize,
    /// Original-buffer row of the first line in `content`. Added to gutter
    /// labels so the preview shows real document line numbers when the
    /// snapshot is a window of a larger buffer.
    pub window_start: usize,
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
        content_of: impl Fn(&S) -> String,
        path_of: impl Fn(&S) -> Option<PathBuf>,
        cursor_row_of: impl Fn(&S) -> usize,
        window_start_of: impl Fn(&S) -> usize,
    ) -> Self {
        let entries = slots
            .iter()
            .enumerate()
            .map(|(idx, s)| BufferEntry {
                idx,
                name: name_of(s),
                dirty: dirty_of(s),
                content: content_of(s),
                path: path_of(s),
                cursor_row: cursor_row_of(s),
                window_start: window_start_of(s),
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
        true
    }

    fn preview(&self, idx: usize) -> (Buffer, String, PreviewSpans) {
        match self.entries.get(idx) {
            Some(e) => {
                let mut buf = Buffer::from_str(&e.content);
                buf.set_cursor(hjkl_buffer::Position {
                    row: e.cursor_row,
                    col: 0,
                });
                (buf, String::new(), PreviewSpans::default())
            }
            None => (Buffer::new(), String::new(), PreviewSpans::default()),
        }
    }

    fn preview_top_row(&self, idx: usize) -> usize {
        self.entries
            .get(idx)
            .map(|e| e.cursor_row.saturating_sub(2))
            .unwrap_or(0)
    }

    fn preview_match_row(&self, idx: usize) -> Option<usize> {
        self.entries.get(idx).map(|e| e.cursor_row)
    }

    fn preview_line_offset(&self, idx: usize) -> usize {
        self.entries.get(idx).map(|e| e.window_start).unwrap_or(0)
    }

    fn select(&self, idx: usize) -> PickerAction {
        match self.entries.get(idx) {
            Some(e) => PickerAction::Custom(Box::new(AppAction::SwitchSlot(e.idx))),
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

// ── HighlightedBufferSource ───────────────────────────────────────────────────

/// Buffer source with tree-sitter syntax highlighting in the preview.
///
/// Delegates all `PickerLogic` methods to the inner `BufferSource`; overrides
/// `preview()` to run tree-sitter and call `PreviewSpans::from_byte_ranges`.
pub struct HighlightedBufferSource {
    inner: BufferSource,
    directory: Arc<LanguageDirectory>,
    theme: Arc<dyn Theme + Send + Sync>,
    highlighters: Mutex<HashMap<String, Highlighter>>,
}

impl HighlightedBufferSource {
    pub fn new(
        inner: BufferSource,
        theme: Arc<dyn Theme + Send + Sync>,
        directory: Arc<LanguageDirectory>,
    ) -> Self {
        Self {
            inner,
            directory,
            theme,
            highlighters: Mutex::new(HashMap::new()),
        }
    }

    fn highlight(&self, abs: &Path, content: &str) -> PreviewSpans {
        let bytes = content.as_bytes();
        let flat = preview_spans(&self.directory, &self.highlighters, abs, bytes);
        let Some(mut flat) = flat else {
            return PreviewSpans::default();
        };
        CommentMarkerPass::new().apply(&mut flat, bytes);
        let theme = Arc::clone(&self.theme);
        let ranges: Vec<(std::ops::Range<usize>, ratatui::style::Style)> = flat
            .into_iter()
            .filter_map(|span| {
                theme
                    .style(span.capture())
                    .map(|s| (span.byte_range.clone(), s.to_ratatui()))
            })
            .collect();
        PreviewSpans::from_byte_ranges(&ranges, bytes)
    }
}

/// Shared helper for the three `Highlighted*Source` previews. Resolves the
/// grammar via the directory, looks up (or builds) a per-language
/// `Highlighter` from `cache`, and returns the flat span list.
fn preview_spans(
    directory: &LanguageDirectory,
    cache: &Mutex<HashMap<String, Highlighter>>,
    path: &Path,
    bytes: &[u8],
) -> Option<Vec<hjkl_bonsai::HighlightSpan>> {
    // Sync `for_path` is intentional here: picker source workers run on their
    // own background threads where blocking is fine.  Migrating to the async
    // API is out of scope for hjkl#17.
    let grammar = directory.for_path(path)?;
    let name = grammar.name().to_string();
    let mut hl_cache = cache.lock().ok()?;
    let h = match hl_cache.entry(name) {
        std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
        std::collections::hash_map::Entry::Vacant(v) => match Highlighter::new(grammar) {
            Ok(h) => v.insert(h),
            Err(_) => return None,
        },
    };
    h.reset();
    h.parse_initial(bytes);
    Some(h.highlight_range(bytes, 0..bytes.len()))
}

impl PickerLogic for HighlightedBufferSource {
    fn title(&self) -> &str {
        self.inner.title()
    }

    fn item_count(&self) -> usize {
        self.inner.item_count()
    }

    fn label(&self, idx: usize) -> String {
        self.inner.label(idx)
    }

    fn match_text(&self, idx: usize) -> String {
        self.inner.match_text(idx)
    }

    fn has_preview(&self) -> bool {
        self.inner.has_preview()
    }

    fn preview(&self, idx: usize) -> (Buffer, String, PreviewSpans) {
        let Some(entry) = self.inner.entries.get(idx) else {
            return (Buffer::new(), String::new(), PreviewSpans::default());
        };
        let content = entry.content.clone();
        let path = entry.path.clone();
        let cursor_row = entry.cursor_row;

        let spans = match &path {
            Some(p) => self.highlight(p, &content),
            None => PreviewSpans::default(),
        };

        let mut buf = Buffer::from_str(&content);
        buf.set_cursor(hjkl_buffer::Position {
            row: cursor_row,
            col: 0,
        });
        (buf, String::new(), spans)
    }

    fn preview_top_row(&self, idx: usize) -> usize {
        self.inner.preview_top_row(idx)
    }

    fn preview_match_row(&self, idx: usize) -> Option<usize> {
        self.inner.preview_match_row(idx)
    }

    fn preview_line_offset(&self, idx: usize) -> usize {
        self.inner.preview_line_offset(idx)
    }

    fn select(&self, idx: usize) -> PickerAction {
        self.inner.select(idx)
    }

    fn enumerate(
        &mut self,
        query: Option<&str>,
        cancel: Arc<AtomicBool>,
    ) -> Option<JoinHandle<()>> {
        self.inner.enumerate(query, cancel)
    }
}

// ── HighlightedFileSource ─────────────────────────────────────────────────────

/// File-source with tree-sitter syntax highlighting in the preview.
///
/// Delegates all `PickerLogic` methods to the inner `FileSource`; overrides
/// `preview()` to run tree-sitter and call `PreviewSpans::from_byte_ranges`.
pub struct HighlightedFileSource {
    inner: FileSource,
    directory: Arc<LanguageDirectory>,
    theme: Arc<dyn Theme + Send + Sync>,
    highlighters: Mutex<HashMap<String, Highlighter>>,
}

impl HighlightedFileSource {
    pub fn new(
        root: PathBuf,
        theme: Arc<dyn Theme + Send + Sync>,
        directory: Arc<LanguageDirectory>,
    ) -> Self {
        Self {
            inner: FileSource::new(root),
            directory,
            theme,
            highlighters: Mutex::new(HashMap::new()),
        }
    }

    fn highlight(&self, abs: &Path, content: &str) -> PreviewSpans {
        let bytes = content.as_bytes();
        let Some(mut flat) = preview_spans(&self.directory, &self.highlighters, abs, bytes) else {
            return PreviewSpans::default();
        };
        CommentMarkerPass::new().apply(&mut flat, bytes);
        let theme = Arc::clone(&self.theme);
        let ranges: Vec<(std::ops::Range<usize>, ratatui::style::Style)> = flat
            .into_iter()
            .filter_map(|span| {
                theme
                    .style(span.capture())
                    .map(|s| (span.byte_range.clone(), s.to_ratatui()))
            })
            .collect();
        PreviewSpans::from_byte_ranges(&ranges, bytes)
    }
}

impl PickerLogic for HighlightedFileSource {
    fn title(&self) -> &str {
        self.inner.title()
    }

    fn item_count(&self) -> usize {
        self.inner.item_count()
    }

    fn label(&self, idx: usize) -> String {
        self.inner.label(idx)
    }

    fn match_text(&self, idx: usize) -> String {
        self.inner.match_text(idx)
    }

    fn has_preview(&self) -> bool {
        self.inner.has_preview()
    }

    fn preview(&self, idx: usize) -> (Buffer, String, PreviewSpans) {
        let path = match self
            .inner
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).cloned())
        {
            Some(p) => p,
            None => return (Buffer::new(), String::new(), PreviewSpans::default()),
        };
        let abs = self.inner.root.join(&path);
        let (content, status) = hjkl_picker::load_preview(&abs);
        if !status.is_empty() {
            return (Buffer::from_str(&content), status, PreviewSpans::default());
        }
        let spans = self.highlight(&abs, &content);
        (Buffer::from_str(&content), status, spans)
    }

    fn select(&self, idx: usize) -> PickerAction {
        match self
            .inner
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).cloned())
        {
            Some(p) => PickerAction::Custom(Box::new(AppAction::OpenPath(p))),
            None => PickerAction::None,
        }
    }

    fn requery_mode(&self) -> RequeryMode {
        self.inner.requery_mode()
    }

    fn label_match_positions(&self, idx: usize, query: &str, label: &str) -> Option<Vec<usize>> {
        self.inner.label_match_positions(idx, query, label)
    }

    fn enumerate(
        &mut self,
        query: Option<&str>,
        cancel: Arc<AtomicBool>,
    ) -> Option<JoinHandle<()>> {
        self.inner.enumerate(query, cancel)
    }
}

// ── HighlightedRgSource ───────────────────────────────────────────────────────

/// Grep source with tree-sitter syntax highlighting in the preview.
///
/// Delegates all `PickerLogic` methods to the inner `RgSource`; overrides
/// `preview()` to run tree-sitter and call `PreviewSpans::from_byte_ranges`.
pub struct HighlightedRgSource {
    inner: RgSource,
    root: PathBuf,
    directory: Arc<LanguageDirectory>,
    theme: Arc<dyn Theme + Send + Sync>,
    highlighters: Mutex<HashMap<String, Highlighter>>,
}

impl HighlightedRgSource {
    pub fn new(
        root: PathBuf,
        theme: Arc<dyn Theme + Send + Sync>,
        directory: Arc<LanguageDirectory>,
    ) -> Self {
        Self {
            inner: RgSource::new(root.clone()),
            root,
            directory,
            theme,
            highlighters: Mutex::new(HashMap::new()),
        }
    }

    fn highlight(&self, abs: &Path, content: &str) -> PreviewSpans {
        let bytes = content.as_bytes();
        let Some(mut flat) = preview_spans(&self.directory, &self.highlighters, abs, bytes) else {
            return PreviewSpans::default();
        };
        CommentMarkerPass::new().apply(&mut flat, bytes);
        let theme = Arc::clone(&self.theme);
        let ranges: Vec<(std::ops::Range<usize>, ratatui::style::Style)> = flat
            .into_iter()
            .filter_map(|span| {
                theme
                    .style(span.capture())
                    .map(|s| (span.byte_range.clone(), s.to_ratatui()))
            })
            .collect();
        PreviewSpans::from_byte_ranges(&ranges, bytes)
    }
}

impl PickerLogic for HighlightedRgSource {
    fn title(&self) -> &str {
        self.inner.title()
    }

    fn requery_mode(&self) -> RequeryMode {
        self.inner.requery_mode()
    }

    fn item_count(&self) -> usize {
        self.inner.item_count()
    }

    fn label(&self, idx: usize) -> String {
        self.inner.label(idx)
    }

    fn match_text(&self, idx: usize) -> String {
        self.inner.match_text(idx)
    }

    fn has_preview(&self) -> bool {
        self.inner.has_preview()
    }

    fn preview(&self, idx: usize) -> (Buffer, String, PreviewSpans) {
        let (path, line) = match self
            .inner
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|m| (m.path.clone(), m.line)))
        {
            Some(v) => v,
            None => return (Buffer::new(), String::new(), PreviewSpans::default()),
        };
        if path.as_os_str().is_empty() {
            return (Buffer::new(), String::new(), PreviewSpans::default());
        }
        let abs = self.root.join(&path);
        let (content, status) = hjkl_picker::load_preview(&abs);
        if !status.is_empty() {
            return (Buffer::from_str(&content), status, PreviewSpans::default());
        }
        let spans = self.highlight(&abs, &content);
        let mut buf = Buffer::from_str(&content);
        let match_row = (line as usize).saturating_sub(1);
        buf.set_cursor(hjkl_buffer::Position {
            row: match_row,
            col: 0,
        });
        (buf, String::new(), spans)
    }

    fn preview_top_row(&self, idx: usize) -> usize {
        self.inner.preview_top_row(idx)
    }

    fn preview_match_row(&self, idx: usize) -> Option<usize> {
        self.inner.preview_match_row(idx)
    }

    fn preview_line_offset(&self, idx: usize) -> usize {
        self.inner.preview_line_offset(idx)
    }

    fn select(&self, idx: usize) -> PickerAction {
        match self
            .inner
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|m| (m.path.clone(), m.line)))
        {
            Some((path, line)) if !path.as_os_str().is_empty() => {
                PickerAction::Custom(Box::new(AppAction::OpenPathAtLine(path, line)))
            }
            _ => PickerAction::None,
        }
    }

    fn label_match_positions(&self, idx: usize, query: &str, label: &str) -> Option<Vec<usize>> {
        self.inner.label_match_positions(idx, query, label)
    }

    fn enumerate(
        &mut self,
        query: Option<&str>,
        cancel: Arc<AtomicBool>,
    ) -> Option<JoinHandle<()>> {
        self.inner.enumerate(query, cancel)
    }
}

// ── DiagSource ────────────────────────────────────────────────────────────────

/// A single entry in the LSP diagnostic picker.
pub struct DiagEntry {
    /// Formatted label shown in the picker list.
    pub label: String,
    /// 0-based row the diagnostic starts on (used to jump after selection).
    pub start_row: usize,
    /// 0-based col the diagnostic starts on.
    pub start_col: usize,
}

/// Picker source that lists LSP diagnostics for the active buffer.
pub struct DiagSource {
    entries: Vec<DiagEntry>,
}

impl DiagSource {
    pub fn new(entries: Vec<DiagEntry>) -> Self {
        Self { entries }
    }
}

impl PickerLogic for DiagSource {
    fn title(&self) -> &str {
        "diagnostics"
    }

    fn item_count(&self) -> usize {
        self.entries.len()
    }

    fn label(&self, idx: usize) -> String {
        self.entries
            .get(idx)
            .map(|e| e.label.clone())
            .unwrap_or_default()
    }

    fn match_text(&self, idx: usize) -> String {
        self.label(idx)
    }

    fn has_preview(&self) -> bool {
        false
    }

    fn preview(&self, _idx: usize) -> (hjkl_buffer::Buffer, String, PreviewSpans) {
        (
            hjkl_buffer::Buffer::new(),
            String::new(),
            PreviewSpans::default(),
        )
    }

    fn select(&self, idx: usize) -> PickerAction {
        match self.entries.get(idx) {
            Some(e) => {
                PickerAction::Custom(Box::new(AppAction::JumpToRowCol(e.start_row, e.start_col)))
            }
            None => PickerAction::None,
        }
    }

    fn enumerate(
        &mut self,
        _query: Option<&str>,
        _cancel: Arc<AtomicBool>,
    ) -> Option<JoinHandle<()>> {
        None
    }
}

// ── StaticListSource ──────────────────────────────────────────────────────────

/// A generic picker source backed by a static list of (label, action) pairs.
/// Used by the LSP goto/references picker.
pub struct StaticListSource {
    title: String,
    entries: Vec<(String, AppAction)>,
}

impl StaticListSource {
    pub fn new(title: String, entries: Vec<(String, AppAction)>) -> Self {
        Self { title, entries }
    }
}

impl PickerLogic for StaticListSource {
    fn title(&self) -> &str {
        &self.title
    }

    fn item_count(&self) -> usize {
        self.entries.len()
    }

    fn label(&self, idx: usize) -> String {
        self.entries
            .get(idx)
            .map(|(l, _)| l.clone())
            .unwrap_or_default()
    }

    fn match_text(&self, idx: usize) -> String {
        self.label(idx)
    }

    fn has_preview(&self) -> bool {
        false
    }

    fn preview(&self, _idx: usize) -> (hjkl_buffer::Buffer, String, PreviewSpans) {
        (
            hjkl_buffer::Buffer::new(),
            String::new(),
            PreviewSpans::default(),
        )
    }

    fn select(&self, idx: usize) -> PickerAction {
        match self.entries.get(idx) {
            Some((_, action)) => {
                // Clone the action into a boxed AppAction.
                let boxed: Box<dyn std::any::Any + Send> = match action {
                    AppAction::OpenPath(p) => Box::new(AppAction::OpenPath(p.clone())),
                    AppAction::OpenPathAtLine(p, l) => {
                        Box::new(AppAction::OpenPathAtLine(p.clone(), *l))
                    }
                    AppAction::JumpToRowCol(r, c) => Box::new(AppAction::JumpToRowCol(*r, *c)),
                    AppAction::SwitchSlot(i) => Box::new(AppAction::SwitchSlot(*i)),
                    AppAction::ShowCommit(s) => Box::new(AppAction::ShowCommit(s.clone())),
                    AppAction::CheckoutBranch(s) => Box::new(AppAction::CheckoutBranch(s.clone())),
                    AppAction::CheckoutTag(s) => Box::new(AppAction::CheckoutTag(s.clone())),
                    AppAction::FetchRemote(s) => Box::new(AppAction::FetchRemote(s.clone())),
                    AppAction::StashApply(i) => Box::new(AppAction::StashApply(*i)),
                    AppAction::StashPop(i) => Box::new(AppAction::StashPop(*i)),
                    AppAction::StashDrop(i) => Box::new(AppAction::StashDrop(*i)),
                    AppAction::ApplyCodeAction(i) => Box::new(AppAction::ApplyCodeAction(*i)),
                };
                PickerAction::Custom(boxed)
            }
            None => PickerAction::None,
        }
    }

    fn enumerate(
        &mut self,
        _query: Option<&str>,
        _cancel: Arc<AtomicBool>,
    ) -> Option<JoinHandle<()>> {
        None
    }
}
