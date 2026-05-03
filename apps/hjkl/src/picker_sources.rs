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

use hjkl_buffer::Buffer;
use hjkl_picker::{FileSource, PickerAction, PickerLogic, PreviewSpans, RequeryMode, RgSource};
use hjkl_tree_sitter::{CommentMarkerPass, Highlighter, LanguageRegistry, Theme};

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
            Some(e) => PickerAction::SwitchSlot(e.idx),
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
    registry: LanguageRegistry,
    theme: Arc<dyn Theme + Send + Sync>,
    highlighters: Mutex<HashMap<&'static str, Highlighter>>,
}

impl HighlightedBufferSource {
    pub fn new(inner: BufferSource, theme: Arc<dyn Theme + Send + Sync>) -> Self {
        Self {
            inner,
            registry: LanguageRegistry::new(),
            theme,
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
        let mut flat = h.highlight_range(bytes, 0..bytes.len());
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
    registry: LanguageRegistry,
    theme: Arc<dyn Theme + Send + Sync>,
    highlighters: Mutex<HashMap<&'static str, Highlighter>>,
}

impl HighlightedFileSource {
    pub fn new(root: PathBuf, theme: Arc<dyn Theme + Send + Sync>) -> Self {
        Self {
            inner: FileSource::new(root),
            registry: LanguageRegistry::new(),
            theme,
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
        let mut flat = h.highlight_range(bytes, 0..bytes.len());
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
        self.inner.select(idx)
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
    registry: LanguageRegistry,
    theme: Arc<dyn Theme + Send + Sync>,
    highlighters: Mutex<HashMap<&'static str, Highlighter>>,
}

impl HighlightedRgSource {
    pub fn new(root: PathBuf, theme: Arc<dyn Theme + Send + Sync>) -> Self {
        Self {
            inner: RgSource::new(root.clone()),
            root,
            registry: LanguageRegistry::new(),
            theme,
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
        let mut flat = h.highlight_range(bytes, 0..bytes.len());
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
        self.inner.select(idx)
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
