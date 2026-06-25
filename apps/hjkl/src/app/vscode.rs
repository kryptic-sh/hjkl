//! VSCode keybinding dispatcher (V7: find / Ctrl+F).
//!
//! Non-modal "EDITOR" mode: there are two underlying states —
//!   • INSERT  — no active selection; caret between chars (bar cursor).
//!   • VISUAL  — a char-wise selection exists; the selection is **exclusive**
//!               (the caret sits *before* the cell at `cursor()`).
//!
//! Shift+arrows extend/begin the selection; plain arrows collapse it.
//! Typing / Backspace / Delete with an active selection replaces / deletes it.
//! Ctrl+A selects the whole buffer.
//!
//! ## V6 clipboard (Ctrl+C / Ctrl+X / Ctrl+V)
//!
//! Cut/copy write to BOTH the unnamed register (`"`) for in-session round-trips
//! AND the system clipboard (`host.write_clipboard`) for cross-app use.
//!
//! Paste reads the OS clipboard first (`host.read_clipboard()`), falling back
//! to the unnamed register when the OS clipboard is unreadable or `None` — so
//! in-session cut→paste round-trips work deterministically in CI (where OSC52
//! is write-only / unreadable).
//!
//! Ctrl+C without a selection is a no-op (the caller keeps the `Ctrl+C →
//! quit` path alive when there is no selection — see `event_loop.rs`).
//!
//! ### Double-sync avoidance
//!
//! `dispatch_vscode_key` must NOT call `sync_after_engine_mutation` internally;
//! the caller (`event_loop.rs`) runs the full post-dispatch sync block after
//! every call. For paste we use a private `vscode_insert_text` helper that
//! calls `insert_str` + `mark_content_dirty` — matching what `handle_paste`
//! does but without the extra `sync_after_engine_mutation` call inside it.
//!
//! Caller contract (mirrors the Insert-mode FallThrough path in `event_loop`):
//! - Call `dispatch_vscode_key` before the main vim routing.
//! - The caller runs all post-edit sync after this returns (viewport, dirty,
//!   content-reset, edits, LSP, sibling cursors, fold-ops, pending_recompute).
//! - Do NOT emit cursor shape or run sync here.

use super::{App, SearchDir};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hjkl_engine::{BufferEdit, Host, InsertDir, Pos, VimMode};

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

            // ── V7 find (Ctrl+F / F3 / Shift+F3) ─────────────────────────

            // Ctrl+F → open the incremental search prompt (forward).
            // If a selection is active, seed the find box with the selected
            // text (VSCode behaviour) — one-liner via set_text.
            (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                let seed = if self.vscode_has_selection() {
                    self.vscode_selection_text()
                } else {
                    None
                };
                self.open_search_prompt(SearchDir::Forward);
                if let (Some(text), Some(field)) = (seed, self.search_field.as_mut()) {
                    field.set_text(&text);
                    field.enter_insert_at_end();
                }
            }

            // F3 → next match (repeat last search forward).
            (KeyCode::F(3), KeyModifiers::NONE) => {
                self.active_editor_mut().search_repeat(true, 1);
            }

            // Shift+F3 → previous match (repeat last search backward).
            (KeyCode::F(3), mods) if mods.contains(KeyModifiers::SHIFT) => {
                self.active_editor_mut().search_repeat(false, 1);
            }

            // ── V6 clipboard chords ────────────────────────────────────────

            // Ctrl+C → copy selection (no-op when no selection; the caller
            // keeps Ctrl+C → quit alive when vscode_has_selection() is false,
            // so this arm only fires when a selection is active).
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                if self.vscode_has_selection() {
                    self.vscode_copy_selection();
                }
                // No selection: fall through (no-op here); the event_loop guard
                // ensures we only reach this arm when a selection is present.
            }

            // Ctrl+X → cut selection (copy + delete). No selection → no-op.
            // NOTE: VSCode cuts the whole line on empty selection; that is a
            // planned follow-up — not implemented here.
            (KeyCode::Char('x'), KeyModifiers::CONTROL) => {
                if self.vscode_has_selection() {
                    self.vscode_copy_selection();
                    self.vscode_delete_selection();
                }
            }

            // Ctrl+V → paste. Reads OS clipboard first, falls back to the
            // unnamed register. If a selection is active, deletes it first
            // (→ INSERT) then inserts the pasted text at the caret.
            (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                // Read OS clipboard first (split borrow: host_mut is released
                // before we read the register).
                let clip = self.active_editor_mut().host_mut().read_clipboard();
                let clip_text = clip.filter(|s| !s.is_empty());
                // Fall back to unnamed register when OS clipboard is
                // unreadable (CI/PTY env where OSC52 is write-only).
                let reg_text = if clip_text.is_none() {
                    let t = self
                        .active_editor()
                        .registers()
                        .read('"')
                        .map(|s| s.text.clone())
                        .unwrap_or_default();
                    if t.is_empty() { None } else { Some(t) }
                } else {
                    None
                };
                let text = clip_text.or(reg_text);
                if let Some(text) = text {
                    if self.vscode_has_selection() {
                        self.vscode_delete_selection();
                    }
                    self.vscode_insert_text(&text);
                }
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
    /// Exposed as `pub(crate)` so `event_loop.rs` can check it inside the
    /// Ctrl+C guard without entering `dispatch_vscode_key`.
    pub(crate) fn vscode_has_selection(&self) -> bool {
        let ed = self.active_editor();
        ed.vim_mode() == VimMode::Visual && ed.visual_char_range_exclusive().is_some()
    }

    // ── V6 clipboard helpers ───────────────────────────────────────────────────

    /// Extract the text covered by the current exclusive VSCode selection.
    ///
    /// Returns `None` when no selection is active. Reads directly from the
    /// buffer rope using char indices so the result is exactly the half-open
    /// range — no trailing character, no linewise newline expansion, and
    /// correct for multi-byte/multi-codepoint content.
    fn vscode_selection_text(&self) -> Option<String> {
        let ((sr, sc), (er, ec)) = self.active_editor().visual_char_range_exclusive()?;
        let rope = self.active_editor().buffer().rope();
        // Convert (row, char-col) to absolute char indices. ropey's
        // `line_to_char` returns the char index of the first codepoint of the
        // row (including the trailing '\n' of the previous row), so adding the
        // column gives the exact codepoint position.
        let start_char = rope.line_to_char(sr) + sc;
        let end_char = rope.line_to_char(er) + ec;
        Some(rope.slice(start_char..end_char).to_string())
    }

    /// Write the current selection text to both the unnamed register and the
    /// system clipboard. Does NOT modify the buffer or change mode.
    fn vscode_copy_selection(&mut self) {
        let text = match self.vscode_selection_text() {
            Some(t) if !t.is_empty() => t,
            _ => return,
        };
        // Write to unnamed register (in-session fallback for CI / read-only clipboard).
        self.active_editor_mut().set_yank(text.clone());
        // Write to system clipboard (fire-and-forget; host queues the write).
        self.active_editor_mut().host_mut().write_clipboard(text);
    }

    /// Insert `text` at the current caret in Insert mode.
    ///
    /// Mirrors the body of `handle_paste` (CRLF normalisation + `insert_str`)
    /// but does NOT call `sync_after_engine_mutation` — the caller's post-
    /// dispatch sync block in `event_loop.rs` handles that.
    fn vscode_insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        // Ensure we are in Insert before inserting.
        if self.active_editor().vim_mode() != VimMode::Insert {
            self.active_editor_mut().enter_insert_i(1);
        }
        let normalised = if text.contains('\r') {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.to_owned()
        };
        self.active_editor_mut().insert_str(&normalised);
        self.active_editor_mut().mark_content_dirty();
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
