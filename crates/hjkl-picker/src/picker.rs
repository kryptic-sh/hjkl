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

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hjkl_buffer::Buffer;
use hjkl_form::{Input as EngineInput, Key as EngineKey, TextFieldEditor};

use crate::logic::{FilteredEntry, PickerAction, PickerEvent, PickerLogic, RequeryMode};
use crate::preview::PreviewSpans;
use crate::score::score;

/// Debounce delay for `RequeryMode::Spawn` sources (milliseconds).
const REQUERY_DEBOUNCE_MS: u64 = 150;

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
    /// Initial top row for the preview viewport (windowed sources, e.g. grep).
    preview_top_row: usize,
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
            preview_top_row: 0,
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
            self.preview_top_row = 0;
            return;
        };
        let label = self.source.label(idx);
        let (buf, status, spans) = self.source.preview(idx);
        self.preview_buffer = buf;
        self.preview_status = status;
        self.preview_label = Some(label);
        self.preview_spans = spans;
        self.preview_top_row = self.source.preview_top_row(idx);
    }

    /// Initial top row for the preview viewport.
    pub fn preview_top_row(&self) -> usize {
        self.preview_top_row
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
