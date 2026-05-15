//! App-side picker sources.
//!
//! `BufferSource` — lists open buffer slots.
//! `DiagSource` — LSP diagnostics for the active buffer.
//! `StaticListSource` — generic (label, action) list (LSP goto picker, …).
//!
//! Sources here are bonsai-agnostic: previews ship `(Buffer, status)` plus
//! an optional `preview_path`. The host renderer (apps/hjkl/src/render.rs)
//! reads the path and runs syntax highlighting through the editor's own
//! grammar pipeline (`App::preview_spans_for`), so picker preview never
//! triggers a tree-sitter clone+compile on the UI thread.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;

use hjkl_buffer::Buffer;
use hjkl_picker::{FileSource, PickerAction, PickerLogic, RequeryMode, RgSource};

use crate::picker_action::AppAction;

// ── FileSourceWithOpen / RgSourceWithOpen ────────────────────────────────────
//
// hjkl-picker's `FileSource` / `RgSource` are bonsai-agnostic and don't know
// about `AppAction`, so their default `select` returns `PickerAction::None`.
// These thin wrappers delegate every other method to the inner source and
// override `select` to box the path / line into the right `AppAction`.

/// File source that emits `AppAction::OpenPath` on selection.
pub struct FileSourceWithOpen {
    inner: FileSource,
}

impl FileSourceWithOpen {
    pub fn new(root: PathBuf) -> Self {
        Self {
            inner: FileSource::new(root),
        }
    }
}

impl PickerLogic for FileSourceWithOpen {
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

    fn preview(&self, idx: usize) -> (Buffer, String) {
        self.inner.preview(idx)
    }

    fn preview_path(&self, idx: usize) -> Option<PathBuf> {
        self.inner.preview_path(idx)
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

/// Rg source that emits `AppAction::OpenPathAtLine` on selection.
pub struct RgSourceWithOpen {
    inner: RgSource,
    root: PathBuf,
}

impl RgSourceWithOpen {
    pub fn new(root: PathBuf) -> Self {
        Self {
            inner: RgSource::new(root.clone()),
            root,
        }
    }
}

impl PickerLogic for RgSourceWithOpen {
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

    fn preview(&self, idx: usize) -> (Buffer, String) {
        self.inner.preview(idx)
    }

    fn preview_path(&self, idx: usize) -> Option<PathBuf> {
        self.inner.preview_path(idx)
    }

    fn preview_top_row(&self, idx: usize) -> usize {
        self.inner.preview_top_row(idx)
    }

    fn preview_match_row(&self, idx: usize) -> Option<usize> {
        self.inner.preview_match_row(idx)
    }

    fn select(&self, idx: usize) -> PickerAction {
        match self
            .inner
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|m| (m.path.clone(), m.line)))
        {
            Some((path, line)) if !path.as_os_str().is_empty() => PickerAction::Custom(Box::new(
                AppAction::OpenPathAtLine(self.root.join(path), line),
            )),
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
    /// File path, used by the host for language detection in the preview.
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
    pub entries: Vec<BufferEntry>,
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

    fn preview(&self, idx: usize) -> (Buffer, String) {
        match self.entries.get(idx) {
            Some(e) => {
                let mut buf = Buffer::from_str(&e.content);
                buf.set_cursor(hjkl_buffer::Position {
                    row: e.cursor_row,
                    col: 0,
                });
                (buf, String::new())
            }
            None => (Buffer::new(), String::new()),
        }
    }

    fn preview_path(&self, idx: usize) -> Option<PathBuf> {
        self.entries.get(idx).and_then(|e| e.path.clone())
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
                    AppAction::AnvilInstall(s) => Box::new(AppAction::AnvilInstall(s.clone())),
                    AppAction::AnvilNoOp(s) => Box::new(AppAction::AnvilNoOp(s.clone())),
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

// ── AnvilPickerSource ─────────────────────────────────────────────────────────

/// Install-state of a single anvil tool.
#[derive(Debug, Clone)]
pub enum AnvilState {
    /// Tool is installed at the given version string.
    Installed { version: String },
    /// Tool is installed but the registry has a newer version.
    Outdated { available_version: String },
    /// Tool is in the registry but not installed.
    Available,
}

/// One row in the anvil picker.
#[derive(Debug, Clone)]
pub struct AnvilPickerItem {
    pub name: String,
    pub category: hjkl_anvil::ToolCategory,
    pub description: String,
    pub state: AnvilState,
}

impl AnvilPickerItem {
    /// Build the display label for this row.
    pub fn label(&self) -> String {
        let state_col = match &self.state {
            AnvilState::Installed { version } => format!("[installed @ {version}]"),
            AnvilState::Outdated {
                available_version, ..
            } => format!("[outdated \u{2192} {available_version}]"),
            AnvilState::Available => "[available]".to_string(),
        };
        let cat = format!("{:?}", self.category).to_lowercase();
        format!(
            "{:<30} {:<20} {:<12} \u{2014} {}",
            state_col, self.name, cat, self.description
        )
    }
}

/// Picker source listing all tools in the embedded anvil registry.
pub struct AnvilPickerSource {
    pub items: Vec<AnvilPickerItem>,
}

impl AnvilPickerSource {
    /// Build the source from a registry and the on-disk `.rev` state.
    pub fn from_registry(registry: &hjkl_anvil::Registry) -> Self {
        let items = registry
            .names()
            .filter_map(|name| {
                let spec = registry.get(name)?;
                let rev = hjkl_anvil::store::read_rev(name).ok().flatten();
                let state = match rev {
                    None => AnvilState::Available,
                    Some(r) => {
                        if r.version == spec.version {
                            AnvilState::Installed {
                                version: r.version.clone(),
                            }
                        } else {
                            AnvilState::Outdated {
                                available_version: spec.version.clone(),
                            }
                        }
                    }
                };
                Some(AnvilPickerItem {
                    name: name.to_string(),
                    category: spec.category,
                    description: spec.description.clone(),
                    state,
                })
            })
            .collect();
        Self { items }
    }
}

impl PickerLogic for AnvilPickerSource {
    fn title(&self) -> &str {
        "anvil tools"
    }

    fn item_count(&self) -> usize {
        self.items.len()
    }

    fn label(&self, idx: usize) -> String {
        self.items.get(idx).map(|i| i.label()).unwrap_or_default()
    }

    fn match_text(&self, idx: usize) -> String {
        // Match on name + category + description so the user can type any of them.
        self.items
            .get(idx)
            .map(|i| {
                let cat = format!("{:?}", i.category).to_lowercase();
                format!("{} {} {}", i.name, cat, i.description)
            })
            .unwrap_or_default()
    }

    fn has_preview(&self) -> bool {
        false
    }

    fn select(&self, idx: usize) -> PickerAction {
        match self.items.get(idx) {
            Some(item) => {
                let action = match &item.state {
                    AnvilState::Available | AnvilState::Outdated { .. } => {
                        AppAction::AnvilInstall(item.name.clone())
                    }
                    AnvilState::Installed { .. } => AppAction::AnvilNoOp(item.name.clone()),
                };
                PickerAction::Custom(Box::new(action))
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
