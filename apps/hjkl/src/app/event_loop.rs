use anyhow::Result;
use crossterm::{
    cursor::SetCursorStyle,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
};
use hjkl_engine::{CursorShape, Host, VimMode};
use hjkl_keymap::{KeyCode as KmKeyCode, KeyEvent as KmKeyEvent, KeyModifiers as KmKeyMods};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::Stdout;
use std::time::Duration;

use super::{App, STATUS_LINE_HEIGHT, SearchDir, prompt_cursor_shape};
use crate::render;

/// Translate a crossterm `KeyEvent` to a `hjkl_keymap::KeyEvent`.
/// Returns `None` for release events or unsupported key codes.
fn to_km_event(key: KeyEvent) -> Option<KmKeyEvent> {
    crate::keymap_translate::from_crossterm(&key)
}

/// Replay a slice of `hjkl_keymap::KeyEvent`s to the engine via crossterm
/// `KeyEvent`s. Each keymap event is converted back to a crossterm event
/// and forwarded to `editor.handle_key`.
fn replay_to_engine(app: &mut App, events: &[KmKeyEvent]) {
    for km_ev in events {
        let ct_ev = km_to_crossterm(km_ev);
        app.active_mut().editor.handle_key(ct_ev);
    }
}

/// Convert a `hjkl_keymap::KeyEvent` back to a `crossterm::event::KeyEvent`
/// for replaying unbound sequences to the engine.
fn km_to_crossterm(ev: &KmKeyEvent) -> KeyEvent {
    let code = match ev.code {
        KmKeyCode::Char(c) => KeyCode::Char(c),
        KmKeyCode::Enter => KeyCode::Enter,
        KmKeyCode::Esc => KeyCode::Esc,
        KmKeyCode::Tab => KeyCode::Tab,
        KmKeyCode::Backspace => KeyCode::Backspace,
        KmKeyCode::Delete => KeyCode::Delete,
        KmKeyCode::Insert => KeyCode::Insert,
        KmKeyCode::Up => KeyCode::Up,
        KmKeyCode::Down => KeyCode::Down,
        KmKeyCode::Left => KeyCode::Left,
        KmKeyCode::Right => KeyCode::Right,
        KmKeyCode::Home => KeyCode::Home,
        KmKeyCode::End => KeyCode::End,
        KmKeyCode::PageUp => KeyCode::PageUp,
        KmKeyCode::PageDown => KeyCode::PageDown,
        KmKeyCode::F(n) => KeyCode::F(n),
    };
    let mut mods = KeyModifiers::NONE;
    if ev.modifiers.contains(KmKeyMods::CTRL) {
        mods |= KeyModifiers::CONTROL;
    }
    if ev.modifiers.contains(KmKeyMods::SHIFT) {
        mods |= KeyModifiers::SHIFT;
    }
    if ev.modifiers.contains(KmKeyMods::ALT) {
        mods |= KeyModifiers::ALT;
    }
    KeyEvent::new(code, mods)
}

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

            // Compute the poll timeout: normally 120 ms (splash animation cadence),
            // but shortened to the remaining which-key deadline when a prefix is pending.
            let poll_timeout = {
                let base = Duration::from_millis(120);
                if self.which_key_enabled && !self.which_key_active {
                    if let Some(prefix_at) = self.pending_prefix_at {
                        let deadline = prefix_at + self.which_key_delay;
                        let now = std::time::Instant::now();
                        let remaining = deadline.saturating_duration_since(now);
                        base.min(remaining)
                    } else {
                        base
                    }
                } else {
                    base
                }
            };

            // Wait for the next event with the computed ceiling.
            if !event::poll(poll_timeout)? {
                // No event arrived. Check if the which-key deadline has now passed.
                if !self.which_key_active && self.active_which_key_prefix().is_some() {
                    let now = std::time::Instant::now();
                    if crate::which_key::should_show(
                        self.pending_prefix_at,
                        self.which_key_delay,
                        self.which_key_enabled,
                        now,
                    ) {
                        self.which_key_active = true;
                        // Fall through to redraw (loop continues).
                    }
                }
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

                    // Any keypress clears the which-key popup immediately. The
                    // prefix resolution branches below call note_prefix_set() again
                    // when chaining into a sub-prefix, which re-arms the timer.
                    self.which_key_active = false;

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

                    if let Some(mapped) = self.apply_runtime_map(key) {
                        if mapped.len() != 1 || mapped[0] != key {
                            for mapped_key in mapped {
                                self.handle_runtime_mapped_key(mapped_key);
                                if self.exit_requested {
                                    break;
                                }
                            }
                            if self.exit_requested {
                                break;
                            }
                            continue;
                        }
                    } else {
                        continue;
                    }

                    // ── Normal-mode app-level chord dispatch ─────────────────
                    if self.active().editor.vim_mode() == VimMode::Normal {
                        // ── Alt-buffer toggle (Ctrl-^ / Ctrl-6) ─────────────
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && (key.code == KeyCode::Char('^') || key.code == KeyCode::Char('6'))
                        {
                            self.buffer_alt();
                            continue;
                        }

                        // ── Shift-H / Shift-L cycle buffers ──────────────────
                        // Only when more than one buffer is open; with a single
                        // slot fall through to the engine's H/L viewport motions.
                        if self.slots.len() > 1
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

                        // ── tmux-navigator: bare Ctrl-h/j/k/l ────────────────
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
                                        let _ = std::process::Command::new("tmux")
                                            .args(["select-pane", flag])
                                            .status();
                                    }
                                }
                                continue;
                            }
                        }

                        // ── App-level count prefix buffering ─────────────────
                        // Buffer digit keys so that count-aware chords (Ngt,
                        // N<C-w>+) can consume the count. When the non-digit key
                        // is not a chord-starter, replay digits to the engine.
                        //
                        // Chord-starters: any key that the app_keymap might
                        // consume as the first key of a chord. We check
                        // has_prefix heuristically by attempting a feed of the
                        // raw key and rewinding if it returns Unbound immediately
                        // without any buffered prefix — but that's complex.
                        // Instead we keep the same explicit list as before:
                        // digits are buffered and replayed if the next key doesn't
                        // match a chord prefix.
                        if key.modifiers == KeyModifiers::NONE {
                            if let KeyCode::Char(d @ '0'..='9') = key.code {
                                let is_zero = d == '0';
                                if !is_zero || !self.pending_count.is_empty() {
                                    self.pending_count.push(d);
                                    continue;
                                }
                                // '0' with empty pending_count → start-of-line; fall through.
                            } else if !self.pending_count.is_empty() {
                                // Non-digit with buffered count.
                                // If it could start a chord, keep count alive.
                                // Otherwise replay digits now.
                                let could_start_chord = match key.code {
                                    KeyCode::Char(c) => {
                                        // Check if this key is a first-key of any Normal binding.
                                        use hjkl_keymap::Mode;
                                        !self.app_keymap.pending(Mode::Normal).is_empty() || {
                                            // Peek: does feeding this key leave Pending?
                                            // We approximate by checking the static set of
                                            // chord-starter chars that are first keys in our bindings.
                                            matches!(c, 'g' | ']' | '[' | 'G')
                                                || c == self.config.editor.leader
                                        }
                                    }
                                    _ => false,
                                };
                                if !could_start_chord {
                                    let digits: String = self.pending_count.drain(..).collect();
                                    for d in digits.chars() {
                                        self.active_mut().editor.handle_key(KeyEvent::new(
                                            KeyCode::Char(d),
                                            KeyModifiers::NONE,
                                        ));
                                    }
                                }
                            }
                        } else {
                            // Modifier key — flush count digits if any.
                            if !self.pending_count.is_empty() {
                                let digits: String = self.pending_count.drain(..).collect();
                                for d in digits.chars() {
                                    self.active_mut().editor.handle_key(KeyEvent::new(
                                        KeyCode::Char(d),
                                        KeyModifiers::NONE,
                                    ));
                                }
                            }
                        }

                        // ── LSP hover (`K`) ───────────────────────────────────
                        if key.code == KeyCode::Char('K')
                            && (key.modifiers == KeyModifiers::NONE
                                || key.modifiers == KeyModifiers::SHIFT)
                        {
                            self.lsp_hover();
                            continue;
                        }

                        // ── Intercept `:` ─────────────────────────────────────
                        if key.code == KeyCode::Char(':') && key.modifiers == KeyModifiers::NONE {
                            self.open_command_prompt();
                            continue;
                        }

                        // ── Intercept `/` and `?` ─────────────────────────────
                        if key.modifiers == KeyModifiers::NONE {
                            if key.code == KeyCode::Char('/') {
                                self.open_search_prompt(SearchDir::Forward);
                                continue;
                            }
                            if key.code == KeyCode::Char('?') {
                                self.open_search_prompt(SearchDir::Backward);
                                continue;
                            }
                        }

                        // ── Escape: cancel any pending prefix ─────────────────
                        if key.code == KeyCode::Esc {
                            self.app_keymap.reset(hjkl_keymap::Mode::Normal);
                            self.pending_count.clear();
                            self.clear_prefix_state();
                            // Fall through to engine so it can exit visual mode etc.
                        }

                        // ── Route through app keymap ───────────────────────────
                        // Translate and feed the key. If Pending/Ambiguous/Match:
                        // consumed. If Unbound: replay buffered count digits +
                        // unbound events to the engine.
                        if let Some(km_ev) = to_km_event(key) {
                            let count = self.pending_count.parse::<u32>().unwrap_or(1).max(1);
                            let mut replay: Vec<KmKeyEvent> = Vec::new();
                            let consumed = self.dispatch_keymap(km_ev, count, &mut replay);
                            if consumed {
                                // Chord is Pending, Ambiguous, or was Matched.
                                // Clear count only on Match (clear_prefix_state is called there).
                                // For Pending/Ambiguous we leave count alive.
                                continue;
                            }
                            // Unbound: flush buffered count digits to engine first.
                            if !self.pending_count.is_empty() {
                                let digits: String = self.pending_count.drain(..).collect();
                                for d in digits.chars() {
                                    self.active_mut().editor.handle_key(KeyEvent::new(
                                        KeyCode::Char(d),
                                        KeyModifiers::NONE,
                                    ));
                                }
                            }
                            // Replay the unbound events to the engine.
                            replay_to_engine(self, &replay);
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
                            continue;
                        }
                        // Key couldn't be translated (unsupported code) — fall through to engine.
                    } else {
                        // Non-Normal mode: reset any pending Normal-mode chord state.
                        self.app_keymap.reset(hjkl_keymap::Mode::Normal);
                        self.pending_count.clear();
                        self.clear_prefix_state();
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

    pub(crate) fn handle_runtime_mapped_key(&mut self, key: KeyEvent) {
        self.status_message = None;

        if self.info_popup.is_some() {
            self.info_popup = None;
            return;
        }

        if self.command_field.is_some() {
            self.handle_command_field_key(key);
            return;
        }

        if self.search_field.is_some() {
            self.handle_search_field_key(key);
            return;
        }

        if self.picker.is_some() {
            self.handle_picker_key(key);
            return;
        }

        if key.code == KeyCode::Char(':')
            && key.modifiers == KeyModifiers::NONE
            && self.active().editor.vim_mode() == VimMode::Normal
        {
            self.open_command_prompt();
            return;
        }

        if key.modifiers == KeyModifiers::NONE && self.active().editor.vim_mode() == VimMode::Normal
        {
            if key.code == KeyCode::Char('/') {
                self.open_search_prompt(SearchDir::Forward);
                return;
            }
            if key.code == KeyCode::Char('?') {
                self.open_search_prompt(SearchDir::Backward);
                return;
            }
        }

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
    }
}
