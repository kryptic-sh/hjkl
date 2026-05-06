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

                    // ── LSP chord resolution (<leader>c{a} / <leader>r{n}) ───
                    if let Some(lsp_prefix) = self.pending_lsp.take() {
                        if self.active().editor.vim_mode() == VimMode::Normal {
                            self.pending_leader = false;
                            match (lsp_prefix, key.code) {
                                ('c', KeyCode::Char('a')) => {
                                    self.lsp_code_actions();
                                }
                                ('r', KeyCode::Char('n')) => {
                                    // Phase 5 MVP: prompt user to use :Rename <newname>.
                                    // TODO: open inline prompt pre-filled with word-at-cursor.
                                    self.status_message =
                                        Some("use :Rename <newname> to rename".into());
                                }
                                _ => {}
                            }
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
                                KeyCode::Char('c') => {
                                    // Begin LSP 'c' sub-command chord (<leader>ca = code actions).
                                    self.pending_lsp = Some('c');
                                    self.pending_leader = false;
                                }
                                KeyCode::Char('r') => {
                                    // Begin LSP 'r' sub-command chord (<leader>rn = rename).
                                    self.pending_lsp = Some('r');
                                    self.pending_leader = false;
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
                        // tmux-navigator: bare Ctrl-h/j/k/l in Normal mode.
                        // When a hjkl neighbour exists → focus it.
                        // When at the edge → fall through to `tmux select-pane`
                        // if $TMUX is set; otherwise silently no-op so we don't
                        // accidentally trigger engine bindings (Ctrl-h = BS,
                        // Ctrl-l = redraw) while the user intends navigation.
                        //
                        // Ctrl-h note: many terminals deliver Ctrl-h as
                        // KeyCode::Backspace with CONTROL modifier rather than
                        // KeyCode::Char('h') + CONTROL. We match both forms.
                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                            let focused = self.focused_window();
                            let is_ctrl_h =
                                key.code == KeyCode::Char('h') || key.code == KeyCode::Backspace;
                            let is_ctrl_j = key.code == KeyCode::Char('j');
                            let is_ctrl_k = key.code == KeyCode::Char('k');
                            let is_ctrl_l = key.code == KeyCode::Char('l');

                            if is_ctrl_h || is_ctrl_j || is_ctrl_k || is_ctrl_l {
                                let neighbour = if is_ctrl_h {
                                    self.layout().neighbor_left(focused)
                                } else if is_ctrl_j {
                                    self.layout().neighbor_below(focused)
                                } else if is_ctrl_k {
                                    self.layout().neighbor_above(focused)
                                } else {
                                    self.layout().neighbor_right(focused)
                                };

                                if neighbour.is_some() {
                                    // Neighbour exists — move focus within hjkl.
                                    if is_ctrl_h {
                                        self.focus_left();
                                    } else if is_ctrl_j {
                                        self.focus_below();
                                    } else if is_ctrl_k {
                                        self.focus_above();
                                    } else {
                                        self.focus_right();
                                    }
                                } else {
                                    // At the edge — hand off to tmux when available.
                                    if std::env::var("TMUX").is_ok() {
                                        let flag = if is_ctrl_h {
                                            "-L"
                                        } else if is_ctrl_j {
                                            "-D"
                                        } else if is_ctrl_k {
                                            "-U"
                                        } else {
                                            "-R"
                                        };
                                        // Ignore errors: non-zero exit (no tmux pane
                                        // in that direction) is a silent no-op.
                                        let _ = std::process::Command::new("tmux")
                                            .args(["select-pane", flag])
                                            .status();
                                    }
                                    // $TMUX not set → silent no-op.
                                }
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
                                // LSP goto motions (g-prefix)
                                ('g', KeyCode::Char('d')) => {
                                    self.lsp_goto_definition();
                                    continue;
                                }
                                ('g', KeyCode::Char('D')) => {
                                    self.lsp_goto_declaration();
                                    continue;
                                }
                                ('g', KeyCode::Char('r')) => {
                                    self.lsp_goto_references();
                                    continue;
                                }
                                ('g', KeyCode::Char('i')) => {
                                    self.lsp_goto_implementation();
                                    continue;
                                }
                                ('g', KeyCode::Char('y')) => {
                                    self.lsp_goto_type_definition();
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
                                // Engine-handled motions like gg/gj/gk/G need
                                // the viewport synced back so the focused
                                // window's stored top_row picks up the
                                // engine's auto-scroll.
                                _ => {
                                    self.active_mut().editor.handle_key(key);
                                    self.sync_viewport_from_editor();
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

                    // ── LSP hover (`K`) in Normal mode ───────────────────────
                    // TODO: Ctrl-w K (split-then-hover) deferred; popup hover works.
                    if key.code == KeyCode::Char('K')
                        && (key.modifiers == KeyModifiers::NONE
                            || key.modifiers == KeyModifiers::SHIFT)
                        && self.active().editor.vim_mode() == VimMode::Normal
                    {
                        self.lsp_hover();
                        continue;
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

                    // ── Insert-mode completion key handling ──────────────────
                    // This block intercepts specific keys in insert mode to
                    // manage the completion popup, before forwarding to the engine.
                    if self.active().editor.vim_mode() == VimMode::Insert {
                        // <C-x><C-o> manual omni-completion trigger.
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.code == KeyCode::Char('x')
                        {
                            self.pending_ctrl_x = true;
                            continue;
                        }
                        if self.pending_ctrl_x {
                            self.pending_ctrl_x = false;
                            if key.modifiers.contains(KeyModifiers::CONTROL)
                                && key.code == KeyCode::Char('o')
                            {
                                self.lsp_request_completion();
                                continue;
                            }
                            // Any other key: fall through normally (consume pending_ctrl_x).
                        }

                        // Keys that navigate/accept/dismiss the popup (popup must be open).
                        if self.completion.is_some() {
                            match key.code {
                                // <C-n> / <C-p> navigate selection.
                                KeyCode::Char('n')
                                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    if let Some(ref mut p) = self.completion {
                                        p.select_next();
                                    }
                                    continue;
                                }
                                KeyCode::Char('p')
                                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    if let Some(ref mut p) = self.completion {
                                        p.select_prev();
                                    }
                                    continue;
                                }
                                // <Tab> or <C-y> accept selected item.
                                KeyCode::Tab => {
                                    self.accept_completion();
                                    continue;
                                }
                                KeyCode::Char('y')
                                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    self.accept_completion();
                                    continue;
                                }
                                // <C-e> dismiss without accepting.
                                KeyCode::Char('e')
                                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    self.dismiss_completion();
                                    continue;
                                }
                                // <Esc> dismisses popup and falls through to engine
                                // which exits insert mode.
                                KeyCode::Esc => {
                                    self.dismiss_completion();
                                    // fall through to engine
                                }
                                // Printable char or backspace: update prefix, maybe dismiss.
                                KeyCode::Char(c) if key.modifiers == KeyModifiers::NONE => {
                                    // Let engine handle it first — we update prefix after.
                                    self.active_mut().editor.handle_key(key);
                                    self.sync_viewport_from_editor();
                                    if self.active_mut().editor.take_dirty() {
                                        let elapsed =
                                            self.active_mut().refresh_dirty_against_saved();
                                        self.last_signature_us = elapsed;
                                        if self.active().dirty {
                                            self.active_mut().is_new_file = false;
                                        }
                                    }
                                    let buffer_id = self.active().buffer_id;
                                    if self.active_mut().editor.take_content_reset() {
                                        self.syntax.reset(buffer_id);
                                    }
                                    let edits = self.active_mut().editor.take_content_edits();
                                    if !edits.is_empty() {
                                        self.syntax.apply_edits(buffer_id, &edits);
                                    }
                                    self.lsp_notify_change_active();
                                    self.recompute_and_install();

                                    // Update popup prefix.
                                    let anchor_col =
                                        self.completion.as_ref().map(|p| p.anchor_col).unwrap_or(0);
                                    let cur_col = self.active().editor.buffer().cursor().col;
                                    let cur_row = self.active().editor.buffer().cursor().row;
                                    let anchor_row = self
                                        .completion
                                        .as_ref()
                                        .map(|p| p.anchor_row)
                                        .unwrap_or(cur_row);
                                    if cur_row != anchor_row || cur_col < anchor_col {
                                        // Cursor moved out of anchor range — dismiss.
                                        self.dismiss_completion();
                                    } else {
                                        let new_prefix = {
                                            let line = self
                                                .active()
                                                .editor
                                                .buffer()
                                                .lines()
                                                .get(cur_row)
                                                .cloned()
                                                .unwrap_or_default();
                                            line[anchor_col.min(line.len())
                                                ..cur_col.min(line.len())]
                                                .to_string()
                                        };
                                        if let Some(ref mut popup) = self.completion {
                                            popup.set_prefix(&new_prefix);
                                            if popup.is_empty() {
                                                self.completion = None;
                                            }
                                        }
                                    }
                                    // Auto-trigger on trigger chars when popup just closed.
                                    if self.completion.is_none() {
                                        self.maybe_auto_trigger_completion(c);
                                    }
                                    continue;
                                }
                                KeyCode::Backspace if key.modifiers == KeyModifiers::NONE => {
                                    // Let engine handle backspace, then update prefix.
                                    self.active_mut().editor.handle_key(key);
                                    self.sync_viewport_from_editor();
                                    if self.active_mut().editor.take_dirty() {
                                        let elapsed =
                                            self.active_mut().refresh_dirty_against_saved();
                                        self.last_signature_us = elapsed;
                                        if self.active().dirty {
                                            self.active_mut().is_new_file = false;
                                        }
                                    }
                                    let buffer_id = self.active().buffer_id;
                                    if self.active_mut().editor.take_content_reset() {
                                        self.syntax.reset(buffer_id);
                                    }
                                    let edits = self.active_mut().editor.take_content_edits();
                                    if !edits.is_empty() {
                                        self.syntax.apply_edits(buffer_id, &edits);
                                    }
                                    self.lsp_notify_change_active();
                                    self.recompute_and_install();

                                    let anchor_col =
                                        self.completion.as_ref().map(|p| p.anchor_col).unwrap_or(0);
                                    let cur_col = self.active().editor.buffer().cursor().col;
                                    let cur_row = self.active().editor.buffer().cursor().row;
                                    let anchor_row = self
                                        .completion
                                        .as_ref()
                                        .map(|p| p.anchor_row)
                                        .unwrap_or(cur_row);
                                    if cur_row != anchor_row || cur_col < anchor_col {
                                        self.dismiss_completion();
                                    } else {
                                        let new_prefix = {
                                            let line = self
                                                .active()
                                                .editor
                                                .buffer()
                                                .lines()
                                                .get(cur_row)
                                                .cloned()
                                                .unwrap_or_default();
                                            line[anchor_col.min(line.len())
                                                ..cur_col.min(line.len())]
                                                .to_string()
                                        };
                                        if let Some(ref mut popup) = self.completion {
                                            popup.set_prefix(&new_prefix);
                                            if popup.is_empty() {
                                                self.completion = None;
                                            }
                                        }
                                    }
                                    continue;
                                }
                                _ => {
                                    // Any other key dismisses the popup.
                                    self.dismiss_completion();
                                }
                            }
                        } else {
                            // Popup is closed. Handle <C-n>/<C-p> as manual trigger.
                            if key.modifiers.contains(KeyModifiers::CONTROL)
                                && matches!(key.code, KeyCode::Char('n') | KeyCode::Char('p'))
                            {
                                self.lsp_request_completion();
                                continue;
                            }
                            // Auto-trigger on trigger chars.
                            if key.modifiers == KeyModifiers::NONE
                                && let KeyCode::Char(c) = key.code
                            {
                                // Let engine handle it first.
                                self.active_mut().editor.handle_key(key);
                                self.sync_viewport_from_editor();
                                if self.active_mut().editor.take_dirty() {
                                    let elapsed = self.active_mut().refresh_dirty_against_saved();
                                    self.last_signature_us = elapsed;
                                    if self.active().dirty {
                                        self.active_mut().is_new_file = false;
                                    }
                                }
                                let buffer_id = self.active().buffer_id;
                                if self.active_mut().editor.take_content_reset() {
                                    self.syntax.reset(buffer_id);
                                }
                                let edits = self.active_mut().editor.take_content_edits();
                                if !edits.is_empty() {
                                    self.syntax.apply_edits(buffer_id, &edits);
                                }
                                self.lsp_notify_change_active();
                                self.recompute_and_install();
                                self.maybe_auto_trigger_completion(c);
                                continue;
                            }
                        }
                    } else {
                        // Left insert mode — dismiss popup.
                        if self.completion.is_some() {
                            self.dismiss_completion();
                        }
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
                Event::Mouse(me) => {
                    use crossterm::event::MouseEventKind;
                    // Skip while overlays are active — let the
                    // overlay's own key handling (or future mouse
                    // handling) keep ownership of input.
                    if self.command_field.is_some()
                        || self.search_field.is_some()
                        || self.picker.is_some()
                        || self.info_popup.is_some()
                    {
                        continue;
                    }
                    // 3 lines per wheel notch — vim's `mousescroll` default.
                    const WHEEL_LINES: i16 = 3;
                    match me.kind {
                        MouseEventKind::ScrollDown => {
                            self.active_mut().editor.scroll_down(WHEEL_LINES);
                            self.sync_viewport_from_editor();
                            self.recompute_and_install();
                        }
                        MouseEventKind::ScrollUp => {
                            self.active_mut().editor.scroll_up(WHEEL_LINES);
                            self.sync_viewport_from_editor();
                            self.recompute_and_install();
                        }
                        _ => {}
                    }
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
