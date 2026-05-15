//! TUI mouse support for `apps/hjkl` — Phase 1 (issue #114).
//!
//! This module owns:
//!
//! - [`cell_to_doc`] — cell-space → doc-space translator using the window's
//!   stored `last_rect`, viewport, and gutter geometry.
//! - [`hit_test_window`] — map a terminal cell to a `WindowId`.
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

fn full_gutter_width(
    line_count: usize,
    number: bool,
    relativenumber: bool,
    numberwidth: usize,
    signcolumn: hjkl_engine::types::SignColumnMode,
    foldcolumn: u32,
    has_visible_signs: bool,
) -> u16 {
    let num_w = gutter_width(line_count, number, relativenumber, numberwidth);
    let sign_w: u16 = match signcolumn {
        hjkl_engine::types::SignColumnMode::Yes => 1,
        hjkl_engine::types::SignColumnMode::No => 0,
        hjkl_engine::types::SignColumnMode::Auto => {
            if has_visible_signs {
                1
            } else {
                0
            }
        }
    };
    let fold_w = foldcolumn.min(12) as u16;
    num_w + sign_w + fold_w
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
    let (scl, fdc) = (s.signcolumn, s.foldcolumn);
    let line_count = slot.editor.buffer().line_count() as usize;

    // Mirror the sign visibility check from render.rs.
    let vp = slot.editor.host().viewport();
    let vp_top = vp.top_row;
    let vp_bot = vp_top + rect.height as usize;
    let has_visible_signs = slot
        .diag_signs
        .iter()
        .chain(slot.diag_signs_lsp.iter())
        .chain(slot.git_signs.iter())
        .any(|sg| sg.row >= vp_top && sg.row < vp_bot);

    let gw = full_gutter_width(line_count, nu, rnu, nuw, scl, fdc, has_visible_signs);

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
}
