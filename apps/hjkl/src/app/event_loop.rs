use anyhow::Result;
use crossterm::{
    cursor::SetCursorStyle,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
};
use hjkl_engine::{CursorShape, Host, VimMode};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::Stdout;
use std::time::Duration;

use super::{App, STATUS_LINE_HEIGHT, SearchDir, prompt_cursor_shape};
use crate::render;

impl App {
    /// Main event loop. Draws every frame, routes key events through
    /// the vim FSM, handles resize, exits on Ctrl-C.
    pub fn run(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        loop {
            // Sync the focused window's stored scroll into the active editor
            // so the engine's scrolloff math starts from the correct baseline.
            self.sync_viewport_to_editor();

            // Drain any pending LSP events (non-blocking).
            self.drain_lsp_events();

            // Update host viewport dimensions from the current terminal size.
            {
                let size = terminal.size()?;
                let vp = self.active_mut().editor.host_mut().viewport_mut();
                vp.width = size.width;
                vp.height = size.height.saturating_sub(STATUS_LINE_HEIGHT);
            }

            // Emit cursor shape before the draw call, once per transition.
            let current_shape = if let Some(ref f) = self.command_field {
                prompt_cursor_shape(f)
            } else if let Some(ref f) = self.search_field {
                prompt_cursor_shape(f)
            } else {
                self.active().editor.host().cursor_shape()
            };
            if current_shape != self.last_cursor_shape {
                match current_shape {
                    CursorShape::Block => {
                        let _ = execute!(terminal.backend_mut(), SetCursorStyle::SteadyBlock);
                    }
                    CursorShape::Bar => {
                        let _ = execute!(terminal.backend_mut(), SetCursorStyle::SteadyBar);
                    }
                    CursorShape::Underline => {
                        let _ = execute!(terminal.backend_mut(), SetCursorStyle::SteadyUnderScore);
                    }
                }
                self.last_cursor_shape = current_shape;
            }

            // Draw the current frame.
            terminal.draw(|frame| render::frame(frame, self))?;

            // Poll any in-flight async grammar loads each tick so a freshly
            // compiled grammar installs without needing a keypress.
            if self.poll_grammar_loads() {
                self.recompute_and_install();
            }

            // Wait for the next event with a 120 ms ceiling so the splash
            // animation can repaint without input. The splash itself reads
            // the wall clock — we just need a redraw cadence here.
            if !event::poll(Duration::from_millis(120))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        if self.command_field.is_some() {
                            self.command_field = None;
                            continue;
                        }
                        if self.search_field.is_some() {
                            self.cancel_search_prompt();
                            continue;
                        }
                        break;
                    }

                    // Dismiss the start screen on any non-Ctrl-C keypress and
                    // let the key fall through to normal handling so `:`,
                    // `/`, `i`, etc. take effect on the same press.
                    if self.start_screen.is_some() {
                        self.start_screen = None;
                    }

                    self.status_message = None;

                    // ── Info popup dismissal ──────────────────────────────────
                    if self.info_popup.is_some() {
                        self.info_popup = None;
                        continue;
                    }

                    // ── Command palette (`:` prompt) ─────────────────────────
                    if self.command_field.is_some() {
                        self.handle_command_field_key(key);
                        if self.exit_requested {
                            break;
                        }
                        continue;
                    }

                    // ── Search prompt (`/` `?`) ──────────────────────────────
                    if self.search_field.is_some() {
                        self.handle_search_field_key(key);
                        if self.exit_requested {
                            break;
                        }
                        continue;
                    }

                    // ── Picker overlay ────────────────────────────────────────
                    if self.picker.is_some() {
                        self.handle_picker_key(key);
                        if self.exit_requested {
                            break;
                        }
                        continue;
                    }

                    // ── Git sub-command resolution ───────────────────────────
                    if self.pending_git && self.active().editor.vim_mode() == VimMode::Normal {
                        self.pending_git = false;
                        self.pending_leader = false;
                        match key.code {
                            KeyCode::Char('s') if key.modifiers == KeyModifiers::NONE => {
                                self.open_git_status_picker();
                            }
                            KeyCode::Char('l') if key.modifiers == KeyModifiers::NONE => {
                                self.open_git_log_picker();
                            }
                            KeyCode::Char('b') if key.modifiers == KeyModifiers::NONE => {
                                self.open_git_branch_picker();
                            }
                            // <leader>gB — file history for the current buffer.
                            // Uppercase B (Shift+b).
                            KeyCode::Char('B')
                                if key.modifiers == KeyModifiers::NONE
                                    || key.modifiers == KeyModifiers::SHIFT =>
                            {
                                self.open_git_file_history_picker();
                            }
                            // <leader>gS — stashes picker (uppercase S).
                            KeyCode::Char('S')
                                if key.modifiers == KeyModifiers::NONE
                                    || key.modifiers == KeyModifiers::SHIFT =>
                            {
                                self.open_git_stash_picker();
                            }
                            // <leader>gt — tags picker.
                            KeyCode::Char('t') if key.modifiers == KeyModifiers::NONE => {
                                self.open_git_tags_picker();
                            }
                            // <leader>gr — remotes picker.
                            KeyCode::Char('r') if key.modifiers == KeyModifiers::NONE => {
                                self.open_git_remotes_picker();
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // ── Leader resolution ────────────────────────────────────
                    let leader = self.config.editor.leader;
                    if self.pending_leader && self.active().editor.vim_mode() == VimMode::Normal {
                        self.pending_leader = false;
                        if key.modifiers == KeyModifiers::NONE {
                            match key.code {
                                // The leader key itself + 'f' both open the file picker
                                // (matches buffr-style "press leader twice or leader+f").
                                KeyCode::Char(c) if c == leader => {
                                    self.open_picker();
                                }
                                KeyCode::Char('f') => {
                                    self.open_picker();
                                }
                                KeyCode::Char('b') => {
                                    self.open_buffer_picker();
                                }
                                KeyCode::Char('/') => {
                                    self.open_grep_picker(None);
                                }
                                KeyCode::Char('g') => {
                                    // Begin git sub-command chord.
                                    self.pending_git = true;
                                }
                                KeyCode::Char('d') => {
                                    // <leader>d — show diag-at-cursor in info popup.
                                    self.show_diag_at_cursor();
                                }
                                _ => {}
                            }
                        }
                        continue;
                    }

                    // ── Leader prefix ────────────────────────────────────────
                    if key.code == KeyCode::Char(leader)
                        && key.modifiers == KeyModifiers::NONE
                        && self.active().editor.vim_mode() == VimMode::Normal
                    {
                        self.pending_leader = true;
                        continue;
                    }

                    // ── Ctrl-w window motion chord ───────────────────────────
                    if self.active().editor.vim_mode() == VimMode::Normal {
                        // Second key of a Ctrl-w chord.
                        if self.pending_window_motion {
                            self.pending_window_motion = false;
                            match key.code {
                                KeyCode::Char('j') => {
                                    self.focus_below();
                                }
                                KeyCode::Char('k') => {
                                    self.focus_above();
                                }
                                KeyCode::Char('h') => {
                                    self.focus_left();
                                }
                                KeyCode::Char('l') => {
                                    self.focus_right();
                                }
                                KeyCode::Char('w') => {
                                    self.focus_next();
                                }
                                KeyCode::Char('W') => {
                                    self.focus_previous();
                                }
                                KeyCode::Char('c') => {
                                    self.close_focused_window();
                                }
                                // Ctrl-w q: vim parity — close window when multiple,
                                // quit app when last.
                                KeyCode::Char('q') => {
                                    if self.layout().leaves().len() > 1 {
                                        self.close_focused_window();
                                    } else {
                                        self.exit_requested = true;
                                    }
                                }
                                // Ctrl-w o: close all windows except focused (:only).
                                KeyCode::Char('o') => {
                                    self.only_focused_window();
                                }
                                // Ctrl-w x / r / R: swap focused leaf with sibling.
                                KeyCode::Char('x') | KeyCode::Char('r') | KeyCode::Char('R') => {
                                    self.swap_with_sibling();
                                }
                                // Ctrl-w T: move focused window to a new tab.
                                KeyCode::Char('T') => match self.move_window_to_new_tab() {
                                    Ok(()) => {
                                        self.status_message =
                                            Some("moved window to new tab".into());
                                    }
                                    Err(msg) => {
                                        self.status_message = Some(msg.to_string());
                                    }
                                },
                                // Ctrl-w n: horizontal split with empty buffer (:new).
                                KeyCode::Char('n') => {
                                    self.dispatch_ex("new");
                                }
                                KeyCode::Char('+') => {
                                    self.resize_height(1);
                                }
                                KeyCode::Char('-') => {
                                    self.resize_height(-1);
                                }
                                KeyCode::Char('>') => {
                                    self.resize_width(1);
                                }
                                KeyCode::Char('<') => {
                                    self.resize_width(-1);
                                }
                                KeyCode::Char('=') => {
                                    self.equalize_layout();
                                }
                                KeyCode::Char('_') => {
                                    self.maximize_height();
                                }
                                KeyCode::Char('|') => {
                                    self.maximize_width();
                                }
                                _ => {} // unknown second key — consume and ignore
                            }
                            continue;
                        }
                        // First key: Ctrl-w sets pending.
                        if key.code == KeyCode::Char('w')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            self.pending_window_motion = true;
                            continue;
                        }
                        // tmux-navigator: bare Ctrl-j/k/h/l navigate splits
                        // in normal mode. Only intercept when a neighbour
                        // exists in that direction — otherwise let the key
                        // fall through to the engine so single-window users
                        // keep vim semantics (Ctrl-h = h, Ctrl-l = redraw).
                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                            let focused = self.focused_window();
                            if key.code == KeyCode::Char('j')
                                && self.layout().neighbor_below(focused).is_some()
                            {
                                self.focus_below();
                                continue;
                            }
                            if key.code == KeyCode::Char('k')
                                && self.layout().neighbor_above(focused).is_some()
                            {
                                self.focus_above();
                                continue;
                            }
                            if key.code == KeyCode::Char('h')
                                && self.layout().neighbor_left(focused).is_some()
                            {
                                self.focus_left();
                                continue;
                            }
                            if key.code == KeyCode::Char('l')
                                && self.layout().neighbor_right(focused).is_some()
                            {
                                self.focus_right();
                                continue;
                            }
                        }
                    } else {
                        // Any non-Normal mode clears the pending flag.
                        self.pending_window_motion = false;
                    }

                    // ── Alt-buffer toggle (Ctrl-^ / Ctrl-6) ─────────────────
                    if self.active().editor.vim_mode() == VimMode::Normal
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                        && (key.code == KeyCode::Char('^') || key.code == KeyCode::Char('6'))
                    {
                        self.buffer_alt();
                        continue;
                    }

                    // ── Shift-H / Shift-L cycle buffers ──────────────────────
                    // Only when more than one buffer is open; with a single
                    // slot fall through to the engine's H/L viewport motions.
                    if self.active().editor.vim_mode() == VimMode::Normal
                        && self.slots.len() > 1
                        && (key.modifiers == KeyModifiers::SHIFT
                            || key.modifiers == KeyModifiers::NONE)
                    {
                        if key.code == KeyCode::Char('H') {
                            self.buffer_prev();
                            continue;
                        }
                        if key.code == KeyCode::Char('L') {
                            self.buffer_next();
                            continue;
                        }
                    }

                    // ── Buffer-motion pending state ──────────────────────────
                    if self.active().editor.vim_mode() == VimMode::Normal
                        && key.modifiers == KeyModifiers::NONE
                    {
                        if let Some(prefix) = self.pending_buffer_motion.take() {
                            match (prefix, key.code) {
                                // TODO: [N]gt count prefix (e.g. `3gt` → jump to tab 3)
                                // is a separate effort; not wired here.
                                ('g', KeyCode::Char('t')) => {
                                    self.dispatch_ex("tabnext");
                                    continue;
                                }
                                ('g', KeyCode::Char('T')) => {
                                    self.dispatch_ex("tabprev");
                                    continue;
                                }
                                (']', KeyCode::Char('b')) => {
                                    self.buffer_next();
                                    continue;
                                }
                                ('[', KeyCode::Char('b')) => {
                                    self.buffer_prev();
                                    continue;
                                }
                                // ]d / [d — navigate diagnostics
                                (']', KeyCode::Char('d')) => {
                                    self.dispatch_ex("lnext");
                                    continue;
                                }
                                ('[', KeyCode::Char('d')) => {
                                    self.dispatch_ex("lprev");
                                    continue;
                                }
                                // ]D / [D — navigate error-only diagnostics
                                (']', KeyCode::Char('D')) => {
                                    self.lnext_severity(Some(super::DiagSeverity::Error));
                                    continue;
                                }
                                ('[', KeyCode::Char('D')) => {
                                    self.lprev_severity(Some(super::DiagSeverity::Error));
                                    continue;
                                }
                                // Didn't match — forward only the current key;
                                // drop the pending prefix (g/]/[ alone has no
                                // other mapped meaning in our engine yet).
                                _ => {
                                    self.active_mut().editor.handle_key(key);
                                    continue;
                                }
                            }
                        }
                    } else {
                        // Any non-Normal key clears pending motions.
                        self.pending_buffer_motion = None;
                        self.pending_git = false;
                        self.pending_leader = false;
                    }

                    // ── Intercept `:` in Normal mode ─────────────────────────
                    if key.code == KeyCode::Char(':')
                        && key.modifiers == KeyModifiers::NONE
                        && self.active().editor.vim_mode() == VimMode::Normal
                    {
                        self.open_command_prompt();
                        continue;
                    }

                    // ── Intercept `/` and `?` in Normal mode ─────────────────
                    if key.modifiers == KeyModifiers::NONE
                        && self.active().editor.vim_mode() == VimMode::Normal
                    {
                        if key.code == KeyCode::Char('/') {
                            self.open_search_prompt(SearchDir::Forward);
                            continue;
                        }
                        if key.code == KeyCode::Char('?') {
                            self.open_search_prompt(SearchDir::Backward);
                            continue;
                        }
                    }

                    // ── Set pending buffer-motion prefix ─────────────────────
                    if self.active().editor.vim_mode() == VimMode::Normal
                        && key.modifiers == KeyModifiers::NONE
                        && matches!(
                            key.code,
                            KeyCode::Char('g') | KeyCode::Char(']') | KeyCode::Char('[')
                        )
                        && let KeyCode::Char(c) = key.code
                    {
                        self.pending_buffer_motion = Some(c);
                        // Fall through: also forward the key to the engine
                        // so its own `g`-pending state is updated correctly
                        // (the engine handles gj/gk/gg/G etc).
                    }

                    // ── Normal editor key handling ───────────────────────────
                    self.active_mut().editor.handle_key(key);

                    // Persist auto-scroll changes made by the engine back into
                    // the focused window so they survive a window-focus switch.
                    self.sync_viewport_from_editor();

                    // Drain dirty for the persistent UI flag.
                    if self.active_mut().editor.take_dirty() {
                        let elapsed = self.active_mut().refresh_dirty_against_saved();
                        self.last_signature_us = elapsed;
                        if self.active().dirty {
                            self.active_mut().is_new_file = false;
                        }
                    }
                    // Fan engine ContentEdits into the syntax tree.
                    let buffer_id = self.active().buffer_id;
                    if self.active_mut().editor.take_content_reset() {
                        self.syntax.reset(buffer_id);
                    }
                    let edits = self.active_mut().editor.take_content_edits();
                    if !edits.is_empty() {
                        self.syntax.apply_edits(buffer_id, &edits);
                    }
                    // Notify LSP of content change (dirty-gen-gated debounce).
                    self.lsp_notify_change_active();
                    self.recompute_and_install();
                }
                Event::Resize(w, h) => {
                    // Update the active editor viewport so the engine sees
                    // the new dimensions; the renderer will repaint all panes.
                    let vp = self.active_mut().editor.host_mut().viewport_mut();
                    vp.width = w;
                    vp.height = h.saturating_sub(STATUS_LINE_HEIGHT);
                }
                Event::FocusGained => {
                    self.checktime_all();
                }
                _ => {}
            }

            if self.exit_requested {
                break;
            }
        }
        Ok(())
    }
}
