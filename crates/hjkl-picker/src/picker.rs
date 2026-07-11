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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hjkl_buffer::Buffer;
use hjkl_engine::{Input, Key};
use hjkl_form::TextFieldEditor;

use crate::logic::{FilteredEntry, PickerAction, PickerEvent, PickerLogic, RequeryMode};
use hjkl_fuzzy::score;

/// Debounce delay for `RequeryMode::Spawn` sources (milliseconds).
const REQUERY_DEBOUNCE_MS: u64 = 150;

/// Case-fold `s`, returning the folded string plus a map from each folded
/// char index back to the source char index.
///
/// `str::to_lowercase` can change the char count for some code points
/// (e.g. 'İ' → "i̇" is 1→2 chars, 'ẞ' → "ss" is 1→2). Tracking the source
/// index per folded char lets callers translate fuzzy-match positions back to
/// the original text so highlights stay aligned.
fn lower_with_map(s: &str) -> (String, Vec<usize>) {
    let mut lowered = String::new();
    let mut map = Vec::new();
    for (i, ch) in s.chars().enumerate() {
        for lc in ch.to_lowercase() {
            lowered.push(lc);
            map.push(i);
        }
    }
    (lowered, map)
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
    /// Cached file-system path the preview was loaded from, when the
    /// source supplies one. Hosts read this to drive language-aware
    /// preview rendering (syntax highlighting) without the picker
    /// itself depending on a tree-sitter / bonsai layer.
    preview_path: Option<std::path::PathBuf>,
    /// Initial top row for the preview viewport (windowed sources, e.g. grep).
    preview_top_row: usize,
    /// Row to mark with `cursor_line_bg` in the preview (grep match line).
    preview_match_row: Option<usize>,
    /// Offset added to gutter line numbers in the preview (for windowed
    /// snapshots like buffer picker).
    preview_line_offset: usize,
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
            preview_path: None,
            preview_top_row: 0,
            preview_match_row: None,
            preview_line_offset: 0,
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
        // `refresh()` already set `last_query = q` when it scheduled this
        // requery, so don't clear it here — clearing would make the next
        // refresh see q as "changed" again and re-schedule another spawn,
        // looping forever every 150ms. Reset `selected` so the cursor
        // doesn't dangle past the new (eventually-shorter) result list,
        // and `preview_idx` so the preview rebuilds against fresh items.
        self.selected = 0;
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

        let spawn_mode = self.source.requery_mode() == RequeryMode::Spawn;
        // For Spawn sources, a query change schedules a requery rather than
        // filtering in-memory.
        if spawn_mode && q_changed {
            self.requery_at = Some(Instant::now() + Duration::from_millis(REQUERY_DEBOUNCE_MS));
        }

        self.last_query.clone_from(&q);
        self.last_seen_count = count;

        if spawn_mode {
            // Source already filtered server-side (rg/grep/findstr). Show
            // every item in the order the source produced them — running
            // the in-memory fuzzy filter here would drop stale results
            // between query change and the first new batch arriving.
            self.filtered = (0..count)
                .map(|idx| FilteredEntry {
                    idx,
                    matches: Vec::new(),
                })
                .collect();
            if self.selected >= self.filtered.len() {
                self.selected = self.filtered.len().saturating_sub(1);
            }
            return true;
        }

        // Empty-query fast path: if the source pre-sorts, preserve its order
        // verbatim; otherwise fall through to the standard scored sort (which
        // collapses to alphabetical-by-match_text on tied 0 scores).
        if q.is_empty() && self.source.preserve_source_order() {
            self.filtered = (0..count)
                .map(|idx| FilteredEntry {
                    idx,
                    matches: Vec::new(),
                })
                .collect();
            if self.selected >= self.filtered.len() {
                self.selected = self.filtered.len().saturating_sub(1);
            }
            return true;
        }

        let q_lower = q.to_lowercase();
        let mut scored: Vec<(i64, usize, String, Vec<usize>)> = Vec::new();
        for i in 0..count {
            let m = self.source.match_text(i);
            // Case-fold char-by-char, tracking each folded char's source index.
            // `to_lowercase()` can change char count (e.g. 'İ' → "i̇", 'ẞ' → "ss"),
            // which would otherwise shift match positions off the original text
            // and highlight the wrong characters.
            let (m_lower, index_map) = lower_with_map(&m);
            let (sc, positions) = if q.is_empty() {
                (0i64, Vec::new())
            } else {
                match score(&m_lower, &q_lower) {
                    Some((sc, folded_pos)) => {
                        // Translate folded-char positions back to original
                        // char indices so highlights land on the right chars.
                        let mut orig: Vec<usize> = folded_pos
                            .into_iter()
                            .filter_map(|p| index_map.get(p).copied())
                            .collect();
                        orig.dedup();
                        (sc, orig)
                    }
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
            self.preview_path = None;
            self.preview_top_row = 0;
            self.preview_match_row = None;
            self.preview_line_offset = 0;
            return;
        };
        let label = self.source.label(idx);
        let (buf, status) = self.source.preview(idx);
        self.preview_buffer = buf;
        self.preview_status = status;
        self.preview_label = Some(label);
        self.preview_path = self.source.preview_path(idx);
        self.preview_top_row = self.source.preview_top_row(idx);
        self.preview_match_row = self.source.preview_match_row(idx);
        self.preview_line_offset = self.source.preview_line_offset(idx);
    }

    /// Initial top row for the preview viewport.
    pub fn preview_top_row(&self) -> usize {
        self.preview_top_row
    }

    /// Row to mark with `cursor_line_bg` in the preview, if any.
    pub fn preview_match_row(&self) -> Option<usize> {
        self.preview_match_row
    }

    /// Offset added to gutter line numbers in the preview pane.
    pub fn preview_line_offset(&self) -> usize {
        self.preview_line_offset
    }

    /// Per-row spans + style table for the preview pane.
    /// Path the preview was loaded from, if the source supplied one.
    /// Hosts read this to drive language-aware preview rendering
    /// (syntax highlighting) without coupling the picker to a parser
    /// crate.
    pub fn preview_path(&self) -> Option<&std::path::Path> {
        self.preview_path.as_deref()
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

    /// Labels and highlight char positions for every filtered item.
    ///
    /// `refresh()` already caps `filtered` at 500 entries, so this stays
    /// bounded. Returning all of them lets the renderer's `List` + `ListState`
    /// scroll naturally — truncating here would prevent the user from
    /// navigating past the initially-visible window.
    ///
    /// For sources that implement `label_match_positions` (e.g. `RgSource`),
    /// the override positions replace the fuzzy-scorer positions so that
    /// highlighted chars stay within the content portion of the label.
    pub fn visible_entries(&self) -> Vec<(String, Vec<usize>)> {
        let query = &self.last_query;
        self.filtered
            .iter()
            .map(|e| {
                let label = self.source.label(e.idx);
                let positions = self
                    .source
                    .label_match_positions(e.idx, query, &label)
                    .unwrap_or_else(|| e.matches.clone());
                (label, positions)
            })
            .collect()
    }

    /// Per-row semantic styling for visible entries. Char-index ranges with
    /// styles, parallel to `visible_entries`. Empty vec for rows the source
    /// declines to style.
    pub fn visible_entry_styles(
        &self,
    ) -> Vec<Vec<(std::ops::Range<usize>, hjkl_engine::types::Style)>> {
        self.filtered
            .iter()
            .map(|e| {
                let label = self.source.label(e.idx);
                self.source.label_styles(e.idx, &label).unwrap_or_default()
            })
            .collect()
    }

    /// Action for the currently highlighted item, if any.
    fn selected_action(&self) -> Option<PickerAction> {
        let idx = self.filtered.get(self.selected)?.idx;
        Some(self.source.select(idx))
    }

    /// File-system path for visible row `row_idx` (0-based index into the
    /// current filtered list), if the source supplies one.
    ///
    /// Used by right-click menus to enable "Open in Split / Tab / Copy Path"
    /// only when the hovered row actually has a path.
    pub fn path_for_visible_row(&self, row_idx: usize) -> Option<std::path::PathBuf> {
        let src_idx = self.filtered.get(row_idx)?.idx;
        self.source.preview_path(src_idx)
    }

    /// Dismiss the picker without selecting anything.
    pub fn cancel(&mut self) -> PickerEvent {
        PickerEvent::Cancel
    }

    /// Accept the currently highlighted item, if any.
    pub fn accept(&mut self) -> PickerEvent {
        match self.selected_action() {
            Some(a) => PickerEvent::Select(a),
            None => PickerEvent::None,
        }
    }

    /// Move selection down by one (wraps).
    pub fn select_next(&mut self) {
        self.move_selection(1);
    }

    /// Move selection up by one (wraps).
    pub fn select_prev(&mut self) {
        self.move_selection(-1);
    }

    /// Forward an agnostic input event to the query text field.
    ///
    /// The adapter should have already consumed navigation keys (Esc, Enter,
    /// arrows, Ctrl-n/p) before calling this; input events whose `key` equals
    /// `Key::Enter` or `Key::Esc` are silently ignored here for safety.
    pub fn handle_query_input(&mut self, input: Input) {
        if input.key == Key::Enter || input.key == Key::Esc {
            return;
        }
        self.query.handle_input(input);
    }

    /// Ask the source to handle an input event for the currently selected item.
    ///
    /// Returns `Some(action)` when the source wants to short-circuit with a
    /// custom action, or `None` when the source declines (the adapter may then
    /// forward the input to the query field via [`Self::handle_query_input`]).
    pub fn handle_source_key(&mut self, input: Input) -> Option<PickerAction> {
        let idx = self.filtered.get(self.selected).map(|e| e.idx)?;
        self.source.handle_key(idx, input)
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

impl Drop for Picker {
    /// Signal the in-flight background scan to stop when the picker closes.
    /// Without this, dropping the picker (Esc / accept) leaves the scan
    /// thread — and any spawned rg/grep child — running to completion.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;
    use crate::logic::{PickerAction, PickerLogic};

    struct OneItem(String);

    impl PickerLogic for OneItem {
        fn title(&self) -> &str {
            "x"
        }
        fn item_count(&self) -> usize {
            1
        }
        fn label(&self, _idx: usize) -> String {
            self.0.clone()
        }
        fn match_text(&self, _idx: usize) -> String {
            self.0.clone()
        }
        fn select(&self, _idx: usize) -> PickerAction {
            PickerAction::None
        }
        fn enumerate(
            &mut self,
            _query: Option<&str>,
            _cancel: Arc<AtomicBool>,
        ) -> Option<JoinHandle<()>> {
            None
        }
    }

    #[test]
    fn lower_with_map_tracks_source_indices_across_expansion() {
        // 'İ' (U+0130) lowercases to two chars ("i̇"); 's' to one.
        let (lowered, map) = lower_with_map("İs");
        assert_eq!(lowered.chars().count(), 3);
        assert_eq!(map, vec![0, 0, 1]);
    }

    #[test]
    fn highlight_positions_map_to_original_after_case_expansion() {
        // "İstanbul" folds to 9 chars but is 8 chars; match positions must map
        // back into the original char range, not the folded one.
        let mut p = Picker::new_with_query(Box::new(OneItem("İstanbul".into())), "stanbul");
        p.refresh();
        let entries = p.visible_entries();
        assert_eq!(entries.len(), 1);
        let (label, positions) = &entries[0];
        assert_eq!(label, "İstanbul");
        let nchars = label.chars().count();
        assert!(
            positions.iter().all(|&pos| pos < nchars),
            "positions {positions:?} out of range for {nchars} chars"
        );
        // The matched "stanbul" region begins at original char index 1.
        assert!(
            positions.contains(&1),
            "positions {positions:?} miss index 1"
        );
    }
}

#[cfg(test)]
mod drop_tests {
    use super::*;
    use crate::logic::{PickerAction, PickerLogic};

    struct CancelProbe {
        seen: Arc<std::sync::Mutex<Option<Arc<AtomicBool>>>>,
    }

    impl PickerLogic for CancelProbe {
        fn title(&self) -> &str {
            "probe"
        }
        fn item_count(&self) -> usize {
            0
        }
        fn label(&self, _idx: usize) -> String {
            String::new()
        }
        fn match_text(&self, _idx: usize) -> String {
            String::new()
        }
        fn select(&self, _idx: usize) -> PickerAction {
            PickerAction::None
        }
        fn enumerate(
            &mut self,
            _query: Option<&str>,
            cancel: Arc<AtomicBool>,
        ) -> Option<JoinHandle<()>> {
            *self.seen.lock().unwrap() = Some(cancel);
            None
        }
    }

    #[test]
    fn drop_sets_cancel_flag_for_background_scan() {
        let seen = Arc::new(std::sync::Mutex::new(None));
        let picker = Picker::new(Box::new(CancelProbe {
            seen: Arc::clone(&seen),
        }));
        let cancel = seen.lock().unwrap().clone().expect("enumerate called");
        assert!(!cancel.load(Ordering::Acquire));
        drop(picker);
        assert!(
            cancel.load(Ordering::Acquire),
            "dropping the picker must signal the scan to stop"
        );
    }
}
