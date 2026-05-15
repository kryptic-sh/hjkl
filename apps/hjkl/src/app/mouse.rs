//! TUI mouse support for `apps/hjkl` — Phase 1 + Phase 2 (issue #114).
//!
//! This module owns:
//!
//! - [`cell_to_doc`] — cell-space → doc-space translator using the window's
//!   stored `last_rect`, viewport, and gutter geometry.
//! - [`hit_test_window`] — map a terminal cell to a `WindowId`.
//! - [`hit_test_zone`] — classify a click into [`Zone`] (Code / Gutter / TabBar / None).
//! - [`MouseClickTracker`] — double/triple-click state machine.
//!
//! The engine receives only doc-space coordinates via the host-agnostic
//! primitives added in `hjkl-engine` 0.8.0 (`mouse_click_doc`,
//! `mouse_extend_drag_doc`, etc.). All cell-geometry knowledge lives here.
//!
//! **Wide-char note**: `hjkl_buffer::visual_col_to_char_col` (and the engine's
//! matching `visual_col_for_char`) treat every non-tab character as 1 visual
//! cell. That matches the buffer renderer. Wide-char (CJK/emoji) support is a
//! separate concern deferred to a later phase.

use hjkl_engine::{Host, Query};
use ratatui::layout::Rect;
use std::time::{Duration, Instant};

use super::{App, window};

// ── Layout hit-testing ────────────────────────────────────────────────────────

/// Walk the layout tree and find the `WindowId` whose `last_rect` contains
/// the given terminal cell `(col, row)`.
///
/// Uses `Window::last_rect` which the renderer writes every frame, so this is
/// always in sync with what the user sees. Returns `None` before the first
/// render (no `last_rect` yet) or when the click is outside all windows.
pub fn hit_test_window(app: &App, col: u16, row: u16) -> Option<window::WindowId> {
    let leaves = app.layout().leaves();
    for win_id in leaves {
        if let Some(Some(win)) = app.windows.get(win_id)
            && let Some(rect) = win.last_rect
            && rect_contains(rect, col, row)
        {
            return Some(win_id);
        }
    }
    None
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

// ── Gutter width (mirrors render.rs — keep in sync) ──────────────────────────

fn gutter_width(line_count: usize, number: bool, relativenumber: bool, numberwidth: usize) -> u16 {
    if !number && !relativenumber {
        return 0;
    }
    let needed = line_count.to_string().len() + 1;
    needed.max(numberwidth) as u16
}

/// Width of the non-text region on the left edge of a window — the cells the
/// renderer reserves before the first text cell.
///
/// Despite hjkl's `full_gutter_width` accounting model in `apps/hjkl/src/render.rs`
/// (which reserves `num + sign + fold` cells), the actual `BufferWidget` renderer
/// in `hjkl-buffer/src/render.rs:222` paints text starting at `area.x + num_gw`
/// — signs OVERLAY the leftmost gutter cell (`paint_signs` paints at `area.x`),
/// they do not push text right. The fold column is reserved in width math but
/// never actually painted.
///
/// `cell_to_doc` must use this — NOT the renderer's accounting `full_gutter_width`
/// — or clicks land off-by-`sign_w + fold_w` when a sign is visible or
/// `foldcolumn > 0`.
///
/// Regression test: `cell_to_doc_with_visible_sign_first_text_cell_still_maps_to_col_zero`.
fn text_start_offset(
    line_count: usize,
    number: bool,
    relativenumber: bool,
    numberwidth: usize,
) -> u16 {
    gutter_width(line_count, number, relativenumber, numberwidth)
}

// ── cell_to_doc ───────────────────────────────────────────────────────────────

/// Translate a terminal cell `(cell_x, cell_y)` inside window `win_id` to a
/// doc-space `(row, col)` using the window's stored `last_rect` and viewport.
///
/// Returns `None` when:
/// - The cell is in the gutter (left of the text area).
/// - The cell is outside `last_rect`.
/// - The click lands past the last doc row (past EOF).
///
/// The `line_fn` callback looks up a line by 0-based doc row. Pass a closure
/// over `app.slots()[slot].editor.buffer().line(row)` (or similar).
pub fn cell_to_doc(
    app: &App,
    win_id: window::WindowId,
    cell_x: u16,
    cell_y: u16,
) -> Option<(usize, usize)> {
    let win = app.windows.get(win_id)?.as_ref()?;
    let rect = win.last_rect?;

    if !rect_contains(rect, cell_x, cell_y) {
        return None;
    }

    let slot_idx = win.slot;
    let slot = app.slots().get(slot_idx)?;
    let s = slot.editor.settings();
    let (nu, rnu, nuw) = (s.number, s.relativenumber, s.numberwidth);
    let line_count = slot.editor.buffer().line_count() as usize;
    let vp = slot.editor.host().viewport();

    let gw = text_start_offset(line_count, nu, rnu, nuw);

    // Relative cell offset from the window's top-left corner.
    let rel_x = cell_x.saturating_sub(rect.x);
    let rel_y = cell_y.saturating_sub(rect.y);

    // Click is inside the gutter → not a text click.
    if rel_x < gw {
        return None;
    }

    // Visual column inside the text area (already accounting for viewport horizontal scroll).
    let text_rel_x = rel_x - gw; // cells from text-area left edge
    let visual_col = vp.top_col.saturating_add(text_rel_x as usize);

    // Doc row.
    let doc_row = vp.top_row.saturating_add(rel_y as usize);
    if doc_row >= line_count {
        return None; // past EOF
    }

    // Char column via tab-expansion inverse.
    let tab_width = vp.effective_tab_width();
    let line_str = slot.editor.buffer().line(doc_row).unwrap_or("");
    let char_col = hjkl_buffer::visual_col_to_char_col(line_str, visual_col, tab_width);

    Some((doc_row, char_col))
}

// ── MouseClickTracker ─────────────────────────────────────────────────────────

/// Tracks double/triple-click state (same position, same window, within 500ms).
///
/// # Click count semantics
///
/// - count == 1 → single click (`mouse_click_doc`)
/// - count == 2 → double-click → select word
/// - count == 3 → triple-click → select line
/// - count >= 4 → reset to 1 (paragraph-select is Phase 8)
#[derive(Debug, Default)]
pub struct MouseClickTracker {
    last: Option<LastClick>,
}

#[derive(Debug)]
struct LastClick {
    win_id: window::WindowId,
    row: usize,
    col: usize,
    at: Instant,
    count: u8,
}

/// Threshold within which two clicks on the same position count as a multi-click.
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(500);

impl MouseClickTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a down-click at `(win_id, row, col)` and return the effective
    /// click count (1, 2, or 3). A count of 4+ wraps back to 1.
    pub fn register(&mut self, win_id: window::WindowId, row: usize, col: usize) -> u8 {
        let now = Instant::now();
        let count = if let Some(ref last) = self.last {
            // Same window + same (row, col) + within 500ms → increment.
            if last.win_id == win_id
                && last.row == row
                && last.col == col
                && now.duration_since(last.at) <= DOUBLE_CLICK_WINDOW
            {
                let next = last.count + 1;
                if next > 3 { 1 } else { next }
            } else {
                1
            }
        } else {
            1
        };
        self.last = Some(LastClick {
            win_id,
            row,
            col,
            at: now,
            count,
        });
        count
    }

    /// Reset the tracker (e.g. when focus changes or an overlay opens).
    /// Currently unused — the 500ms timeout handles natural resets.
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.last = None;
    }
}

// ── Word-bound helpers ────────────────────────────────────────────────────────

/// Expand the char at `col` in `line` to word boundaries (alphanumeric / `_`).
/// Returns `(word_start, word_end_exclusive)` in char indices.
/// If `col` is not on a word char, returns the single-char range `(col, col+1)`
/// clamped to line length.
pub fn word_bounds(line: &str, col: usize) -> (usize, usize) {
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    if len == 0 {
        return (0, 0);
    }
    let col = col.min(len.saturating_sub(1));
    if !is_word_char(chars[col]) {
        return (col, (col + 1).min(len));
    }
    // Expand left.
    let start = (0..=col)
        .rev()
        .find(|&i| !is_word_char(chars[i]))
        .map(|i| i + 1)
        .unwrap_or(0);
    // Expand right.
    let end = (col..len).find(|&i| !is_word_char(chars[i])).unwrap_or(len);
    (start, end)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

// ── Zone hit-testing (Phase 2) ────────────────────────────────────────────────

/// The semantic zone of a terminal cell — used by right-click dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Zone {
    /// Inside the text area of a window.
    Code {
        win_id: window::WindowId,
        doc_row: usize,
        doc_col: usize,
    },
    /// Inside the gutter (line numbers / signs / fold column) of a window.
    Gutter {
        win_id: window::WindowId,
        doc_row: usize,
    },
    /// On the vim-style tab bar at the top of the screen.
    TabBar { tab_idx: usize },
    /// On the buffer line (one entry per open slot) — shown when
    /// `app.slots.len() > 1`. Sits at row 0 by itself, or at row 1
    /// when the tab bar is also visible.
    BufferLine { slot_idx: usize },
    /// Outside every known zone (e.g. the status line).
    None,
}

/// Compute the x-position ranges for each tab label on the tab bar.
///
/// Mirrors the layout logic in `render::tab_bar` so that click coordinates can
/// be mapped to a tab index without exposing render internals.
///
/// Each tab occupies `[start, start + len)` cells. The returned `Vec` has one
/// entry per tab; entries past the visible area are absent (truncation).
pub fn tab_x_ranges(app: &App, bar_width: u16) -> Vec<(u16, u16)> {
    let max_width = bar_width as usize;
    let mut ranges = Vec::new();
    let mut used = 0usize;

    for (i, tab) in app.tabs.iter().enumerate() {
        let slot_idx = app.windows[tab.focused_window]
            .as_ref()
            .map(|w| w.slot)
            .unwrap_or(0);
        let slot = &app.slots()[slot_idx];
        let base_name = slot
            .filename
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("[No Name]");
        let tab_dirty = tab.layout.leaves().iter().any(|&wid| {
            app.windows[wid]
                .as_ref()
                .map(|w| app.slots()[w.slot].dirty)
                .unwrap_or(false)
        });
        let label = if tab_dirty {
            format!("[{}: +{}]", i + 1, base_name)
        } else {
            format!("[{}: {}]", i + 1, base_name)
        };

        let sep_len = if i == 0 { 0 } else { 1 }; // single space between entries
        let entry_width = sep_len + label.len();

        if used + entry_width > max_width {
            break;
        }

        let start = (used + sep_len) as u16;
        let end = (used + entry_width) as u16;
        ranges.push((start, end));
        used += entry_width;
    }

    ranges
}

/// Compute the x-position ranges for each entry on the buffer line — the
/// one-row strip rendered above the editor when `app.slots.len() > 1`.
///
/// Mirrors `render::buffer_line` (separator `│` between entries, label
/// formatted as ` name ` or ` name+ ` when dirty).
pub fn buffer_line_x_ranges(app: &App, bar_width: u16) -> Vec<(u16, u16)> {
    let max_width = bar_width as usize;
    let mut ranges = Vec::new();
    let mut used = 0usize;

    for (i, slot) in app.slots().iter().enumerate() {
        let base_name = slot
            .filename
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("[No Name]");
        let label = if slot.dirty {
            format!(" {}+ ", base_name)
        } else {
            format!(" {} ", base_name)
        };

        let sep_len = if i == 0 { 0 } else { 1 }; // single '│' between entries
        let entry_width = sep_len + label.len();

        if used + entry_width > max_width {
            break;
        }

        let start = (used + sep_len) as u16;
        let end = (used + entry_width) as u16;
        ranges.push((start, end));
        used += entry_width;
    }

    ranges
}

/// Classify a terminal cell `(col, row)` into a [`Zone`].
///
/// Resolution order:
/// 1. If on the tab bar row (only present when `app.tabs.len() > 1`) → map
///    `col` to a tab index.
/// 2. If on the buffer line row (only present when `app.slots.len() > 1`,
///    sits below the tab bar when both are shown) → map `col` to a slot index.
/// 3. Otherwise, try [`hit_test_window`] to find a containing window.
///    - If the click x-offset is inside the gutter → [`Zone::Gutter`].
///    - If the click translates to a doc position → [`Zone::Code`].
/// 4. Fallback → [`Zone::None`].
pub fn hit_test_zone(app: &App, col: u16, row: u16) -> Zone {
    let show_tab_bar = app.tabs.len() > 1;
    let show_buffer_line = app.slots().len() > 1;
    let tab_bar_row: Option<u16> = if show_tab_bar { Some(0) } else { None };
    let buffer_line_row: Option<u16> = if show_buffer_line {
        Some(if show_tab_bar { 1 } else { 0 })
    } else {
        None
    };

    // Terminal width fallback for bar-geometry math (windows publish their
    // last_rect every frame; before the first render we use 80 as a safe
    // default — the same value `render::frame` would compute from the area).
    let bar_width = app
        .windows
        .iter()
        .filter_map(|w| w.as_ref())
        .filter_map(|w| w.last_rect)
        .map(|r| r.width)
        .max()
        .unwrap_or(80);

    // ── 1. Tab bar ────────────────────────────────────────────────────────
    if Some(row) == tab_bar_row {
        let ranges = tab_x_ranges(app, bar_width);
        for (i, (start, end)) in ranges.iter().enumerate() {
            if col >= *start && col < *end {
                return Zone::TabBar { tab_idx: i };
            }
        }
        return Zone::None;
    }

    // ── 2. Buffer line ────────────────────────────────────────────────────
    if Some(row) == buffer_line_row {
        let ranges = buffer_line_x_ranges(app, bar_width);
        for (i, (start, end)) in ranges.iter().enumerate() {
            if col >= *start && col < *end {
                return Zone::BufferLine { slot_idx: i };
            }
        }
        return Zone::None;
    }

    // ── 2. Window hit-test ────────────────────────────────────────────────
    let Some(win_id) = hit_test_window(app, col, row) else {
        return Zone::None;
    };

    let Some(Some(win)) = app.windows.get(win_id) else {
        return Zone::None;
    };
    let Some(rect) = win.last_rect else {
        return Zone::None;
    };

    let slot_idx = win.slot;
    let Some(slot) = app.slots().get(slot_idx) else {
        return Zone::None;
    };

    let s = slot.editor.settings();
    let (nu, rnu, nuw) = (s.number, s.relativenumber, s.numberwidth);
    let line_count = slot.editor.buffer().line_count() as usize;
    let vp = slot.editor.host().viewport();

    let gw = text_start_offset(line_count, nu, rnu, nuw);

    let rel_x = col.saturating_sub(rect.x);
    let rel_y = row.saturating_sub(rect.y);

    if rel_x < gw {
        // Click is in the gutter — compute doc_row without char_col.
        let doc_row = vp.top_row.saturating_add(rel_y as usize);
        if doc_row < line_count {
            return Zone::Gutter { win_id, doc_row };
        }
        return Zone::None;
    }

    // Click is in the text area — delegate to cell_to_doc for the full translation.
    if let Some((doc_row, doc_col)) = cell_to_doc(app, win_id, col, row) {
        return Zone::Code {
            win_id,
            doc_row,
            doc_col,
        };
    }

    // cell_to_doc returned None (past EOF or outside rect).
    Zone::None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── visual_col_to_char_col (through the buffer crate) ────────────────────

    #[test]
    fn visual_col_ascii_exact() {
        assert_eq!(hjkl_buffer::visual_col_to_char_col("hello", 2, 4), 2);
    }

    #[test]
    fn visual_col_tab_in_middle() {
        // "x\tyz" tab_w=4: x=0, \t covers 1-3, y=4, z=5
        let line = "x\tyz";
        // Any click in the tab run (1, 2, 3) lands on char 1 (the tab).
        assert_eq!(hjkl_buffer::visual_col_to_char_col(line, 1, 4), 1);
        assert_eq!(hjkl_buffer::visual_col_to_char_col(line, 2, 4), 1);
        assert_eq!(hjkl_buffer::visual_col_to_char_col(line, 3, 4), 1);
        // Click after the tab lands on 'y' (char 2).
        assert_eq!(hjkl_buffer::visual_col_to_char_col(line, 4, 4), 2);
    }

    #[test]
    fn visual_col_past_eol_clamps_to_char_count() {
        assert_eq!(hjkl_buffer::visual_col_to_char_col("hi", 99, 4), 2);
    }

    // ── MouseClickTracker ─────────────────────────────────────────────────────

    #[test]
    fn click_tracker_same_pos_within_timeout_increments() {
        let mut t = MouseClickTracker::new();
        assert_eq!(t.register(0, 1, 2), 1);
        assert_eq!(t.register(0, 1, 2), 2);
        assert_eq!(t.register(0, 1, 2), 3);
    }

    #[test]
    fn click_tracker_count_three_wraps_at_four() {
        let mut t = MouseClickTracker::new();
        t.register(0, 0, 0); // 1
        t.register(0, 0, 0); // 2
        t.register(0, 0, 0); // 3
        // 4th click wraps to 1.
        assert_eq!(t.register(0, 0, 0), 1);
    }

    #[test]
    fn click_tracker_different_pos_resets() {
        let mut t = MouseClickTracker::new();
        t.register(0, 1, 2);
        assert_eq!(t.register(0, 3, 4), 1);
    }

    #[test]
    fn click_tracker_different_window_resets() {
        let mut t = MouseClickTracker::new();
        t.register(0, 1, 2);
        assert_eq!(t.register(1, 1, 2), 1);
    }

    #[test]
    fn click_tracker_timeout_resets() {
        let mut t = MouseClickTracker::new();
        // Manually plant a stale last-click.
        t.last = Some(LastClick {
            win_id: 0,
            row: 0,
            col: 0,
            at: Instant::now() - Duration::from_secs(1),
            count: 2,
        });
        // Should reset to 1 because > 500ms elapsed.
        assert_eq!(t.register(0, 0, 0), 1);
    }

    // ── word_bounds ───────────────────────────────────────────────────────────

    #[test]
    fn word_bounds_middle_of_word() {
        // "hello world" → click on 'e' (col 1) → word "hello" → (0, 5)
        assert_eq!(word_bounds("hello world", 1), (0, 5));
    }

    #[test]
    fn word_bounds_on_space() {
        // Space is not a word char → single-char range.
        assert_eq!(word_bounds("hello world", 5), (5, 6));
    }

    #[test]
    fn word_bounds_empty_line() {
        assert_eq!(word_bounds("", 0), (0, 0));
    }

    #[test]
    fn word_bounds_past_eol_clamps() {
        // "hi" has 2 chars; col 99 clamps to 1 (last char 'i').
        assert_eq!(word_bounds("hi", 99), (0, 2));
    }

    // ── hit_test_window ───────────────────────────────────────────────────────

    #[test]
    fn rect_contains_basic() {
        let r = Rect::new(5, 10, 20, 5);
        assert!(rect_contains(r, 5, 10)); // top-left
        assert!(rect_contains(r, 24, 14)); // bottom-right
        assert!(!rect_contains(r, 4, 10)); // left of
        assert!(!rect_contains(r, 25, 10)); // right of
        assert!(!rect_contains(r, 5, 9)); // above
        assert!(!rect_contains(r, 5, 15)); // below
    }

    // ── cell_to_doc gutter math ───────────────────────────────────────────────

    /// Build a minimal App with `content` loaded into slot 0 and the window's
    /// `last_rect` + viewport set to `area`. Centralises the setup ceremony so
    /// the cell_to_doc tests stay focused on the gutter math.
    fn make_app_with_content(content: &str, area: Rect) -> App {
        use hjkl_engine::BufferEdit;

        let mut app = App::new(None, false, None, None).expect("App::new");

        // Replace slot 0's buffer with the test content.
        {
            let buf = app.slots_mut()[0].editor.buffer_mut();
            BufferEdit::replace_all(buf, content);
        }

        // Set window 0's last_rect (the renderer writes this every frame;
        // tests must supply it manually).
        if let Some(Some(win)) = app.windows.get_mut(0) {
            win.last_rect = Some(area);
            win.top_row = 0;
            win.top_col = 0;
        }

        // Set viewport dims to match the area minus a small status-line gap.
        {
            let vp = app.slots_mut()[0].editor.host_mut().viewport_mut();
            vp.width = area.width;
            vp.height = area.height;
            vp.text_width = area.width;
            vp.top_row = 0;
            vp.top_col = 0;
            vp.tab_width = 4;
        }

        app
    }

    /// Round-trip: cell_to_doc must be the INVERSE of the renderer's text
    /// placement. With default settings (number=true, numberwidth=4,
    /// signcolumn=auto, foldcolumn=0) AND no signs present, text renders at
    /// `area.x + num_gw` where num_gw = max(line_count.to_string().len()+1,
    /// numberwidth). A click on that cell must map to (row, 0).
    #[test]
    fn cell_to_doc_no_signs_first_text_cell_is_col_zero() {
        // 5 lines (< 100): num_gw = max(2, 4) = 4. Text starts at cell 4.
        let app =
            make_app_with_content("line1\nline2\nline3\nline4\nline5", Rect::new(0, 0, 80, 24));
        let got = cell_to_doc(&app, 0, 4, 0);
        assert_eq!(
            got,
            Some((0, 0)),
            "click on the first text cell (col=4) of a 5-line buffer should map to (row=0, col=0); got {got:?}"
        );
    }

    /// Regression: when a sign is visible in the viewport, the renderer paints
    /// the sign char at `area.x` (overwriting the first gutter digit) — text
    /// still renders at `area.x + num_gw`. Pre-fix, `cell_to_doc` included
    /// `sign_w` in its gutter computation and treated the first text cell as
    /// gutter (returning `None` or mapping clicks +1 char to the right).
    ///
    /// User-visible symptom: "buffer with less than 100 lines, clicks are off
    /// by 1 char to the right" — small buffers more often have a visible LSP
    /// diagnostic sign (auto signcolumn = visible when any sign in viewport).
    #[test]
    fn cell_to_doc_with_visible_sign_first_text_cell_still_maps_to_col_zero() {
        use hjkl_buffer::Sign;
        use ratatui::style::Style;

        let mut app =
            make_app_with_content("line1\nline2\nline3\nline4\nline5", Rect::new(0, 0, 80, 24));

        // Inject a diagnostic sign on row 0 so signcolumn=auto activates.
        app.slots_mut()[0].diag_signs.push(Sign {
            row: 0,
            ch: 'E',
            style: Style::default(),
            priority: 10,
        });

        // num_gw = 4. With a visible sign, the renderer STILL paints text at
        // cell 4 (signs overpaint the gutter, they don't push text right).
        let got = cell_to_doc(&app, 0, 4, 0);
        assert_eq!(
            got,
            Some((0, 0)),
            "click on the first text cell (col=4) with a visible sign should still map to (row=0, col=0); \
             got {got:?} — if Some((0, 1)) or None, the mouse code is including sign_w in the gutter offset \
             but the renderer paints signs as an overlay (paint_signs at area.x in hjkl-buffer/src/render.rs:530)"
        );

        // Click on the second text cell maps to col=1.
        let got2 = cell_to_doc(&app, 0, 5, 0);
        assert_eq!(got2, Some((0, 1)), "click on cell 5 should map to col 1");
    }

    // ── Buffer line zone ──────────────────────────────────────────────────────

    /// Build an app with N tempfile-backed slots so the buffer line renders.
    fn make_app_with_n_slots(n: usize) -> (App, Vec<std::path::PathBuf>) {
        let mut paths = Vec::new();
        for i in 0..n {
            let p = std::env::temp_dir().join(format!("hjkl_mouse_bl_{i}_{}.txt", rand_suffix()));
            std::fs::write(&p, "content\n").unwrap();
            paths.push(p);
        }
        let mut app = App::new(Some(paths[0].clone()), false, None, None).unwrap();
        for p in &paths[1..] {
            app.dispatch_ex(&format!("e {}", p.display()));
        }
        // Window 0's last_rect — needed so hit_test_zone's bar_width fallback
        // doesn't kick in for tests that exercise wide bars.
        if let Some(Some(win)) = app.windows.get_mut(0) {
            win.last_rect = Some(Rect::new(0, 0, 200, 24));
        }
        (app, paths)
    }

    fn rand_suffix() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{nanos:x}")
    }

    fn cleanup_paths(paths: &[std::path::PathBuf]) {
        for p in paths {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Three slots with predictable filenames; buffer_line_x_ranges must produce
    /// one (start, end) entry per slot and the entries must be contiguous, with
    /// a 1-cell `│` gap between them.
    #[test]
    fn buffer_line_x_ranges_three_slots() {
        let (app, paths) = make_app_with_n_slots(3);
        let ranges = buffer_line_x_ranges(&app, 200);
        cleanup_paths(&paths);

        assert_eq!(ranges.len(), 3, "one range per slot: got {ranges:?}");
        // First entry starts at 0 (no leading separator).
        assert_eq!(ranges[0].0, 0, "first entry starts at col 0");
        // Subsequent entries leave a 1-cell gap for the `│` separator.
        for i in 1..ranges.len() {
            assert_eq!(
                ranges[i].0,
                ranges[i - 1].1 + 1,
                "entry {i} must start one cell after the previous entry's end (separator gap)"
            );
        }
    }

    /// With multiple slots and no extra tabs, the buffer line sits at row 0 and
    /// a click on a slot label returns `Zone::BufferLine { slot_idx }`.
    #[test]
    fn hit_test_zone_buffer_line_at_row_zero_when_no_tabs() {
        let (app, paths) = make_app_with_n_slots(3);
        let ranges = buffer_line_x_ranges(&app, 200);
        // Click on the first cell of each slot's range.
        for (i, (start, _)) in ranges.iter().enumerate() {
            let zone = hit_test_zone(&app, *start, 0);
            assert_eq!(
                zone,
                Zone::BufferLine { slot_idx: i },
                "click at col {start}, row 0 should be BufferLine {{ slot_idx: {i} }} (got {zone:?})"
            );
        }
        cleanup_paths(&paths);
    }

    /// With one slot and no extra tabs, row 0 is the editor — no buffer line.
    #[test]
    fn hit_test_zone_no_buffer_line_with_single_slot() {
        let mut app = App::new(None, false, None, None).unwrap();
        if let Some(Some(win)) = app.windows.get_mut(0) {
            win.last_rect = Some(Rect::new(0, 0, 80, 24));
        }
        // Need viewport published so cell_to_doc has dims.
        {
            let vp = app.slots_mut()[0].editor.host_mut().viewport_mut();
            vp.width = 80;
            vp.height = 24;
            vp.text_width = 80;
        }
        let zone = hit_test_zone(&app, 10, 0);
        if let Zone::BufferLine { .. } = zone {
            panic!("expected no buffer line zone for single-slot app");
        }
    }
}
