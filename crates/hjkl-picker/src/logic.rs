use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;

use hjkl_buffer::View;

/// Action emitted when the user picks an item. The App dispatches each
/// variant to the right machinery.
pub enum PickerAction {
    /// User picked an item — payload is app-defined. Downcast to your
    /// app's action type via `Any`.
    Custom(Box<dyn Any + Send>),
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
    ///
    /// Returns `(buffer, status_text)`. The picker is bonsai-agnostic —
    /// it never produces syntax spans itself. Hosts that want syntax
    /// highlighting in the preview pane read [`Self::preview_path`] and
    /// run the buffer's bytes through their own highlighter at render
    /// time.
    fn preview(&self, idx: usize) -> (View, String) {
        let _ = idx;
        (View::new(), String::new())
    }

    /// File-system path the preview's content was loaded from, when one
    /// exists. Used by the host to drive language-aware preview rendering
    /// (e.g. tree-sitter syntax highlighting). Default `None` for sources
    /// whose preview has no on-disk path (an in-memory snapshot, a help
    /// message, etc.).
    fn preview_path(&self, idx: usize) -> Option<PathBuf> {
        let _ = idx;
        None
    }

    /// Initial scroll position (top row) for the preview viewport.
    /// Sources that show a windowed preview around a specific line override
    /// this so the gutter line numbers reflect the actual file line. Default 0.
    fn preview_top_row(&self, idx: usize) -> usize {
        let _ = idx;
        0
    }

    /// 0-based row to visually mark in the preview (e.g. grep match line).
    /// Default `None` → no highlight. Returning `Some(row)` tells the
    /// renderer to paint a `cursor_line_bg` across that row.
    fn preview_match_row(&self, idx: usize) -> Option<usize> {
        let _ = idx;
        None
    }

    /// Added to the gutter line numbers in the preview. Sources that
    /// snapshot a window of a larger document (e.g. buffer picker
    /// snapshotting ±N lines around the cursor) use this so the gutter
    /// shows the original document line numbers rather than restarting
    /// at 1. Default 0.
    fn preview_line_offset(&self, idx: usize) -> usize {
        let _ = idx;
        0
    }

    /// Translate the picked row into an action.
    fn select(&self, idx: usize) -> PickerAction;

    /// Handle a key before the picker's default handling. Return `Some(action)`
    /// to short-circuit and emit that action immediately. Return `None` to let
    /// the picker handle the key normally. Default: `None`.
    fn handle_key(&self, idx: usize, key: hjkl_engine::Input) -> Option<PickerAction> {
        let _ = (idx, key);
        None
    }

    /// How the picker should react when the query changes.
    fn requery_mode(&self) -> RequeryMode {
        RequeryMode::FilterInMemory
    }

    /// Override the highlight positions for the row at `idx`.
    ///
    /// Default (`None`) means the picker uses fuzzy-scorer match positions.
    /// Sources whose query has its own match semantics (regex grep, exact
    /// match) implement this to return positions in the LABEL string
    /// (char indices, same convention as fuzzy positions).
    fn label_match_positions(&self, idx: usize, query: &str, label: &str) -> Option<Vec<usize>> {
        let _ = (idx, query, label);
        None
    }

    /// Whether the picker should keep the source's enumeration order when
    /// the query is empty. Default `false` means empty-query rows are sorted
    /// by match-text ascending. Sources that pre-sort meaningfully (e.g. by
    /// recency, by HEAD-first) override to `true`.
    fn preserve_source_order(&self) -> bool {
        false
    }

    /// Optional per-row semantic styling for the label. Char-index ranges into
    /// the label with a base style. Fuzzy-match positions overlay these.
    /// Default `None` means no extra styling.
    fn label_styles(
        &self,
        idx: usize,
        label: &str,
    ) -> Option<Vec<(std::ops::Range<usize>, hjkl_engine::types::Style)>> {
        let _ = (idx, label);
        None
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
