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
use hjkl_tree_sitter::{DotFallbackTheme, Highlighter, LanguageRegistry, Theme};

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
    pub fn new(root: PathBuf) -> Self {
        Self {
            inner: FileSource::new(root),
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
        let flat = h.highlight_range(bytes, 0..bytes.len());
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
    pub fn new(root: PathBuf) -> Self {
        Self {
            inner: RgSource::new(root.clone()),
            root,
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
        let flat = h.highlight_range(bytes, 0..bytes.len());
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
