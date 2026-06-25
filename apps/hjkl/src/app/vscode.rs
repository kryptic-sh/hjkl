//! VSCode keybinding dispatcher (Slice 1 — Vim slice stays untouched).
//!
//! Non-modal "EDITOR" mode: the buffer is always in Insert mode. Navigation
//! and common Ctrl shortcuts (Save, Undo, Redo) are mapped; selection,
//! clipboard, find, and multi-cursor are tracked in epic #265 (V5–V8).
//!
//! Caller contract (mirrors the Insert-mode FallThrough path in `event_loop`):
//! - Call `dispatch_vscode_key` before the main vim routing.
//! - The caller runs all post-edit sync after this returns (viewport, dirty,
//!   content-reset, edits, LSP, sibling cursors, fold-ops, pending_recompute).
//! - Do NOT emit cursor shape or run sync here.

use super::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hjkl_engine::{InsertDir, VimMode};

impl App {
    /// Dispatch a single key event in VSCode (non-modal) mode.
    ///
    /// Ensures the engine is always in Insert mode, then maps keys to insert
    /// primitives and a small set of Ctrl shortcuts.
    pub(crate) fn dispatch_vscode_key(&mut self, key: KeyEvent) {
        // VSCode mode is always "in insert mode" internally so the bar cursor
        // and insert primitives both apply. Enter insert at the cursor if not
        // already there. (On first use after launch the engine is in Normal.)
        if self.active_editor().vim_mode() != VimMode::Insert {
            self.active_editor_mut().enter_insert_i(1);
        }

        match (key.code, key.modifiers) {
            // ── Ctrl shortcuts ────────────────────────────────────────────────

            // Ctrl+S → save
            (KeyCode::Char('s'), KeyModifiers::CONTROL) => {
                self.do_save(None);
            }

            // Ctrl+Z → undo
            (KeyCode::Char('z'), KeyModifiers::CONTROL) => {
                self.active_editor_mut().undo();
            }

            // Ctrl+Y → redo
            (KeyCode::Char('y'), KeyModifiers::CONTROL) => {
                self.active_editor_mut().redo();
            }

            // Ctrl+Shift+Z → redo (common alternative)
            (KeyCode::Char('z'), mods) if mods == KeyModifiers::CONTROL | KeyModifiers::SHIFT => {
                self.active_editor_mut().redo();
            }

            // Esc → no-op in VSCode mode (non-modal; do NOT leave Insert)
            (KeyCode::Esc, _) => {}

            // ── Printable characters ──────────────────────────────────────────
            (KeyCode::Char(c), mods)
                if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT =>
            {
                self.active_editor_mut().insert_char(c);
            }

            // ── Editing keys ──────────────────────────────────────────────────
            (KeyCode::Backspace, _) => self.active_editor_mut().insert_backspace(),
            (KeyCode::Enter, _) => self.active_editor_mut().insert_newline(),
            (KeyCode::Tab, _) => self.active_editor_mut().insert_tab(),
            (KeyCode::Delete, _) => self.active_editor_mut().insert_delete(),
            (KeyCode::Home, _) => self.active_editor_mut().insert_home(),
            (KeyCode::End, _) => self.active_editor_mut().insert_end(),

            // ── Arrow navigation ──────────────────────────────────────────────
            (KeyCode::Left, _) => self.active_editor_mut().insert_arrow(InsertDir::Left),
            (KeyCode::Right, _) => self.active_editor_mut().insert_arrow(InsertDir::Right),
            (KeyCode::Up, _) => self.active_editor_mut().insert_arrow(InsertDir::Up),
            (KeyCode::Down, _) => self.active_editor_mut().insert_arrow(InsertDir::Down),

            // ── Page navigation ───────────────────────────────────────────────
            (KeyCode::PageUp, _) => {
                let h = self.active_editor().viewport_height_value();
                self.active_editor_mut().insert_pageup(h);
            }
            (KeyCode::PageDown, _) => {
                let h = self.active_editor().viewport_height_value();
                self.active_editor_mut().insert_pagedown(h);
            }

            // Drop anything else (function keys, alt combos, …)
            _ => {}
        }
    }
}
