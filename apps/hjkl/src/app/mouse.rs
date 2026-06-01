//! TUI mouse support for `apps/hjkl` — Phase 1 + Phase 2 (issue #114).
//!
//! This module owns:
//!
//! - [`cell_to_doc`] — cell-space → doc-space translator using the window's
//!   stored `last_rect`, viewport, and gutter geometry.
//! - [`hit_test_window`] — map a terminal cell to a `WindowId`.
//! - [`hit_test_zone`] — classify a click into [`Zone`] (Code / Gutter / TabBar / None).
//! - [`hit_test_border`] — detect clicks on split dividers (Phase 9).
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

// ── Phase 9: border hit-testing ───────────────────────────────────────────────

/// Orientation of a split border — which axis the border divides.
///
/// `Vertical` means a VSplit (side-by-side panes; the border is a vertical
/// column of `│` characters). `Horizontal` means a HSplit (stacked panes;
/// the border is a horizontal row of `─` characters).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitOrientation {
    /// VSplit — border is a vertical column dividing columns.
    Vertical,
    /// HSplit — border is a horizontal row dividing rows.
    Horizontal,
}

/// A draggable split border identified by the screen cell that IS the border,
/// plus enough context to drive resize during a drag.
#[derive(Debug, Clone, Copy)]
pub struct BorderHit {
    /// Orientation of the split that owns this border.
    pub orientation: SplitOrientation,
    /// The border cell (col, row) in terminal coordinates.
    pub border_cell: (u16, u16),
    /// The origin (x for VSplit, y for HSplit) of the split's `last_rect`.
    /// Used to convert drag position → split_pos (cells from origin).
    pub split_origin: u16,
    /// Total size (width for VSplit, height for HSplit) of the split's
    /// `last_rect`. Needed in `resize_split_to` for ratio math.
    pub split_total: u16,
}

/// Walk the layout tree and find a border within `tolerance` cells of
/// `(col, row)`. `tolerance = 0` requires an exact hit on the 1-cell divider.
///
/// The divider geometry mirrors `render::render_layout`:
/// - VSplit: separator column = `rect_a.x + a_w - 1` where `a_w = round(area.width * ratio)`.
/// - HSplit: separator row    = `rect_a.y + a_h - 1` where `a_h = round(area.height * ratio)`.
///
/// Both use the split's `last_rect` (written by the renderer each frame).
/// Returns `None` before the first render or when not on any border.
pub fn hit_test_border(app: &App, col: u16, row: u16) -> Option<BorderHit> {
    let layout = app.layout();
    hit_test_border_tree(layout, col, row)
}

fn hit_test_border_tree(layout: &window::LayoutTree, col: u16, row: u16) -> Option<BorderHit> {
    match layout {
        window::LayoutTree::Leaf(_) => None,
        window::LayoutTree::Split {
            dir,
            ratio,
            a,
            b,
            last_rect,
        } => {
            let area = (*last_rect)?;
            // Compute the separator position from ratio (matches render::split_rect).
            // Match on Axis (exhaustive) so future SplitDir variants cause a
            // compile error rather than a silent runtime no-op.
            use hjkl_layout::Axis;
            let hit = match dir.axis() {
                Axis::Col => {
                    // Vertical split: side-by-side columns.
                    let a_w = ((area.w as f32) * ratio).round() as u16;
                    let a_w = a_w.clamp(1, area.w.saturating_sub(1).max(1));
                    // Separator column: rightmost cell of rect_a (before shrinking).
                    let sep_col = area.x + a_w.saturating_sub(1);
                    if col == sep_col && row >= area.y && row < area.y + area.h {
                        Some(BorderHit {
                            orientation: SplitOrientation::Vertical,
                            border_cell: (col, row),
                            split_origin: area.x,
                            split_total: area.w,
                        })
                    } else {
                        None
                    }
                }
                Axis::Row => {
                    // Horizontal split: stacked rows.
                    let a_h = ((area.h as f32) * ratio).round() as u16;
                    let a_h = a_h.clamp(1, area.h.saturating_sub(1).max(1));
                    // Separator row: bottom row of rect_a (before shrinking).
                    let sep_row = area.y + a_h.saturating_sub(1);
                    if row == sep_row && col >= area.x && col < area.x + area.w {
                        Some(BorderHit {
                            orientation: SplitOrientation::Horizontal,
                            border_cell: (col, row),
                            split_origin: area.y,
                            split_total: area.h,
                        })
                    } else {
                        None
                    }
                }
            };
            // Return this split's hit if found; otherwise recurse into children.
            if hit.is_some() {
                hit
            } else {
                hit_test_border_tree(a, col, row).or_else(|| hit_test_border_tree(b, col, row))
            }
        }
        // `LayoutTree` is `#[non_exhaustive]`; unknown variant → no border hit.
        _ => None,
    }
}

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

fn rect_contains(rect: window::LayoutRect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.w && row >= rect.y && row < rect.y + rect.h
}

fn sign_column_width(
    signcolumn: hjkl_engine::types::SignColumnMode,
    has_visible_signs: bool,
) -> u16 {
    match signcolumn {
        hjkl_engine::types::SignColumnMode::Yes => 1,
        hjkl_engine::types::SignColumnMode::No => 0,
        hjkl_engine::types::SignColumnMode::Auto => {
            if has_visible_signs {
                1
            } else {
                0
            }
        }
    }
}

/// Width of the non-text region on the left edge of a window — the cells the
/// renderer reserves before the first text cell.
///
/// Gutter layout is `[sign][number][fold][text]`, matching the renderer
/// (`apps/hjkl/src/render.rs`). Text starts at
/// `area.x + sign_w + num_gw + fold_w`. The fold column (#245) is now
/// painted, so it MUST be included here or clicks in a buffer with folds
/// map one column too far left.
///
/// `lnum_width` must come from `Editor::lnum_width()`; `fold_w` is the fold
/// column width (auto: 1 when the buffer has folds, widened by `foldcolumn`).
/// `cell_to_doc` and `hit_test_zone` must use this so mouse clicks map to the
/// correct document coordinates.
fn text_start_offset(
    lnum_width: u16,
    signcolumn: hjkl_engine::types::SignColumnMode,
    has_visible_signs: bool,
    fold_w: u16,
) -> u16 {
    let sign_w = sign_column_width(signcolumn, has_visible_signs);
    lnum_width + sign_w + fold_w
}

/// Fold-column width for `slot`, matching the renderer's auto rule: 1 when
/// the buffer has any fold (widened by an explicit `foldcolumn`), else the
/// `foldcolumn` setting (0 by default).
fn fold_column_width_for(slot: &crate::app::BufferSlot) -> u16 {
    let fdc = slot.editor.settings().foldcolumn.min(12) as u16;
    if slot.editor.buffer().folds().is_empty() {
        fdc
    } else {
        fdc.max(1)
    }
}

// ── doc_row_at_screen_offset ─────────────────────────────────────────────────

/// Map a screen-row offset (rows below the viewport top, as the renderer
/// draws them) to a document row, skipping rows hidden by closed folds —
/// the inverse of the renderer's fold-collapsing walk and the screen→doc
/// counterpart of `viewport_math::cursor_screen_row_from`. Without this,
/// clicks below a closed fold land on the wrong line.
pub(crate) fn doc_row_at_screen_offset(
    buffer: &hjkl_buffer::Buffer,
    top_row: usize,
    screen_offset: usize,
) -> usize {
    let mut doc = top_row;
    for _ in 0..screen_offset {
        match buffer.next_visible_row(doc) {
            Some(r) => doc = r,
            None => break, // past last visible row — clamp
        }
    }
    doc
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
/// Resolve the document row shown at screen offset `rel_y` (rows from the
/// window top) in the boxed-blame view, via the render plan. Returns `None`
/// when that screen row is a box border (no doc row) or past the plan.
pub(crate) fn box_plan_doc_row(
    slot: &crate::app::BufferSlot,
    top_row: usize,
    height: usize,
    rel_y: usize,
) -> Option<usize> {
    use hjkl_buffer_tui::render::BlameRow;
    let buf = slot.editor.buffer();
    let plan = crate::app::git_hunks::build_blame_box_plan(
        &slot.blame,
        buf.row_count(),
        |r| buf.is_row_hidden(r),
        top_row,
        height,
        0,
    );
    match plan.get(rel_y) {
        Some(BlameRow::Content(d)) => Some(*d),
        _ => None,
    }
}

/// Resolve the document row whose commit a hovered cell belongs to in the
/// BLAME view — including the virtual box border rows. The commit header
/// (`BorderTop`) maps to the run's first content line and the bottom border to
/// its last, so hovering the header shows that commit's message popup. Outside
/// box mode it falls back to the plain content row under the cell. Returns
/// `None` outside any window or past the plan / buffer.
pub(crate) fn blame_hover_doc_row(app: &App, col: u16, row: u16) -> Option<usize> {
    use hjkl_buffer_tui::render::BlameRow;
    let win_id = hit_test_window(app, col, row)?;
    let win = app.windows.get(win_id)?.as_ref()?;
    let rect = win.last_rect?;
    let slot = app.slots().get(win.slot)?;
    let vp = slot.editor.host().viewport();
    let rel_y = row.saturating_sub(rect.y) as usize;
    let buf = slot.editor.buffer();
    let line_count = buf.line_count() as usize;

    // Non-box BLAME (soft-wrap): no virtual rows — use the plain content row.
    if !matches!(vp.wrap, hjkl_buffer::Wrap::None) {
        let d = doc_row_at_screen_offset(buf, vp.top_row, rel_y);
        return (d < line_count).then_some(d);
    }

    let plan = crate::app::git_hunks::build_blame_box_plan(
        &slot.blame,
        buf.row_count(),
        |r| buf.is_row_hidden(r),
        vp.top_row,
        rect.h as usize,
        0,
    );
    match plan.get(rel_y)? {
        BlameRow::Content(d) => Some(*d),
        // Commit header → the run's first content line below it.
        BlameRow::BorderTop(_) => plan.get(rel_y + 1..)?.iter().find_map(|r| match r {
            BlameRow::Content(d) => Some(*d),
            _ => None,
        }),
        // Bottom border → the run's last content line above it.
        BlameRow::BorderBottom => plan[..rel_y].iter().rev().find_map(|r| match r {
            BlameRow::Content(d) => Some(*d),
            _ => None,
        }),
    }
}

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
    let line_count = slot.editor.buffer().line_count() as usize;
    let vp = slot.editor.host().viewport();

    // Compute sign-column visibility for this window's viewport.
    let vp_top = vp.top_row;
    let vp_bot = vp_top + rect.h as usize;
    let has_visible_signs = slot
        .diag_signs
        .iter()
        .chain(slot.diag_signs_lsp.iter())
        .chain(slot.git_signs.iter())
        .any(|sg| sg.row >= vp_top && sg.row < vp_bot);

    // Boxed-blame view reserves a 1-col left frame and inserts virtual border
    // rows; map clicks through the box plan and account for the frame.
    let box_mode = slot.editor.is_blame() && matches!(vp.wrap, hjkl_buffer::Wrap::None);
    let frame = u16::from(box_mode);

    let gw = text_start_offset(
        slot.editor.lnum_width(),
        s.signcolumn,
        has_visible_signs,
        fold_column_width_for(slot),
    ) + frame;

    // Relative cell offset from the window's top-left corner.
    let rel_x = cell_x.saturating_sub(rect.x);
    let rel_y = cell_y.saturating_sub(rect.y);

    // Click is inside the gutter (or box frame) → not a text click.
    if rel_x < gw {
        return None;
    }

    // Visual column inside the text area (already accounting for viewport horizontal scroll).
    let text_rel_x = rel_x - gw; // cells from text-area left edge
    let visual_col = vp.top_col.saturating_add(text_rel_x as usize);

    // Doc row. In box mode resolve via the render plan (border rows have no
    // doc row); otherwise fold-aware walk from the viewport top.
    let doc_row = if box_mode {
        // `None` (border row / past the plan) propagates as "not a text click".
        box_plan_doc_row(slot, vp.top_row, rect.h as usize, rel_y as usize)?
    } else {
        doc_row_at_screen_offset(slot.editor.buffer(), vp.top_row, rel_y as usize)
    };
    if doc_row >= line_count {
        return None; // past EOF
    }

    // Char column via tab-expansion inverse.
    let tab_width = vp.effective_tab_width();
    let rope = slot.editor.buffer().rope();
    let line_str = if doc_row < rope.len_lines() {
        hjkl_buffer::rope_line_str(&rope, doc_row)
    } else {
        String::new()
    };
    let char_col = hjkl_buffer::visual_col_to_char_col(&line_str, visual_col, tab_width);

    Some((doc_row, char_col))
}

// ── doc_to_cell ─────────────────────────────────────────────────────────────────

/// Inverse of [`cell_to_doc`]: translate a doc-space `(doc_row, char_col)` in
/// window `win_id` to the terminal cell `(cell_x, cell_y)` where that character
/// is drawn, using the window's stored `last_rect`, viewport, and gutter
/// geometry. Used to anchor the K-key hover popup at the cursor cell so it
/// reuses the same compact, content-sized popup as mouse hover.
///
/// Returns `None` when the doc row is outside the visible viewport or the cell
/// would fall outside the window's text area.
pub fn doc_to_cell(
    app: &App,
    win_id: window::WindowId,
    doc_row: usize,
    char_col: usize,
) -> Option<(u16, u16)> {
    let win = app.windows.get(win_id)?.as_ref()?;
    let rect = win.last_rect?;

    let slot_idx = win.slot;
    let slot = app.slots().get(slot_idx)?;
    let s = slot.editor.settings();
    let vp = slot.editor.host().viewport();

    // Row must be within the visible viewport.
    let vp_top = vp.top_row;
    let vp_bot = vp_top + rect.h as usize;
    if doc_row < vp_top || doc_row >= vp_bot {
        return None;
    }

    // Gutter width — must match cell_to_doc's sign-column visibility math.
    let has_visible_signs = slot
        .diag_signs
        .iter()
        .chain(slot.diag_signs_lsp.iter())
        .chain(slot.git_signs.iter())
        .any(|sg| sg.row >= vp_top && sg.row < vp_bot);
    let gw = text_start_offset(
        slot.editor.lnum_width(),
        s.signcolumn,
        has_visible_signs,
        fold_column_width_for(slot),
    );

    // Box mode (BLAME, no soft-wrap) inserts virtual border rows and reserves a
    // 1-col left frame, so the screen row is the doc row's index in the render
    // plan (not `doc_row - vp_top`) and the text shifts right by the frame.
    let box_mode = slot.editor.is_blame() && matches!(vp.wrap, hjkl_buffer::Wrap::None);
    let cell_y = if box_mode {
        use hjkl_buffer_tui::render::BlameRow;
        let buf = slot.editor.buffer();
        let plan = crate::app::git_hunks::build_blame_box_plan(
            &slot.blame,
            buf.row_count(),
            |r| buf.is_row_hidden(r),
            vp_top,
            rect.h as usize,
            0,
        );
        let idx = plan
            .iter()
            .position(|r| matches!(r, BlameRow::Content(d) if *d == doc_row))?;
        rect.y + idx as u16
    } else {
        rect.y + (doc_row - vp_top) as u16
    };

    // char col → visual col (tab expansion) → screen cell, accounting for
    // horizontal scroll. The exact inverse of cell_to_doc's column math.
    let tab_width = vp.effective_tab_width();
    let rope = slot.editor.buffer().rope();
    let line_str = if doc_row < rope.len_lines() {
        hjkl_buffer::rope_line_str(&rope, doc_row)
    } else {
        String::new()
    };
    let visual_col = hjkl_buffer::char_col_to_visual_col(&line_str, char_col, tab_width);
    let text_rel_x = visual_col.saturating_sub(vp.top_col) as u16;
    let cell_x = rect.x + gw + u16::from(box_mode) + text_rel_x;

    if cell_x >= rect.x + rect.w {
        return None;
    }
    Some((cell_x, cell_y))
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
    /// On the buffer-line region of the unified top bar (one entry per open
    /// slot, left-aligned) — shown when `app.slots.len() > 1`. Always at
    /// row 0; shares the row with `TabBar` entries (right-aligned).
    BufferLine { slot_idx: usize },
    /// On the status line (bottom row when no prompt/command overlay is active).
    StatusLine,
    /// On a split border (the 1-cell divider between two panes).
    SplitBorder {
        /// Orientation of the split this border belongs to.
        orientation: super::mouse::SplitOrientation,
        /// Border cell in terminal coordinates (col, row).
        border_cell: (u16, u16),
        /// Origin of the split's `last_rect` (x for VSplit, y for HSplit).
        split_origin: u16,
        /// Total size of the split's `last_rect`.
        split_total: u16,
    },
    /// On a visible row inside the picker overlay. `row_idx` is the
    /// 0-based index into the picker's current filtered list.
    PickerRow { row_idx: usize },
    /// Outside every known zone (e.g. the status line).
    None,
}

/// Compute the total cell width consumed by all tab labels in the unified top bar.
///
/// Tabs are right-aligned; this value is subtracted from the bar width to find
/// where the tab region begins (i.e. `start_x = bar_width - tabs_total_width()`).
pub fn tabs_total_width(app: &App) -> usize {
    let mut total = 0usize;
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
            format!(" {}: {}+ ", i + 1, base_name)
        } else {
            format!(" {}: {} ", i + 1, base_name)
        };
        let sep_len = if i == 0 { 0 } else { 1 }; // single space between tabs
        total += sep_len + label.len();
    }
    total
}

/// Compute the x-position ranges for each tab label on the unified top bar.
///
/// Mirrors the layout logic in `render::top_bar` (right-aligned tabs).
/// `start_x` is the column where tabs begin: `bar_width - tabs_total_width()`.
///
/// Each tab occupies `[start, end)` cells in absolute screen columns.
pub fn tab_x_ranges(app: &App, bar_width: u16) -> Vec<(u16, u16)> {
    let total_tabs = tabs_total_width(app);
    let start_x = (bar_width as usize).saturating_sub(total_tabs);
    let mut ranges = Vec::new();
    let mut used = start_x;

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
            format!(" {}: {}+ ", i + 1, base_name)
        } else {
            format!(" {}: {} ", i + 1, base_name)
        };
        let sep_len = if i == 0 { 0 } else { 1 }; // single space between entries
        let entry_width = sep_len + label.len();

        let entry_start = (used + sep_len) as u16;
        let entry_end = (used + entry_width) as u16;
        ranges.push((entry_start, entry_end));
        used += entry_width;
    }

    ranges
}

/// Compute the x-position ranges for each entry on the buffer-line region of
/// the unified top bar. Buffers are left-aligned, starting at col 0.
///
/// `bar_width` is the full row width. `buf_budget` is the number of cells
/// available to buffers (`bar_width - tabs_total_width` when tabs are shown,
/// `bar_width` otherwise).
///
/// Mirrors `render::top_bar` (separator `│` between entries, label formatted
/// as ` name ` or ` name+ ` when dirty).
pub fn buffer_line_x_ranges(app: &App, bar_width: u16) -> Vec<(u16, u16)> {
    let show_tabs = app.tabs.len() > 1;
    let tabs_len = if show_tabs { tabs_total_width(app) } else { 0 };
    let buf_budget = (bar_width as usize).saturating_sub(tabs_len);
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

        if used + entry_width > buf_budget {
            break;
        }

        let start = (used + sep_len) as u16;
        let end = (used + entry_width) as u16;
        ranges.push((start, end));
        used += entry_width;
    }

    ranges
}

/// Compute the picker overlay rect for the current viewport, mirroring the
/// geometry in `render::picker_overlay` (80% width, 70% height, centered in
/// buf_area).
///
/// Returns `None` when no picker is open or the viewport has not been
/// initialised yet.
pub fn picker_overlay_rect(app: &App) -> Option<Rect> {
    app.picker.as_ref()?;
    let vp = app.active().editor.host().viewport();
    let show_top_bar = app.tabs.len() > 1 || app.slots().len() > 1;
    let top_bar_h = if show_top_bar {
        crate::app::TOP_BAR_HEIGHT
    } else {
        0
    };
    let buf_area = Rect {
        x: 0,
        y: top_bar_h,
        width: vp.width,
        height: vp.height,
    };
    // centered_rect(80, 70, buf_area)
    let width = buf_area.width.saturating_mul(80) / 100;
    let height = buf_area.height.saturating_mul(70) / 100;
    let x = buf_area.x + (buf_area.width.saturating_sub(width)) / 2;
    let y = buf_area.y + (buf_area.height.saturating_sub(height)) / 2;
    Some(Rect {
        x,
        y,
        width,
        height,
    })
}

/// Hit-test a terminal cell against the picker's result list, returning the
/// 0-based filtered-row index when the click lands on a list item.
///
/// Mirrors `render::render_picker_input_and_list` geometry:
/// - The overlay area = `picker_overlay_rect`.
/// - When the source has a preview AND `area.width >= 80`, the left half is
///   the list side (split at 50%); otherwise the whole area is the list side.
/// - Inside the list side: the first 3 rows are the input block; the remainder
///   is the list block (with a 1-cell border on each side).
/// - List row `i` is at absolute terminal row `list_area.y + 1 + i`.
pub fn hit_test_picker_row(app: &App, col: u16, row: u16) -> Option<usize> {
    let area = picker_overlay_rect(app)?;

    let picker = app.picker.as_ref()?;
    let has_preview = picker.has_preview();

    // Determine the list side (left pane).
    const PREVIEW_MIN_WIDTH: u16 = 80;
    let left_area = if has_preview && area.width >= PREVIEW_MIN_WIDTH {
        Rect {
            x: area.x,
            y: area.y,
            width: area.width / 2,
            height: area.height,
        }
    } else {
        area
    };

    // Click must land in the left area.
    if !rect_contains(window::rect_to_layout(left_area), col, row) {
        return None;
    }

    // Input block occupies first 3 rows of left_area; list is the rest.
    let input_h: u16 = 3;
    if left_area.height <= input_h {
        return None;
    }
    let list_y = left_area.y + input_h;
    let list_h = left_area.height - input_h;

    // The list block has a 1-cell border; inner rows start at list_y + 1.
    if row <= list_y || row >= list_y + list_h {
        return None;
    }
    let item_idx = (row - list_y - 1) as usize;

    // Validate against the number of visible entries.
    let entry_count = picker.visible_entries().len();
    if item_idx >= entry_count {
        return None;
    }

    Some(item_idx)
}

/// Classify a terminal cell `(col, row)` into a [`Zone`].
///
/// Resolution order:
/// 1. **Picker exclusive**: when the picker is open, check `hit_test_picker_row`.
///    Returns `Zone::PickerRow` or `Zone::None`; no other zones are tested.
/// 2. If the unified top bar is visible (`app.tabs.len() > 1 ||
///    app.slots().len() > 1`) and `row == 0`:
///    - Right side (tab region): if `col` falls in a tab range → `Zone::TabBar`.
///    - Left side (buffer region): if `col` falls in a buffer range →
///      `Zone::BufferLine`.
///    - Otherwise → `Zone::None`.
/// 3. Status line: bottom row when no overlay is active → `Zone::StatusLine`.
/// 4. Split border: `hit_test_border` → `Zone::SplitBorder`.
/// 5. Window hit-test:
///    - Gutter → `Zone::Gutter`.
///    - Text area → `Zone::Code`.
/// 6. Fallback → `Zone::None`.
pub fn hit_test_zone(app: &App, col: u16, row: u16) -> Zone {
    // ── 1. Picker is exclusive ────────────────────────────────────────────
    if app.picker.is_some() {
        return match hit_test_picker_row(app, col, row) {
            Some(row_idx) => Zone::PickerRow { row_idx },
            None => Zone::None,
        };
    }

    let show_tab_bar = app.tabs.len() > 1;
    let show_buffer_line = app.slots().len() > 1;
    let show_top_bar = show_tab_bar || show_buffer_line;

    // Terminal width fallback for bar-geometry math (windows publish their
    // last_rect every frame; before the first render we use 80 as a safe
    // default — the same value `render::frame` would compute from the area).
    let bar_width = app
        .windows
        .iter()
        .filter_map(|w| w.as_ref())
        .filter_map(|w| w.last_rect)
        .map(|r| r.w)
        .max()
        .unwrap_or(80);

    // ── 2. Unified top bar (row 0) ────────────────────────────────────────
    if show_top_bar && row == 0 {
        // Check tab region first (right-aligned); tabs take priority over
        // the padding between left and right sides.
        if show_tab_bar {
            let tab_ranges = tab_x_ranges(app, bar_width);
            for (i, (start, end)) in tab_ranges.iter().enumerate() {
                if col >= *start && col < *end {
                    return Zone::TabBar { tab_idx: i };
                }
            }
        }
        // Check buffer region (left-aligned).
        if show_buffer_line {
            let buf_ranges = buffer_line_x_ranges(app, bar_width);
            for (i, (start, end)) in buf_ranges.iter().enumerate() {
                if col >= *start && col < *end {
                    return Zone::BufferLine { slot_idx: i };
                }
            }
        }
        return Zone::None;
    }

    // ── 3. Status line (bottom row, no overlay) ───────────────────────────
    // The terminal height is the full screen rect height.
    let screen = app.screen_rect();
    let terminal_height = screen.height;
    let is_status_row = row + 1 == terminal_height; // row is 0-based
    if is_status_row && !app.overlay_active() {
        return Zone::StatusLine;
    }

    // ── 4. Split border ───────────────────────────────────────────────────
    if let Some(bh) = hit_test_border(app, col, row) {
        return Zone::SplitBorder {
            orientation: bh.orientation,
            border_cell: bh.border_cell,
            split_origin: bh.split_origin,
            split_total: bh.split_total,
        };
    }

    // ── 5. Window hit-test ────────────────────────────────────────────────
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
    let line_count = slot.editor.buffer().line_count() as usize;
    let vp = slot.editor.host().viewport();

    // Compute sign-column visibility for this window.
    let vp_top = vp.top_row;
    let vp_bot = vp_top + rect.h as usize;
    let has_visible_signs = slot
        .diag_signs
        .iter()
        .chain(slot.diag_signs_lsp.iter())
        .chain(slot.git_signs.iter())
        .any(|sg| sg.row >= vp_top && sg.row < vp_bot);

    let gw = text_start_offset(
        slot.editor.lnum_width(),
        s.signcolumn,
        has_visible_signs,
        fold_column_width_for(slot),
    );

    let rel_x = col.saturating_sub(rect.x);
    let rel_y = row.saturating_sub(rect.y);

    // Box mode reserves a 1-col left frame; widen the gutter hit region by it.
    let box_mode = slot.editor.is_blame() && matches!(vp.wrap, hjkl_buffer::Wrap::None);
    let gw = gw + u16::from(box_mode);

    if rel_x < gw {
        // Click is in the gutter (or box frame) — compute doc_row only.
        let doc_row = if box_mode {
            match box_plan_doc_row(slot, vp.top_row, rect.h as usize, rel_y as usize) {
                Some(d) => d,
                None => return Zone::None, // border row
            }
        } else {
            doc_row_at_screen_offset(slot.editor.buffer(), vp.top_row, rel_y as usize)
        };
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

// ── App methods extracted from mod.rs ────────────────────────────────────────

impl App {
    /// Primary-selection paste at terminal cell `(col, row)`. Pulled out
    /// of [`Self::middle_click`] so the Code-zone path is independently
    /// expressible (and so the X11/Wayland-only branch is grep-able).
    pub(crate) fn middle_click_paste_primary(&mut self, col: u16, row: u16) {
        use hjkl_clipboard::{Capabilities, MimeType, Selection};

        let Some(win_id) = hit_test_window(self, col, row) else {
            return;
        };
        let Some((doc_row, doc_col)) = cell_to_doc(self, win_id, col, row) else {
            return;
        };

        // Read primary selection BEFORE any mut borrows of self.
        let primary_text: Option<String> = {
            let cb = self.active().editor.host().clipboard();
            cb.filter(|cb| {
                cb.capabilities().contains(Capabilities::PRIMARY)
                    && cb.capabilities().contains(Capabilities::READ)
            })
            .and_then(|cb| {
                cb.get(Selection::Primary, MimeType::Text)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
            })
        };

        let current_focus = self.focused_window();
        if win_id != current_focus {
            self.switch_focus(win_id);
        }

        self.active_mut().editor.mouse_click_doc(doc_row, doc_col);
        self.sync_after_engine_mutation();

        if let Some(text) = primary_text {
            self.active_mut().editor.set_yank(text);
            self.active_mut().editor.paste_after(1);
            self.sync_after_engine_mutation();
        }
    }
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
        let r = window::LayoutRect::new(5, 10, 20, 5);
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
        // tests must supply it manually). Convert from ratatui Rect.
        if let Some(Some(win)) = app.windows.get_mut(0) {
            win.last_rect = Some(window::rect_to_layout(area));
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

    /// With the dedicated sign-column layout, when a sign is visible, the
    /// sign occupies a SEPARATE column to the left of the number column.
    /// num_gw=4 (5-line buffer, numberwidth=4), sign_w=1 (auto + visible sign)
    /// → text starts at cell 5 (sign_w + num_gw = 1 + 4).
    ///
    /// Pre-fix (overlay model): signs painted at area.x, text still at
    /// area.x + num_gw (cell 4). A click on cell 4 was already text col 0.
    ///
    /// Post-fix (dedicated column): the sign has its own cell (x=0); text
    /// starts at x=5, so a click on cell 4 is now IN the gutter and
    /// returns None, and a click on cell 5 is the first text cell (col 0).
    #[test]
    fn cell_to_doc_with_visible_sign_first_text_cell_is_at_sign_w_plus_num_gw() {
        use hjkl_buffer_tui::Sign;
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

        // sign_w=1 (auto + visible sign), num_gw=4 → text starts at cell 5.
        // Click on cell 4 is inside the gutter → None.
        let got_gutter = cell_to_doc(&app, 0, 4, 0);
        assert_eq!(
            got_gutter, None,
            "cell 4 is in the gutter (sign col=0, num col=1..4, spacer=4); got {got_gutter:?}"
        );

        // Click on cell 5 is the first text cell → (row=0, col=0).
        let got = cell_to_doc(&app, 0, 5, 0);
        assert_eq!(
            got,
            Some((0, 0)),
            "click on the first text cell (col=5 = sign_w+num_gw) should map to (row=0, col=0); got {got:?}"
        );

        // Click on cell 6 maps to col=1.
        let got2 = cell_to_doc(&app, 0, 6, 0);
        assert_eq!(got2, Some((0, 1)), "click on cell 6 should map to col 1");
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
            win.last_rect = Some(window::LayoutRect::new(0, 0, 200, 24));
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
            win.last_rect = Some(window::LayoutRect::new(0, 0, 80, 24));
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

    // ── Unified top bar tests (T3) ────────────────────────────────────────────

    /// Helper: build an app with multiple slots (via `:e`) AND multiple tabs
    /// (via `:tabnew`).  Note: `:tabnew` without args adds an anonymous slot,
    /// so `app.slots().len()` will be `n_slots + n_extra_tabs`.
    /// Window 0 gets `last_rect` set to a wide area so bar_width is correct.
    fn make_app_with_slots_and_tabs(
        n_slots: usize,
        n_extra_tabs: usize,
    ) -> (App, Vec<std::path::PathBuf>) {
        assert!(n_slots >= 1);
        let mut paths = Vec::new();
        // Create temp files for all slots.
        for i in 0..n_slots {
            let p = std::env::temp_dir().join(format!("hjkl_unified_{i}_{}.txt", rand_suffix()));
            std::fs::write(&p, "content\n").unwrap();
            paths.push(p);
        }
        let mut app = App::new(Some(paths[0].clone()), false, None, None).unwrap();
        // Open remaining slots.
        for p in &paths[1..] {
            app.dispatch_ex(&format!("e {}", p.display()));
        }
        // Open extra tabs (each adds 1 anonymous slot).
        for _ in 0..n_extra_tabs {
            app.dispatch_ex("tabnew");
        }
        // Wide window so bar geometry doesn't truncate anything in tests.
        if let Some(Some(win)) = app.windows.get_mut(0) {
            win.last_rect = Some(window::LayoutRect::new(0, 0, 200, 24));
        }
        (app, paths)
    }

    /// 3 slots + 2 tabs → unified bar at row 0.
    /// Col 0 → BufferLine{0}; col near right edge → TabBar{last}.
    #[test]
    fn hit_test_zone_unified_bar_buffer_then_tab_horizontal() {
        // 3 slots via :e + 1 extra tab via :tabnew = 4 slots, 2 tabs.
        let (app, paths) = make_app_with_slots_and_tabs(3, 1);
        assert!(app.slots().len() > 1, "expected multiple slots");
        assert_eq!(app.tabs.len(), 2, "expected 2 tabs");

        // Col 0 must be in the first buffer entry (left-aligned).
        let zone0 = hit_test_zone(&app, 0, 0);
        assert_eq!(
            zone0,
            Zone::BufferLine { slot_idx: 0 },
            "col 0 row 0 should be BufferLine{{0}} (got {zone0:?})"
        );

        // The last tab label sits flush with col 199 (bar_width - 1).
        // Find it via tab_x_ranges.
        let tab_ranges = tab_x_ranges(&app, 200);
        assert_eq!(tab_ranges.len(), 2, "expected 2 tab ranges");
        let (last_start, last_end) = tab_ranges[1];
        // Click somewhere inside the last tab's range.
        let click_col = last_start + (last_end - last_start) / 2;
        let zone_tab = hit_test_zone(&app, click_col, 0);
        assert_eq!(
            zone_tab,
            Zone::TabBar { tab_idx: 1 },
            "click at col {click_col} row 0 should be TabBar{{1}} (got {zone_tab:?})"
        );

        cleanup_paths(&paths);
    }

    /// 1 initial slot + 1 extra tab (via :tabnew which adds anon slot) = 2 slots, 2 tabs.
    /// But what matters: tabs.len() > 1, and buffer region of first slot (slot 0,
    /// which is NOT the active tab's slot) maps correctly.
    /// Separately: test with a single-slot setup where no buffer line shows.
    ///
    /// Use `make_app_with_slots_and_tabs(1, 1)` → 2 slots (1 original + 1 anon), 2 tabs.
    /// The active tab shows the anon slot; the buffer line will render (2 slots).
    /// For "only tabs, no buffers" we need single-slot + multi-tab without extra slot creation.
    /// We build that inline.
    #[test]
    fn hit_test_zone_unified_bar_only_tabs_no_buffers() {
        // Build an app with only 1 slot but 2 tabs.
        // Open a second tab by using tabnew with a temp file so no extra anon slot is added.
        // Actually open_new_slot always pushes. Use a different approach:
        // open second tab using `tabnew` then bdelete the anon slot.
        // Simplest: just use make_app_with_n_slots(1) + manually inject a second tab
        // pointing to the same slot.
        use crate::app::window::{LayoutTree, Tab};
        let mut app = App::new(None, false, None, None).unwrap();
        // Manually add a second tab pointing to slot 0 (same as first tab).
        let new_win_id = app.next_window_id;
        app.next_window_id += 1;
        app.windows.push(Some(crate::app::window::Window::new(0)));
        app.tabs
            .push(Tab::new(LayoutTree::Leaf(new_win_id), new_win_id));
        // Wide window for bar_width.
        if let Some(Some(win)) = app.windows.get_mut(0) {
            win.last_rect = Some(window::LayoutRect::new(0, 0, 200, 24));
        }
        assert_eq!(app.slots().len(), 1, "expected 1 slot");
        assert_eq!(app.tabs.len(), 2, "expected 2 tabs");

        // Col 0 is padding (buffer region empty) → Zone::None.
        let zone_left = hit_test_zone(&app, 0, 0);
        assert_eq!(
            zone_left,
            Zone::None,
            "col 0 with no buffers should be Zone::None (got {zone_left:?})"
        );

        // Click inside the first tab range → TabBar{0}.
        let tab_ranges = tab_x_ranges(&app, 200);
        assert!(!tab_ranges.is_empty(), "tab_ranges must not be empty");
        let (start0, end0) = tab_ranges[0];
        let click_col = start0 + (end0 - start0) / 2;
        let zone_tab = hit_test_zone(&app, click_col, 0);
        assert_eq!(
            zone_tab,
            Zone::TabBar { tab_idx: 0 },
            "click at col {click_col} row 0 should be TabBar{{0}} (got {zone_tab:?})"
        );

        // No paths to clean up (anonymous slot).
    }

    /// Single tab + 3 slots. Tab region is empty (no tabbar when 1 tab).
    /// Click on buffers → BufferLine.
    #[test]
    fn hit_test_zone_unified_bar_only_buffers_no_tabs() {
        // 3 slots via :e, 0 extra tabs → 3 slots, 1 tab.
        let (app, paths) = make_app_with_slots_and_tabs(3, 0);
        assert_eq!(app.slots().len(), 3, "expected 3 slots");
        assert_eq!(app.tabs.len(), 1, "expected 1 tab");

        let buf_ranges = buffer_line_x_ranges(&app, 200);
        assert_eq!(buf_ranges.len(), 3, "expected 3 buffer ranges");

        for (i, (start, _)) in buf_ranges.iter().enumerate() {
            let zone = hit_test_zone(&app, *start, 0);
            assert_eq!(
                zone,
                Zone::BufferLine { slot_idx: i },
                "col {start} row 0 should be BufferLine{{{i}}} (got {zone:?})"
            );
        }

        cleanup_paths(&paths);
    }

    // ── hit_test_border (Phase 9) ─────────────────────────────────────────────

    /// Helper: build an app with two windows in a VSplit, pre-fill last_rects
    /// so hit_test_border can operate without a live renderer.
    fn make_vsplit_app() -> App {
        use crate::app::window::{LayoutRect, LayoutTree, Tab, Window};

        let mut app = App::new(None, false, None, None).unwrap();

        // Add a second window.
        let win1 = app.next_window_id;
        app.next_window_id += 1;
        app.windows.push(Some(Window::new(0)));

        // Build: VSplit(ratio=0.5, Leaf(0), Leaf(1)), total area 80x24.
        // With ratio=0.5 and width=80: a_w = round(80*0.5)=40
        // sep_col = 0 + 40 - 1 = 39
        let split_area = LayoutRect::new(0, 0, 80, 24);
        app.tabs[0] = Tab::new(
            LayoutTree::Split {
                dir: crate::app::window::SplitDir::Vertical,
                ratio: 0.5,
                a: Box::new(LayoutTree::Leaf(0)),
                b: Box::new(LayoutTree::Leaf(win1)),
                last_rect: Some(split_area),
            },
            0,
        );
        // Fill window last_rects.
        if let Some(Some(w)) = app.windows.get_mut(0) {
            w.last_rect = Some(LayoutRect::new(0, 0, 39, 24)); // left pane (shrunk by 1)
        }
        if let Some(Some(w)) = app.windows.get_mut(win1) {
            w.last_rect = Some(LayoutRect::new(40, 0, 40, 24)); // right pane
        }
        app
    }

    /// Helper: build an app with two windows in an HSplit, pre-fill last_rects.
    fn make_hsplit_app() -> App {
        use crate::app::window::{LayoutRect, LayoutTree, Tab, Window};

        let mut app = App::new(None, false, None, None).unwrap();

        let win1 = app.next_window_id;
        app.next_window_id += 1;
        app.windows.push(Some(Window::new(0)));

        // HSplit(ratio=0.5, Leaf(0), Leaf(1)), area 80x24
        // a_h = round(24*0.5) = 12; sep_row = 0 + 12 - 1 = 11
        let split_area = LayoutRect::new(0, 0, 80, 24);
        app.tabs[0] = Tab::new(
            LayoutTree::Split {
                dir: crate::app::window::SplitDir::Horizontal,
                ratio: 0.5,
                a: Box::new(LayoutTree::Leaf(0)),
                b: Box::new(LayoutTree::Leaf(win1)),
                last_rect: Some(split_area),
            },
            0,
        );
        if let Some(Some(w)) = app.windows.get_mut(0) {
            w.last_rect = Some(LayoutRect::new(0, 0, 80, 11));
        }
        if let Some(Some(w)) = app.windows.get_mut(win1) {
            w.last_rect = Some(LayoutRect::new(0, 12, 80, 12));
        }
        app
    }

    #[test]
    fn hit_test_border_on_vertical_divider() {
        let app = make_vsplit_app();
        // sep_col = 39 (for ratio=0.5, width=80)
        let hit = hit_test_border(&app, 39, 10);
        assert!(
            hit.is_some(),
            "click on vertical divider (col=39) should return BorderHit"
        );
        let h = hit.unwrap();
        assert_eq!(h.orientation, SplitOrientation::Vertical);
        assert_eq!(h.border_cell, (39, 10));
        assert_eq!(h.split_origin, 0);
        assert_eq!(h.split_total, 80);
    }

    #[test]
    fn hit_test_border_off_divider() {
        let app = make_vsplit_app();
        // 2 cells away from divider (col=41) → None
        let hit = hit_test_border(&app, 41, 10);
        assert!(
            hit.is_none(),
            "click 2 cells away from divider should return None"
        );
    }

    #[test]
    fn hit_test_border_on_horizontal_divider() {
        let app = make_hsplit_app();
        // sep_row = 11 (for ratio=0.5, height=24)
        let hit = hit_test_border(&app, 20, 11);
        assert!(
            hit.is_some(),
            "click on horizontal divider (row=11) should return BorderHit"
        );
        let h = hit.unwrap();
        assert_eq!(h.orientation, SplitOrientation::Horizontal);
        assert_eq!(h.border_cell, (20, 11));
        assert_eq!(h.split_origin, 0);
        assert_eq!(h.split_total, 24);
    }

    #[test]
    fn hit_test_border_with_nested_splits() {
        use crate::app::window::{LayoutRect, LayoutTree, SplitDir, Tab, Window};

        // Layout: HSplit(
        //   a = VSplit(Leaf(0), Leaf(1))   — top row, two columns
        //   b = Leaf(2)                    — bottom row
        // )
        // Full area: 80x24
        // HSplit: a_h = round(24*0.5) = 12; sep_row = 11
        // VSplit (inner, area 80x12): a_w = round(80*0.5) = 40; sep_col = 39

        let mut app = App::new(None, false, None, None).unwrap();

        let win1 = app.next_window_id;
        app.next_window_id += 1;
        {
            let mut w = Window::new(0);
            w.last_rect = Some(LayoutRect::new(40, 0, 40, 11));
            app.windows.push(Some(w));
        }
        let win2 = app.next_window_id;
        app.next_window_id += 1;
        {
            let mut w = Window::new(0);
            w.last_rect = Some(LayoutRect::new(0, 12, 80, 12));
            app.windows.push(Some(w));
        }

        if let Some(Some(w)) = app.windows.get_mut(0) {
            w.last_rect = Some(LayoutRect::new(0, 0, 39, 11));
        }

        app.tabs[0] = Tab::new(
            LayoutTree::Split {
                dir: SplitDir::Horizontal,
                ratio: 0.5,
                a: Box::new(LayoutTree::Split {
                    dir: SplitDir::Vertical,
                    ratio: 0.5,
                    a: Box::new(LayoutTree::Leaf(0)),
                    b: Box::new(LayoutTree::Leaf(win1)),
                    last_rect: Some(LayoutRect::new(0, 0, 80, 12)),
                }),
                b: Box::new(LayoutTree::Leaf(win2)),
                last_rect: Some(LayoutRect::new(0, 0, 80, 24)),
            },
            0,
        );

        // Click on the vertical divider inside the top VSplit (col=39, row=5).
        let hit_v = hit_test_border(&app, 39, 5);
        assert!(
            hit_v.is_some(),
            "nested VSplit border at col=39 row=5 should be hittable"
        );
        assert_eq!(hit_v.unwrap().orientation, SplitOrientation::Vertical);

        // Click on the horizontal divider (row=11, col=20).
        let hit_h = hit_test_border(&app, 20, 11);
        assert!(
            hit_h.is_some(),
            "outer HSplit border at row=11 col=20 should be hittable"
        );
        assert_eq!(hit_h.unwrap().orientation, SplitOrientation::Horizontal);
    }

    /// Single tab + single slot → no top bar. Row 0 is the editor, not the bar.
    #[test]
    fn hit_test_zone_no_bar_at_all_when_single_tab_single_slot() {
        let mut app = App::new(None, false, None, None).unwrap();
        // Set up window rect so hit_test_window can find it.
        if let Some(Some(win)) = app.windows.get_mut(0) {
            win.last_rect = Some(window::LayoutRect::new(0, 0, 80, 24));
        }
        {
            let vp = app.slots_mut()[0].editor.host_mut().viewport_mut();
            vp.width = 80;
            vp.height = 24;
            vp.text_width = 80;
        }
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.slots().len(), 1);

        // Row 0 must NOT be Zone::TabBar or Zone::BufferLine.
        let zone = hit_test_zone(&app, 10, 0);
        assert!(
            !matches!(zone, Zone::TabBar { .. } | Zone::BufferLine { .. }),
            "single tab + single slot: row 0 should be editor zone, got {zone:?}"
        );
    }

    // ── Phase 7+8 zone tests ──────────────────────────────────────────────────

    /// Helper: build a minimal App with viewport set to 80x24 (no top bar).
    fn make_basic_app_80x24() -> App {
        let mut app = App::new(None, false, None, None).unwrap();
        if let Some(Some(win)) = app.windows.get_mut(0) {
            win.last_rect = Some(window::LayoutRect::new(0, 0, 80, 24));
        }
        {
            let vp = app.slots_mut()[0].editor.host_mut().viewport_mut();
            vp.width = 80;
            vp.height = 24;
            vp.text_width = 80;
            vp.top_row = 0;
            vp.top_col = 0;
        }
        app
    }

    /// Click on the last terminal row with no overlay active must return
    /// `Zone::StatusLine`.
    ///
    /// With vp.height=24 and STATUS_LINE_HEIGHT=1 (no top bar):
    /// screen height = 25; status row = 24.
    #[test]
    fn hit_test_zone_status_line_at_bottom() {
        let app = make_basic_app_80x24();

        // Confirm single tab + single slot → no top bar.
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.slots().len(), 1);

        let screen = app.screen_rect();
        // The status row is the last row (0-based: screen.height - 1).
        let status_row = screen.height.saturating_sub(1);
        let zone = hit_test_zone(&app, 10, status_row);
        assert_eq!(
            zone,
            Zone::StatusLine,
            "click at row={status_row} (last row, no overlay) should be Zone::StatusLine; got {zone:?}"
        );
    }

    /// Click on a row ABOVE the status line must NOT return `Zone::StatusLine`.
    #[test]
    fn hit_test_zone_above_status_line_is_not_status_zone() {
        let app = make_basic_app_80x24();
        let screen = app.screen_rect();
        let above_status = screen.height.saturating_sub(2);
        let zone = hit_test_zone(&app, 10, above_status);
        assert!(
            !matches!(zone, Zone::StatusLine),
            "row above status line must not be Zone::StatusLine; got {zone:?}"
        );
    }

    /// When the picker is open, `hit_test_zone` must return `Zone::PickerRow` for
    /// cells inside the picker list area, and `Zone::None` for cells outside the
    /// picker overlay. No other zone should be returned regardless of what lies
    /// underneath the overlay.
    ///
    /// Picker geometry (80x24 viewport, no top bar, no preview because source
    /// has no preview):
    ///   buf_area = {0, 0, 80, 24}  (top_bar_h=0)
    ///   area = centered_rect(80, 70, buf_area)
    ///       width=64, height=16, x=8, y=4
    ///   left_area = area (no preview — has_preview=false keeps full area)
    ///   input_area = {x:8, y:4, w:64, h:3}
    ///   list_area  = {x:8, y:7, w:64, h:13}
    ///   list items start at row 8 (list_area.y + 1 = 7+1 = 8)
    #[test]
    fn hit_test_zone_picker_is_exclusive() {
        use crate::picker::Picker;
        use hjkl_picker::{PickerAction, PickerLogic, RequeryMode};
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        // Synchronous in-memory stub source. The previous version used
        // FileSourceWithOpen against std::env::temp_dir() which depends on
        // background enumeration finishing within Picker::new's 30ms wait —
        // racy on macOS/Windows CI runners (saw consistent failures pre-fix).
        // A no-I/O stub makes the test deterministic everywhere.
        struct StubSource(Vec<String>);
        impl PickerLogic for StubSource {
            fn title(&self) -> &str {
                "stub"
            }
            fn item_count(&self) -> usize {
                self.0.len()
            }
            fn label(&self, i: usize) -> String {
                self.0[i].clone()
            }
            fn match_text(&self, i: usize) -> String {
                self.0[i].clone()
            }
            fn has_preview(&self) -> bool {
                false
            }
            fn select(&self, _i: usize) -> PickerAction {
                PickerAction::None
            }
            fn requery_mode(&self) -> RequeryMode {
                RequeryMode::FilterInMemory
            }
            fn enumerate(
                &mut self,
                _q: Option<&str>,
                _c: Arc<AtomicBool>,
            ) -> Option<std::thread::JoinHandle<()>> {
                None
            }
        }

        let mut app = make_basic_app_80x24();
        let source = Box::new(StubSource(vec![
            "a".into(),
            "b".into(),
            "c".into(),
            "d".into(),
            "e".into(),
        ]));
        app.picker = Some(Picker::new(source));

        // Compute expected picker rect.
        let area = picker_overlay_rect(&app).expect("picker must be open");

        // Input area takes the first 3 rows; list area is the rest.
        let list_y = area.y + 3;
        let list_inner_y = list_y + 1; // inside list block border

        // A click inside the list content area.
        let col_inside = area.x + 2;
        let row_inside = list_inner_y;

        let zone = hit_test_zone(&app, col_inside, row_inside);
        assert!(
            matches!(zone, Zone::PickerRow { .. }),
            "click inside picker list (col={col_inside}, row={row_inside}) should be Zone::PickerRow; got {zone:?}"
        );

        // A click OUTSIDE the picker area entirely must be Zone::None.
        // The picker left edge is at area.x; click to the left of that.
        if area.x > 0 {
            let col_outside = 0;
            let row_outside = row_inside;
            let zone_out = hit_test_zone(&app, col_outside, row_outside);
            assert_eq!(
                zone_out,
                Zone::None,
                "click outside picker overlay must be Zone::None; got {zone_out:?}"
            );
        }
    }
}
