use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;

use hjkl_buffer::Buffer;

use crate::preview::PreviewSpans;

/// Action emitted when the user picks an item. The App dispatches each
/// variant to the right machinery.
pub enum PickerAction {
    /// Open the path in the editor (routes through `do_edit`).
    OpenPath(PathBuf),
    /// Open the path at a specific 1-based line number.
    OpenPathAtLine(PathBuf, u32),
    /// Switch to an already-open buffer slot by index.
    SwitchSlot(usize),
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
