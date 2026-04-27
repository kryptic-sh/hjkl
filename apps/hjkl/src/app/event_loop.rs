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

            // Wait for the next event with a 120 ms ceiling.
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

                    // ── Leader resolution ────────────────────────────────────
                    if self.pending_leader && self.active().editor.vim_mode() == VimMode::Normal {
                        self.pending_leader = false;
                        if key.modifiers == KeyModifiers::NONE {
                            match key.code {
                                KeyCode::Char(' ') | KeyCode::Char('f') => {
                                    self.open_picker();
                                }
                                KeyCode::Char('b') => {
                                    self.open_buffer_picker();
                                }
                                KeyCode::Char('/') => {
                                    self.open_grep_picker(None);
                                }
                                _ => {}
                            }
                        }
                        continue;
                    }

                    // ── Leader prefix ────────────────────────────────────────
                    if key.code == KeyCode::Char(' ')
                        && key.modifiers == KeyModifiers::NONE
                        && self.active().editor.vim_mode() == VimMode::Normal
                    {
                        self.pending_leader = true;
                        continue;
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
                                ('g', KeyCode::Char('t')) => {
                                    self.buffer_next();
                                    continue;
                                }
                                ('g', KeyCode::Char('T')) => {
                                    self.buffer_prev();
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
                        // Any non-Normal key clears the pending motion.
                        self.pending_buffer_motion = None;
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
                    self.recompute_and_install();
                }
                Event::Resize(w, h) => {
                    let vp = self.active_mut().editor.host_mut().viewport_mut();
                    vp.width = w;
                    vp.height = h.saturating_sub(STATUS_LINE_HEIGHT);
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
