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

/// Map a [`hjkl_vim::OperatorKind`] (reducer-side) to a
/// [`hjkl_engine::Operator`] (engine-side). All nine reducer-side operators
/// have a corresponding engine variant.
fn op_kind_to_operator(k: hjkl_vim::OperatorKind) -> hjkl_engine::Operator {
    match k {
        hjkl_vim::OperatorKind::Delete => hjkl_engine::Operator::Delete,
        hjkl_vim::OperatorKind::Yank => hjkl_engine::Operator::Yank,
        hjkl_vim::OperatorKind::Change => hjkl_engine::Operator::Change,
        hjkl_vim::OperatorKind::Indent => hjkl_engine::Operator::Indent,
        hjkl_vim::OperatorKind::Outdent => hjkl_engine::Operator::Outdent,
        hjkl_vim::OperatorKind::Uppercase => hjkl_engine::Operator::Uppercase,
        hjkl_vim::OperatorKind::Lowercase => hjkl_engine::Operator::Lowercase,
        hjkl_vim::OperatorKind::ToggleCase => hjkl_engine::Operator::ToggleCase,
        hjkl_vim::OperatorKind::Reflow => hjkl_engine::Operator::Reflow,
    }
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

            // Install any git diff-sign results that arrived from the worker.
            // Redraw request is implicit: if signs changed the next frame picks
            // them up; we don't force a dedicated redraw here to avoid a busy loop.
            let _ = self.poll_git_signs();

            // Poll any in-flight anvil install jobs and surface status toasts.
            let _ = self.poll_anvil_jobs();

            // Compute the poll timeout: normally 120 ms (splash animation cadence),
            // but shortened to the soonest of (a) which-key popup deadline,
            // (b) chord-timeout deadline (Ambiguous → timeout_resolve), whichever
            // applies.
            let poll_timeout = {
                let base = Duration::from_millis(120);
                let now = std::time::Instant::now();
                let mut t = base;
                if let Some(prefix_at) = self.pending_prefix_at {
                    if self.which_key_enabled && !self.which_key_active {
                        let deadline = prefix_at + self.which_key_delay;
                        t = t.min(deadline.saturating_duration_since(now));
                    }
                    if !self
                        .app_keymap
                        .pending(crate::app::keymap::HjklMode::Normal)
                        .is_empty()
                    {
                        let deadline = prefix_at + self.app_keymap.timeout_duration();
                        t = t.min(deadline.saturating_duration_since(now));
                    }
                }
                t
            };

            // Wait for the next event with the computed ceiling.
            if !event::poll(poll_timeout)? {
                let now = std::time::Instant::now();
                // No event arrived. Check if the which-key deadline has now passed.
                if !self.which_key_active
                    && !self.active_which_key_prefix().is_empty()
                    && crate::which_key::should_show(
                        self.pending_prefix_at,
                        self.which_key_delay,
                        self.which_key_enabled,
                        now,
                    )
                {
                    self.which_key_active = true;
                    // Fall through to redraw (loop continues).
                }
                // Check if the chord-timeout deadline has now passed. When the
                // pending chord has both a terminal match and longer extensions
                // (Ambiguous), this fires the shorter binding after `timeoutlen`.
                if let Some(prefix_at) = self.pending_prefix_at
                    && !self
                        .app_keymap
                        .pending(crate::app::keymap::HjklMode::Normal)
                        .is_empty()
                    && now >= prefix_at + self.app_keymap.timeout_duration()
                    && let Some(replay) =
                        self.resolve_chord_timeout(crate::app::keymap::HjklMode::Normal)
                {
                    self.which_key_active = false;
                    if !replay.is_empty() {
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

                    // ── Non-Normal-mode trie dispatch (user runtime maps) ────
                    // For Insert/Visual/OpPending/CommandLine, try the trie
                    // first so user `:imap` / `:vmap` etc. bindings fire.
                    // Built-ins are registered Normal-only so they never match
                    // here. If Unbound, fall through to the mode-specific
                    // handling below (Insert completion, engine, etc.).
                    if self.active().editor.vim_mode() != VimMode::Normal
                        && let Some(km_ev) = to_km_event(key)
                        && let Some(km_mode) = super::current_km_mode(self)
                    {
                        let mut replay: Vec<KmKeyEvent> = Vec::new();
                        let consumed = self.dispatch_keymap_in_mode(km_ev, 1, &mut replay, km_mode);
                        if consumed {
                            if self.exit_requested {
                                break;
                            }
                            continue;
                        }
                        // Unbound — fall through to existing mode handling.
                        // (Single-key unbound is fine; multi-key chord tail
                        // is silently consumed per normal policy.)
                    }

                    // ── Visual-mode `:` → command prompt prefilled with '<,'> ─
                    if key.code == KeyCode::Char(':')
                        && key.modifiers == KeyModifiers::NONE
                        && matches!(
                            self.active().editor.vim_mode(),
                            VimMode::Visual | VimMode::VisualLine | VimMode::VisualBlock
                        )
                    {
                        // Exit visual mode by feeding Esc to the engine. The
                        // visual-exit hook in hjkl-engine sets the `<` / `>`
                        // marks so :'<,'> resolves.
                        self.active_mut()
                            .editor
                            .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                        self.open_command_prompt_with("'<,'>");
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
                                        use crate::app::keymap::HjklMode as Mode;
                                        !self.app_keymap.pending(Mode::Normal).is_empty() || {
                                            // Peek: does feeding this key leave Pending?
                                            // We approximate by checking the static set of
                                            // chord-starter chars that are first keys in our bindings.
                                            // Phase 3a: h/j/k/l/+/-/<Space> are now keymap-bound
                                            // motions; keep count alive so 5j/3k etc. work.
                                            // Phase 3c: ^/$  added (line-anchor motions).
                                            // Phase 3g: H/M/L added (viewport motions).
                                            // Ctrl-prefixed keys (C-d/u/f/b) are not Char events
                                            // so they never reach this arm; the non-Char branch
                                            // below handles them (they're already forwarded as
                                            // ctrl+Char events that bypass digit-prefix replay).
                                            matches!(
                                                c,
                                                'g' | 'z'
                                                    | ']'
                                                    | '['
                                                    | 'G'
                                                    | 'H'
                                                    | 'M'
                                                    | 'L'
                                                    | 'd'
                                                    | 'y'
                                                    | 'c'
                                                    | 'h'
                                                    | 'j'
                                                    | 'k'
                                                    | 'l'
                                                    | '+'
                                                    | '-'
                                                    | ' '
                                                    | 'w'
                                                    | 'W'
                                                    | 'b'
                                                    | 'B'
                                                    | 'e'
                                                    | 'E'
                                                    | '^'
                                                    | '$'
                                                    | ';'
                                                    | ','
                                                    | '%'
                                            ) || c == self.config.editor.leader
                                        }
                                    }
                                    // Phase 3a: <BS> is now a keymap-bound motion (CharLeft);
                                    // keep count alive so count+<BS> reaches dispatch_action.
                                    // Phase 3c: <Home>/<End> are keymap-bound line-anchor motions.
                                    KeyCode::Backspace | KeyCode::Home | KeyCode::End => true,
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
                            // Modifier key. For Ctrl+Char keys the keymap may
                            // match (e.g. <C-d>/<C-u>/<C-f>/<C-b> from Phase 3g)
                            // and should receive the buffered count. Keep
                            // pending_count alive so dispatch_keymap sees it.
                            // If the keymap misses, the "Unbound" path below
                            // drains the digits to the engine before replaying.
                            // For non-Ctrl modifier keys (Alt, etc.) flush now as
                            // before — they are not keymap-bound count consumers.
                            let is_ctrl_char = key.modifiers == KeyModifiers::CONTROL
                                && matches!(key.code, KeyCode::Char(_));
                            if !is_ctrl_char && !self.pending_count.is_empty() {
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
                            self.app_keymap.reset(crate::app::keymap::HjklMode::Normal);
                            self.pending_count.clear();
                            self.clear_prefix_state();
                            self.which_key_sticky = false;
                            // Fall through to engine so it can exit visual mode etc.
                        }

                        // ── which-key Backspace (chord navigate-up) ─────────────────
                        if key.code == KeyCode::Backspace
                            && key.modifiers == KeyModifiers::NONE
                            && self.active().editor.vim_mode() == VimMode::Normal
                        {
                            let pending_non_empty = !self
                                .app_keymap
                                .pending(crate::app::keymap::HjklMode::Normal)
                                .is_empty();
                            if pending_non_empty {
                                self.app_keymap.pop(crate::app::keymap::HjklMode::Normal);
                                // If pop emptied the buffer, enter sticky so the popup
                                // stays showing root entries until the user types something.
                                if self
                                    .app_keymap
                                    .pending(crate::app::keymap::HjklMode::Normal)
                                    .is_empty()
                                {
                                    self.which_key_sticky = true;
                                }
                                // Re-arm the which-key timer and force-show the popup.
                                self.note_prefix_set();
                                self.which_key_active = true;
                                continue;
                            }
                            if self.which_key_sticky {
                                // At root in sticky mode — noop per spec.
                                continue;
                            }
                            // No chord, no sticky → fall through to engine
                            // (backspace = move left in vim Normal mode).
                        } else {
                            // Any non-Backspace key clears sticky which-key.
                            self.which_key_sticky = false;
                        }

                        // ── hjkl-vim pending-state reducer ────────────────────
                        // App-level pending chord (r<x>, …) is driven by the
                        // hjkl-vim reducer. When `pending_state` is `Some`, feed
                        // the next key there BEFORE the keymap trie or engine.
                        if let Some(state) = self.pending_state {
                            use hjkl_vim::{Key as VimKey, Outcome};
                            let vim_key = match key.code {
                                KeyCode::Char(c) => Some(VimKey::Char(c)),
                                KeyCode::Esc => Some(VimKey::Esc),
                                KeyCode::Enter => Some(VimKey::Enter),
                                KeyCode::Backspace => Some(VimKey::Backspace),
                                KeyCode::Tab => Some(VimKey::Tab),
                                _ => None,
                            };
                            match vim_key {
                                None => {
                                    // Unrecognised key — forward without consuming state.
                                    // (Outcome::Forward path)
                                }
                                Some(vk) => {
                                    match hjkl_vim::step(state, vk) {
                                        Outcome::Wait(new_state) => {
                                            self.pending_state = Some(new_state);
                                            continue;
                                        }
                                        Outcome::Commit(hjkl_vim::EngineCmd::ReplaceChar {
                                            ch,
                                            count,
                                        }) => {
                                            self.pending_state = None;
                                            self.active_mut().editor.replace_char_at(ch, count);
                                            self.sync_viewport_from_editor();
                                            if self.active_mut().editor.take_dirty() {
                                                let elapsed =
                                                    self.active_mut().refresh_dirty_against_saved();
                                                self.last_signature_us = elapsed;
                                                if self.active().dirty {
                                                    self.active_mut().is_new_file = false;
                                                }
                                            }
                                            continue;
                                        }
                                        Outcome::Commit(hjkl_vim::EngineCmd::FindChar {
                                            ch,
                                            forward,
                                            till,
                                            count,
                                        }) => {
                                            self.pending_state = None;
                                            self.active_mut()
                                                .editor
                                                .find_char(ch, forward, till, count);
                                            self.sync_viewport_from_editor();
                                            continue;
                                        }
                                        Outcome::Commit(hjkl_vim::EngineCmd::AfterGChord {
                                            ch,
                                            count,
                                        }) => {
                                            self.pending_state = None;
                                            // App-level g-prefix actions (formerly
                                            // trie-bound as gt/gd/etc.) are dispatched
                                            // here before falling through to the engine.
                                            match ch {
                                                't' => {
                                                    self.dispatch_action(
                                                        crate::keymap_actions::AppAction::Tabnext,
                                                        count as u32,
                                                    );
                                                    continue;
                                                }
                                                'T' => {
                                                    self.dispatch_action(
                                                        crate::keymap_actions::AppAction::Tabprev,
                                                        count as u32,
                                                    );
                                                    continue;
                                                }
                                                'd' => {
                                                    self.dispatch_action(
                                                        crate::keymap_actions::AppAction::LspGotoDef,
                                                        count as u32,
                                                    );
                                                    continue;
                                                }
                                                'D' => {
                                                    self.dispatch_action(
                                                        crate::keymap_actions::AppAction::LspGotoDecl,
                                                        count as u32,
                                                    );
                                                    continue;
                                                }
                                                'r' => {
                                                    self.dispatch_action(
                                                        crate::keymap_actions::AppAction::LspGotoRef,
                                                        count as u32,
                                                    );
                                                    continue;
                                                }
                                                'i' => {
                                                    self.dispatch_action(
                                                        crate::keymap_actions::AppAction::LspGotoImpl,
                                                        count as u32,
                                                    );
                                                    continue;
                                                }
                                                'y' => {
                                                    self.dispatch_action(
                                                        crate::keymap_actions::AppAction::LspGotoTypeDef,
                                                        count as u32,
                                                    );
                                                    continue;
                                                }
                                                _ => {}
                                            }
                                            // Chord-init case-ops: intercept u/U/~/q and
                                            // set reducer AfterOp instead of calling
                                            // after_g (which would set engine Pending::Op).
                                            // This keeps the full gU/gu/g~/gq op-pending
                                            // path inside the reducer from here on.
                                            let case_op_kind = match ch {
                                                'u' => Some(hjkl_vim::OperatorKind::Lowercase),
                                                'U' => Some(hjkl_vim::OperatorKind::Uppercase),
                                                '~' => Some(hjkl_vim::OperatorKind::ToggleCase),
                                                'q' => Some(hjkl_vim::OperatorKind::Reflow),
                                                _ => None,
                                            };
                                            if let Some(op) = case_op_kind {
                                                self.pending_state =
                                                    Some(hjkl_vim::PendingState::AfterOp {
                                                        op,
                                                        count1: count,
                                                        inner_count: 0,
                                                    });
                                                continue;
                                            }
                                            // All other g-chords: delegate to engine.
                                            self.active_mut().editor.after_g(ch, count);
                                            self.sync_viewport_from_editor();
                                            if self.active_mut().editor.take_dirty() {
                                                let elapsed =
                                                    self.active_mut().refresh_dirty_against_saved();
                                                self.last_signature_us = elapsed;
                                                if self.active().dirty {
                                                    self.active_mut().is_new_file = false;
                                                }
                                            }
                                            continue;
                                        }
                                        Outcome::Commit(hjkl_vim::EngineCmd::AfterZChord {
                                            ch,
                                            count,
                                        }) => {
                                            self.pending_state = None;
                                            // All z-chords delegate directly to the engine.
                                            // after_z reads ed.vim.mode internally so the
                                            // visual-selection zf path works without extra
                                            // host logic.
                                            self.active_mut().editor.after_z(ch, count);
                                            self.sync_viewport_from_editor();
                                            // after_z may set Pending::Op (zf in Normal);
                                            // is_chord_pending() bypass on the NEXT key
                                            // ensures the engine's op-pending arm fires.
                                            if self.active_mut().editor.take_dirty() {
                                                let elapsed =
                                                    self.active_mut().refresh_dirty_against_saved();
                                                self.last_signature_us = elapsed;
                                                if self.active().dirty {
                                                    self.active_mut().is_new_file = false;
                                                }
                                            }
                                            continue;
                                        }
                                        Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpMotion {
                                            op,
                                            motion_key,
                                            total_count,
                                        }) => {
                                            self.pending_state = None;
                                            self.active_mut().editor.apply_op_motion(
                                                op_kind_to_operator(op),
                                                motion_key,
                                                total_count,
                                            );
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
                                            let edits =
                                                self.active_mut().editor.take_content_edits();
                                            if !edits.is_empty() {
                                                self.syntax.apply_edits(buffer_id, &edits);
                                            }
                                            self.lsp_notify_change_active();
                                            self.recompute_and_install();
                                            continue;
                                        }
                                        Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpDouble {
                                            op,
                                            total_count,
                                        }) => {
                                            self.pending_state = None;
                                            self.active_mut().editor.apply_op_double(
                                                op_kind_to_operator(op),
                                                total_count,
                                            );
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
                                            let edits =
                                                self.active_mut().editor.take_content_edits();
                                            if !edits.is_empty() {
                                                self.syntax.apply_edits(buffer_id, &edits);
                                            }
                                            self.lsp_notify_change_active();
                                            self.recompute_and_install();
                                            continue;
                                        }
                                        Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpTextObj {
                                            op,
                                            ch,
                                            inner,
                                            total_count,
                                        }) => {
                                            self.pending_state = None;
                                            self.active_mut().editor.apply_op_text_obj(
                                                op_kind_to_operator(op),
                                                ch,
                                                inner,
                                                total_count,
                                            );
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
                                            let edits =
                                                self.active_mut().editor.take_content_edits();
                                            if !edits.is_empty() {
                                                self.syntax.apply_edits(buffer_id, &edits);
                                            }
                                            self.lsp_notify_change_active();
                                            self.recompute_and_install();
                                            continue;
                                        }
                                        Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpG {
                                            op,
                                            ch,
                                            total_count,
                                        }) => {
                                            self.pending_state = None;
                                            self.active_mut().editor.apply_op_g(
                                                op_kind_to_operator(op),
                                                ch,
                                                total_count,
                                            );
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
                                            let edits =
                                                self.active_mut().editor.take_content_edits();
                                            if !edits.is_empty() {
                                                self.syntax.apply_edits(buffer_id, &edits);
                                            }
                                            self.lsp_notify_change_active();
                                            self.recompute_and_install();
                                            continue;
                                        }
                                        Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpFind {
                                            op,
                                            ch,
                                            forward,
                                            till,
                                            total_count,
                                        }) => {
                                            self.pending_state = None;
                                            self.active_mut().editor.apply_op_find(
                                                op_kind_to_operator(op),
                                                ch,
                                                forward,
                                                till,
                                                total_count,
                                            );
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
                                            let edits =
                                                self.active_mut().editor.take_content_edits();
                                            if !edits.is_empty() {
                                                self.syntax.apply_edits(buffer_id, &edits);
                                            }
                                            self.lsp_notify_change_active();
                                            self.recompute_and_install();
                                            continue;
                                        }
                                        Outcome::Commit(
                                            hjkl_vim::EngineCmd::SetPendingRegister { reg },
                                        ) => {
                                            self.pending_state = None;
                                            self.active_mut().editor.set_pending_register(reg);
                                            continue;
                                        }
                                        Outcome::Cancel => {
                                            self.pending_state = None;
                                            continue;
                                        }
                                        Outcome::Forward => {
                                            // State stays alive; fall through to normal routing.
                                        }
                                    }
                                }
                            }
                        }

                        // ── Route through app keymap ───────────────────────────
                        // Engine has an in-flight pending chord (f<x>,
                        // m<a>, op-pending, g-pending, register-select, macro-
                        // record, etc.) — bypass the keymap trie so the engine
                        // can complete its command without us eating its
                        // continuation key. (Fixes gg/gj/f<space>/etc.)
                        let engine_pending = self.active().editor.is_chord_pending();

                        // Translate and feed the key. If Pending/Ambiguous/Match:
                        // consumed. If Unbound: replay buffered count digits +
                        // unbound events to the engine.
                        if !engine_pending && let Some(km_ev) = to_km_event(key) {
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
                            // Multi-key replay = trie consumed a chord prefix then
                            // hit an unmapped tail (e.g. `gg` with g-prefix bindings,
                            // `<leader>x`). Always forward so `gg`/`gj`/etc reach the
                            // engine. Side effect: unmapped chord tails like <leader>x
                            // leak to the engine (space=move-right, x=delete-char) —
                            // vim-compatible; users can `:nmap <leader> <Nop>` to stop.
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
                        self.app_keymap.reset(crate::app::keymap::HjklMode::Normal);
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
}
