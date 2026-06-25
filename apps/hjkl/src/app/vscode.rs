//! VSCode keybinding dispatcher (V5: selection + non-modal typing/nav).
//!
//! Non-modal "EDITOR" mode: there are two underlying states —
//!   • INSERT  — no active selection; caret between chars (bar cursor).
//!   • VISUAL  — a char-wise selection exists; the selection is **exclusive**
//!               (the caret sits *before* the cell at `cursor()`).
//!
//! Shift+arrows extend/begin the selection; plain arrows collapse it.
//! Typing / Backspace / Delete with an active selection replaces / deletes it.
//! Ctrl+A selects the whole buffer. Clipboard (Ctrl+C/X/V) is V6.
//!
//! Caller contract (mirrors the Insert-mode FallThrough path in `event_loop`):
//! - Call `dispatch_vscode_key` before the main vim routing.
//! - The caller runs all post-edit sync after this returns (viewport, dirty,
//!   content-reset, edits, LSP, sibling cursors, fold-ops, pending_recompute).
//! - Do NOT emit cursor shape or run sync here.

use super::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hjkl_engine::{BufferEdit, InsertDir, Pos, VimMode};

impl App {
    /// Dispatch a single key event in VSCode (non-modal) mode.
    ///
    /// Home state is INSERT. A charwise-Visual selection is the "selection"
    /// state — it is exclusive (half-open end, `selection_exclusive = true`
    /// on every editor in vscode mode).
    pub(crate) fn dispatch_vscode_key(&mut self, key: KeyEvent) {
        // ── ensure the editor starts in Insert (first call after launch) ─────
        // When no selection is active the engine must be in Insert. If we're
        // in Visual (selection active) we stay there; if in Normal (startup or
        // after some internal transition) we force Insert.
        {
            let mode = self.active_editor().vim_mode();
            if mode != VimMode::Insert && mode != VimMode::Visual {
                self.active_editor_mut().enter_insert_i(1);
            }
        }

        match (key.code, key.modifiers) {
            // ── Ctrl shortcuts ─────────────────────────────────────────────────

            // Ctrl+S → save
            (KeyCode::Char('s'), KeyModifiers::CONTROL) => {
                self.do_save(None);
            }

            // Ctrl+Z → undo
            (KeyCode::Char('z'), KeyModifiers::CONTROL) => {
                self.collapse_to_insert_if_visual();
                self.active_editor_mut().undo();
            }

            // Ctrl+Y → redo
            (KeyCode::Char('y'), KeyModifiers::CONTROL) => {
                self.collapse_to_insert_if_visual();
                self.active_editor_mut().redo();
            }

            // Ctrl+Shift+Z → redo (common alternative)
            (KeyCode::Char('z'), mods) if mods == KeyModifiers::CONTROL | KeyModifiers::SHIFT => {
                self.collapse_to_insert_if_visual();
                self.active_editor_mut().redo();
            }

            // Ctrl+A → select whole buffer
            (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.vscode_select_all();
            }

            // Esc → collapse any selection, stay in Insert (non-modal; do NOT
            // leave Insert the way vim does)
            (KeyCode::Esc, _) => {
                self.collapse_to_insert_if_visual();
            }

            // ── Shift+arrow: begin or extend selection ─────────────────────────
            (KeyCode::Left, mods) if mods.contains(KeyModifiers::SHIFT) => {
                self.vscode_shift_arrow(ShiftDir::Left);
            }
            (KeyCode::Right, mods) if mods.contains(KeyModifiers::SHIFT) => {
                self.vscode_shift_arrow(ShiftDir::Right);
            }
            (KeyCode::Up, mods) if mods.contains(KeyModifiers::SHIFT) => {
                self.vscode_shift_arrow(ShiftDir::Up);
            }
            (KeyCode::Down, mods) if mods.contains(KeyModifiers::SHIFT) => {
                self.vscode_shift_arrow(ShiftDir::Down);
            }
            (KeyCode::Home, mods) if mods.contains(KeyModifiers::SHIFT) => {
                self.vscode_shift_arrow(ShiftDir::Home);
            }
            (KeyCode::End, mods) if mods.contains(KeyModifiers::SHIFT) => {
                self.vscode_shift_arrow(ShiftDir::End);
            }

            // ── Plain arrows: collapse selection then navigate ─────────────────
            (KeyCode::Left, KeyModifiers::NONE) => {
                self.vscode_plain_arrow(InsertDir::Left);
            }
            (KeyCode::Right, KeyModifiers::NONE) => {
                self.vscode_plain_arrow(InsertDir::Right);
            }
            (KeyCode::Up, KeyModifiers::NONE) => {
                self.vscode_plain_arrow(InsertDir::Up);
            }
            (KeyCode::Down, KeyModifiers::NONE) => {
                self.vscode_plain_arrow(InsertDir::Down);
            }
            (KeyCode::Home, KeyModifiers::NONE) => {
                self.vscode_plain_home_end(false);
            }
            (KeyCode::End, KeyModifiers::NONE) => {
                self.vscode_plain_home_end(true);
            }

            // ── Printable characters ───────────────────────────────────────────
            (KeyCode::Char(c), mods)
                if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT =>
            {
                if self.vscode_has_selection() {
                    self.vscode_delete_selection();
                }
                self.active_editor_mut().insert_char(c);
            }

            // ── Editing keys ───────────────────────────────────────────────────
            (KeyCode::Backspace, _) => {
                if self.vscode_has_selection() {
                    self.vscode_delete_selection();
                } else {
                    self.active_editor_mut().insert_backspace();
                }
            }
            (KeyCode::Delete, _) => {
                if self.vscode_has_selection() {
                    self.vscode_delete_selection();
                } else {
                    self.active_editor_mut().insert_delete();
                }
            }
            (KeyCode::Enter, _) => {
                if self.vscode_has_selection() {
                    self.vscode_delete_selection();
                }
                self.active_editor_mut().insert_newline();
            }
            (KeyCode::Tab, _) => {
                if self.vscode_has_selection() {
                    self.vscode_delete_selection();
                }
                self.active_editor_mut().insert_tab();
            }

            // ── Page navigation ────────────────────────────────────────────────
            (KeyCode::PageUp, _) => {
                self.collapse_to_insert_if_visual();
                let h = self.active_editor().viewport_height_value();
                self.active_editor_mut().insert_pageup(h);
            }
            (KeyCode::PageDown, _) => {
                self.collapse_to_insert_if_visual();
                let h = self.active_editor().viewport_height_value();
                self.active_editor_mut().insert_pagedown(h);
            }

            // Drop anything else (function keys, alt combos, …)
            _ => {}
        }
    }

    // ── Selection helpers ──────────────────────────────────────────────────────

    /// `true` when there is an active (non-empty) VSCode selection.
    fn vscode_has_selection(&self) -> bool {
        let ed = self.active_editor();
        ed.vim_mode() == VimMode::Visual && ed.visual_char_range_exclusive().is_some()
    }

    /// Collapse a Visual selection back to Insert mode. The caret stays where
    /// it is (anchor is discarded). No-op when already in Insert.
    fn collapse_to_insert_if_visual(&mut self) {
        if self.active_editor().vim_mode() == VimMode::Visual {
            self.active_editor_mut().exit_visual_to_normal();
            self.active_editor_mut().enter_insert_i(1);
        }
    }

    /// Delete the current exclusive VSCode selection and return to Insert.
    /// The caret lands at `start` (the lower of anchor/cursor).
    ///
    /// Uses `BufferEdit::delete_range` directly (half-open, exactly what
    /// exclusive VSCode selection expresses) — no vim register or undo-group
    /// special-casing needed.
    fn vscode_delete_selection(&mut self) {
        let range = self.active_editor().visual_char_range_exclusive();
        if let Some(((sr, sc), (er, ec))) = range {
            let start = Pos::new(sr as u32, sc as u32);
            let end = Pos::new(er as u32, ec as u32);
            // Exit visual first, re-enter insert.
            self.active_editor_mut().exit_visual_to_normal();
            self.active_editor_mut().enter_insert_i(1);
            // Delete the half-open range [start, end).
            BufferEdit::delete_range(self.active_editor_mut().buffer_mut(), start..end);
            // Place the caret at start.
            self.active_editor_mut().set_cursor_doc(sr, sc);
            // Mark content dirty so sync path picks up the change.
            self.active_editor_mut().mark_content_dirty();
        }
    }

    /// Ctrl+A — select the whole buffer. Anchor at (0,0), caret at the
    /// exclusive end of the last line (char_len of last line).
    fn vscode_select_all(&mut self) {
        // Collapse any existing selection first.
        if self.active_editor().vim_mode() == VimMode::Visual {
            self.active_editor_mut().exit_visual_to_normal();
        }
        // Move to (0,0) and enter visual.
        self.active_editor_mut().enter_insert_i(1);
        self.active_editor_mut().set_cursor_doc(0, 0);
        self.active_editor_mut().enter_visual_char();
        // Move the caret to the exclusive end of the buffer.
        let (last_row, last_col) = {
            let ed = self.active_editor();
            let row_count = ed.row_count();
            if row_count == 0 {
                (0, 0)
            } else {
                let last_r = row_count - 1;
                let last_c = ed.line_char_count(last_r);
                (last_r, last_c)
            }
        };
        self.active_editor_mut().set_cursor_doc(last_row, last_col);
    }

    /// Shift+arrow: begin or extend the selection.
    fn vscode_shift_arrow(&mut self, dir: ShiftDir) {
        let in_visual = self.active_editor().vim_mode() == VimMode::Visual;
        // If Insert → anchor at current cursor, enter visual.
        if !in_visual {
            let (row, col) = self.active_editor().cursor();
            // Enter insert first (may already be there) then enter visual char.
            // (The engine anchors at the cursor when entering visual.)
            let _ = (row, col); // anchor is implicit in enter_visual_char
            self.active_editor_mut().enter_visual_char();
        }
        // Move the caret one step in the requested direction.
        let (new_row, new_col) = self.vscode_compute_move(dir);
        let anchor = self.active_editor().visual_anchor();
        self.active_editor_mut().set_cursor_doc(new_row, new_col);
        // If the caret returned to the anchor → empty selection → collapse.
        let cursor = self.active_editor().cursor();
        if cursor == anchor {
            self.active_editor_mut().exit_visual_to_normal();
            self.active_editor_mut().enter_insert_i(1);
        }
    }

    /// Plain (no-shift) arrow: if a selection is active, collapse to the
    /// appropriate boundary; if not, perform the insert-mode arrow move.
    fn vscode_plain_arrow(&mut self, dir: InsertDir) {
        if self.vscode_has_selection() {
            // VSCode collapses to selection start for Left/Up, end for Right/Down.
            let use_start = matches!(dir, InsertDir::Left | InsertDir::Up);
            let (row, col) = {
                let ed = self.active_editor();
                if let Some(((sr, sc), (er, ec))) = ed.visual_char_range_exclusive() {
                    if use_start { (sr, sc) } else { (er, ec) }
                } else {
                    ed.cursor()
                }
            };
            self.active_editor_mut().exit_visual_to_normal();
            self.active_editor_mut().enter_insert_i(1);
            self.active_editor_mut().set_cursor_doc(row, col);
        } else {
            self.active_editor_mut().insert_arrow(dir);
        }
    }

    /// Plain Home/End: if a selection is active, collapse (Home → start, End →
    /// end); if not, perform the insert-mode home/end move.
    fn vscode_plain_home_end(&mut self, is_end: bool) {
        if self.vscode_has_selection() {
            let (row, col) = {
                let ed = self.active_editor();
                if let Some(((sr, sc), (er, ec))) = ed.visual_char_range_exclusive() {
                    if is_end { (er, ec) } else { (sr, sc) }
                } else {
                    ed.cursor()
                }
            };
            self.active_editor_mut().exit_visual_to_normal();
            self.active_editor_mut().enter_insert_i(1);
            self.active_editor_mut().set_cursor_doc(row, col);
        } else if is_end {
            self.active_editor_mut().insert_end();
        } else {
            self.active_editor_mut().insert_home();
        }
    }

    /// Compute the caret position after a one-step shift-arrow move.
    /// The editor may be in Visual mode when this is called.
    fn vscode_compute_move(&self, dir: ShiftDir) -> (usize, usize) {
        let ed = self.active_editor();
        let (row, col) = ed.cursor();
        match dir {
            ShiftDir::Left => {
                if col > 0 {
                    (row, col - 1)
                } else if row > 0 {
                    // Wrap to end of previous line.
                    let prev_len = ed.line_char_count(row - 1);
                    (row - 1, prev_len)
                } else {
                    (0, 0)
                }
            }
            ShiftDir::Right => {
                let line_len = ed.line_char_count(row);
                let row_count = ed.row_count();
                if col < line_len {
                    (row, col + 1)
                } else if row + 1 < row_count {
                    // Wrap to start of next line.
                    (row + 1, 0)
                } else {
                    (row, col)
                }
            }
            ShiftDir::Up => {
                if row > 0 {
                    let new_col = col.min(ed.line_char_count(row - 1));
                    (row - 1, new_col)
                } else {
                    (0, 0)
                }
            }
            ShiftDir::Down => {
                let row_count = ed.row_count();
                if row + 1 < row_count {
                    let new_col = col.min(ed.line_char_count(row + 1));
                    (row + 1, new_col)
                } else {
                    // Already on last row — move to exclusive end of line.
                    let line_len = ed.line_char_count(row);
                    (row, line_len)
                }
            }
            ShiftDir::Home => (row, 0),
            ShiftDir::End => {
                let line_len = ed.line_char_count(row);
                (row, line_len)
            }
        }
    }
}

/// Direction for a Shift+arrow move (begins or extends a selection).
#[derive(Debug, Clone, Copy)]
enum ShiftDir {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}
