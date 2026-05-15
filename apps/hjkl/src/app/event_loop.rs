use anyhow::Result;
use crossterm::{
    cursor::SetCursorStyle,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
};
use hjkl_engine::{CursorShape, Host, VimMode};
use hjkl_keymap::{Chord as KmChord, KeyEvent as KmKeyEvent};
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
/// `KeyEvent`s. Thin wrapper delegating to `App::replay_to_engine`; kept
/// for callers inside this file that pass `app` as a plain `&mut App` arg.
fn replay_to_engine(app: &mut App, events: &[KmKeyEvent]) {
    app.replay_to_engine(events);
}

/// Map a [`hjkl_vim::OperatorKind`] (reducer-side) to a
/// [`hjkl_engine::Operator`] (engine-side). All nine reducer-side operators
/// have a corresponding engine variant.
pub(crate) fn op_kind_to_operator(k: hjkl_vim::OperatorKind) -> hjkl_engine::Operator {
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
    /// Insert-mode key dispatcher. Calls `Editor::insert_*` primitives
    /// directly, bypassing the engine FSM for Insert-mode keys.
    ///
    /// This is called from the main event loop whenever the editor is in
    /// `VimMode::Insert` and the key has not been consumed by an overlay
    /// (completion popup, etc.). Normal / Visual modes still route through
    /// `hjkl_vim::handle_key`.
    ///
    /// ### `Ctrl-R {reg}` — register paste
    /// `insert_ctrl_r_arm()` sets an internal flag (`insert_pending_register`).
    /// The NEXT printable character names the register; we detect this via
    /// `editor.is_insert_register_pending()` and call
    /// `insert_paste_register(c)` instead of `insert_char(c)`.
    ///
    /// ### `Ctrl-O` — one-shot normal
    /// `insert_ctrl_o_arm()` flips `vim.mode` to Normal (and syncs
    /// `current_mode`). The NEXT key therefore reads `vim_mode() == Normal`
    /// and is dispatched as a Normal-mode key naturally — no extra flag needed
    /// here. After that single normal command the engine's end-of-step hook
    /// flips back to Insert.
    pub(crate) fn dispatch_insert_key(&mut self, key: KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        use hjkl_engine::InsertDir;

        // `Ctrl-R` two-key sequence: the previous key armed the register
        // selector. The next printable char names the register to paste.
        // Any non-printable key cancels (mirrors vim behaviour).
        if self.active().editor.is_insert_register_pending() {
            // Clear the flag first (mirrors step_insert which clears before
            // calling insert_paste_register_bridge).
            self.active_mut().editor.clear_insert_register_pending();
            if let (KeyCode::Char(c), mods) = (key.code, key.modifiers)
                && !mods.contains(KeyModifiers::CONTROL)
            {
                self.active_mut().editor.insert_paste_register(c);
            }
            // Non-char key: flag already cleared; just drop the key.
            return;
        }

        match (key.code, key.modifiers) {
            // Printable characters (including shifted variants like 'A', '!', …).
            // Crossterm sets SHIFT for capital letters but the char `c` already
            // contains the upper-cased glyph, so we just forward `c` directly.
            (KeyCode::Char(c), mods)
                if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT =>
            {
                self.active_mut().editor.insert_char(c);
            }

            // Navigation / editing keys
            (KeyCode::Backspace, _) => self.active_mut().editor.insert_backspace(),
            (KeyCode::Enter, _) => self.active_mut().editor.insert_newline(),
            (KeyCode::Tab, _) => self.active_mut().editor.insert_tab(),
            (KeyCode::Esc, _) => self.active_mut().editor.leave_insert_to_normal(),
            (KeyCode::Delete, _) => self.active_mut().editor.insert_delete(),
            (KeyCode::Home, _) => self.active_mut().editor.insert_home(),
            (KeyCode::End, _) => self.active_mut().editor.insert_end(),

            // Arrow keys
            (KeyCode::Left, _) => self.active_mut().editor.insert_arrow(InsertDir::Left),
            (KeyCode::Right, _) => self.active_mut().editor.insert_arrow(InsertDir::Right),
            (KeyCode::Up, _) => self.active_mut().editor.insert_arrow(InsertDir::Up),
            (KeyCode::Down, _) => self.active_mut().editor.insert_arrow(InsertDir::Down),

            // Page keys — need the current viewport height.
            (KeyCode::PageUp, _) => {
                let h = self.active().editor.viewport_height_value();
                self.active_mut().editor.insert_pageup(h);
            }
            (KeyCode::PageDown, _) => {
                let h = self.active().editor.viewport_height_value();
                self.active_mut().editor.insert_pagedown(h);
            }

            // Ctrl-prefixed insert shortcuts
            (KeyCode::Char('w'), KeyModifiers::CONTROL) => self.active_mut().editor.insert_ctrl_w(),
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => self.active_mut().editor.insert_ctrl_u(),
            (KeyCode::Char('h'), KeyModifiers::CONTROL) => self.active_mut().editor.insert_ctrl_h(),
            // `Ctrl-O`: flip to one-shot Normal; the next key routes as Normal.
            (KeyCode::Char('o'), KeyModifiers::CONTROL) => {
                self.active_mut().editor.insert_ctrl_o_arm()
            }
            // `Ctrl-R`: arm register selector; next char calls insert_paste_register.
            (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                self.active_mut().editor.insert_ctrl_r_arm()
            }
            (KeyCode::Char('t'), KeyModifiers::CONTROL) => self.active_mut().editor.insert_ctrl_t(),
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => self.active_mut().editor.insert_ctrl_d(),

            // Silently drop unrecognised keys (function keys, Alt combos, etc.).
            _ => {}
        }
    }

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
                        self.sync_after_engine_mutation();
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

                    // ── Visual-mode `:` → command prompt prefilled with '<,'> ─
                    // Must run BEFORE route_chord_key so a pending_state from a
                    // prior chord (e.g. first `g` in Visual mode) does not eat
                    // the `:` key. Visual `:` is not a chord continuation.
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
                        hjkl_vim::handle_key(
                            &mut self.active_mut().editor,
                            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                        );
                        self.open_command_prompt_with("'<,'>");
                        continue;
                    }

                    // ── Normal-mode app-level pre-routing ────────────────────
                    // These run BEFORE route_chord_key below. They may `continue`
                    // (consuming the key) or fall through to route_chord_key.
                    // Out of scope for route_chord_key: Ctrl-^, H/L buffer cycle,
                    // tmux-nav, count-prefix, LSP K, `:`, `/`, Esc, which-key BS.
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
                        //
                        // Skip count-prefix buffering entirely when a pending_state
                        // chord is active (e.g. SelectRegister after `"a`). In that
                        // case the next key is consumed by the reducer (not the count
                        // accumulator), and flushing digits to the engine would corrupt
                        // the engine's internal count state. route_chord_key below owns
                        // the key in that situation.
                        if self.pending_state.is_none() && key.modifiers == KeyModifiers::NONE {
                            if let KeyCode::Char(d @ '0'..='9') = key.code {
                                // try_accumulate returns false for '0' with empty buffer
                                // (vim's LineStart quirk); in that case fall through to keymap.
                                if self.pending_count.try_accumulate(d) {
                                    continue;
                                }
                                // '0' with empty pending_count → start-of-line; fall through.
                            } else if !self.pending_count.is_empty() {
                                // Non-digit with buffered count.
                                // If it could start a chord, keep count alive.
                                // Otherwise replay digits now.
                                //
                                // Query the trie rather than a static char-set:
                                // ask whether this key is a root-level key in
                                // the Normal-mode bindings (i.e. a valid first
                                // key of any chord).  `children_all` with an
                                // empty prefix returns all root entries without
                                // mutating the pending-chord state.
                                use crate::app::keymap::HjklMode as Mode;
                                let could_start_chord =
                                    !self.app_keymap.pending(Mode::Normal).is_empty()
                                        || to_km_event(key).is_some_and(|km_ev| {
                                            let root = self.app_keymap.children_all(
                                                Mode::Normal,
                                                &KmChord::from_events(vec![]),
                                            );
                                            root.iter().any(|(k, _)| *k == km_ev)
                                        });
                                if !could_start_chord {
                                    self.flush_pending_count_to_engine();
                                }
                            }
                        } else if self.pending_state.is_none() {
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
                                self.flush_pending_count_to_engine();
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
                        // Skip when a pending_state chord is waiting for its
                        // second key — e.g. `@:` (PlayMacroTarget expects ':').
                        if key.code == KeyCode::Char(':')
                            && key.modifiers == KeyModifiers::NONE
                            && self.pending_state.is_none()
                        {
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
                            self.pending_count.reset();
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

                        // Fall through to route_chord_key below.
                    } else {
                        // Non-Normal mode: reset any pending Normal-mode chord state.
                        self.app_keymap.reset(crate::app::keymap::HjklMode::Normal);
                        self.pending_count.reset();
                        self.clear_prefix_state();
                    }

                    // ── Canonical chord routing ───────────────────────────────
                    // Handles:
                    //   (1) pending_state reducer (all modes, pending_state.is_some())
                    //   (2) Non-Normal trie dispatch (mode != Normal, pending_state.is_none())
                    //   (3) Normal-mode keymap dispatch (mode == Normal, pending_state.is_none())
                    // count-prefix, engine-pending bypass, and replay logic are encapsulated
                    // inside route_chord_key for step (3).
                    if self.route_chord_key(key) {
                        if self.exit_requested {
                            break;
                        }
                        continue;
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
                                    self.sync_after_engine_mutation();
                                    continue;
                                }
                                KeyCode::Char('y')
                                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    self.accept_completion();
                                    self.sync_after_engine_mutation();
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
                                    // Phase 6.5: call insert primitive directly.
                                    self.active_mut().editor.insert_char(c);
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
                                    // Phase 6.5: call insert primitive directly.
                                    self.active_mut().editor.insert_backspace();
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
                                // Phase 6.5: call insert primitive directly.
                                self.active_mut().editor.insert_char(c);
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
                    // Insert mode uses the inline dispatcher which calls
                    // Editor::insert_* primitives directly. Normal / Visual
                    // modes route through the FSM via hjkl_vim::handle_key.
                    if self.active().editor.vim_mode() == VimMode::Insert {
                        self.dispatch_insert_key(key);
                        // dispatch_insert_key calls Editor primitives directly and
                        // does not go through hjkl_vim::handle_key, which is the
                        // normal site for emit_cursor_shape_if_changed. Emit here
                        // so Esc (→ Normal/Block) and Ctrl-O surface immediately.
                        self.active_mut().editor.emit_cursor_shape_if_changed();
                    } else {
                        hjkl_vim::handle_key(&mut self.active_mut().editor, key);
                    }

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
                    use crossterm::event::{MouseButton, MouseEventKind};
                    // Skip while overlays are active — Phase 8 will handle
                    // mouse in overlays.
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
                        MouseEventKind::Down(MouseButton::Left) => {
                            use crate::app::mouse;
                            if let Some(win_id) = mouse::hit_test_window(self, me.column, me.row) {
                                // Focus the clicked window if it differs.
                                let current_focus = self.focused_window();
                                if win_id != current_focus {
                                    self.sync_viewport_from_editor();
                                    self.set_focused_window(win_id);
                                    self.sync_viewport_to_editor();
                                }
                                if let Some((doc_row, doc_col)) =
                                    mouse::cell_to_doc(self, win_id, me.column, me.row)
                                {
                                    let count =
                                        self.mouse_click_tracker.register(win_id, doc_row, doc_col);
                                    match count {
                                        1 => {
                                            self.active_mut()
                                                .editor
                                                .mouse_click_doc(doc_row, doc_col);
                                        }
                                        2 => {
                                            // Double-click: select word.
                                            self.active_mut()
                                                .editor
                                                .mouse_click_doc(doc_row, doc_col);
                                            let line = self
                                                .active()
                                                .editor
                                                .buffer()
                                                .line(doc_row)
                                                .unwrap_or("")
                                                .to_owned();
                                            let (ws, we) = mouse::word_bounds(&line, doc_col);
                                            // Anchor at word start, cursor at word end - 1.
                                            self.active_mut().editor.enter_visual_char();
                                            self.active_mut().editor.set_cursor_doc(doc_row, ws);
                                            self.active_mut().editor.mouse_begin_drag();
                                            self.active_mut().editor.set_cursor_doc(
                                                doc_row,
                                                we.saturating_sub(1).max(ws),
                                            );
                                        }
                                        _ => {
                                            // Triple-click (and count≥4 wraps to 1 in tracker,
                                            // so this branch only fires at count==3).
                                            self.active_mut()
                                                .editor
                                                .mouse_click_doc(doc_row, doc_col);
                                            self.active_mut().editor.enter_visual_line();
                                        }
                                    }
                                    self.sync_after_engine_mutation();
                                }
                            }
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            use crate::app::mouse;
                            let win_id = self.focused_window();
                            if let Some((doc_row, doc_col)) =
                                mouse::cell_to_doc(self, win_id, me.column, me.row)
                            {
                                // Begin drag on first drag event if not already in
                                // visual mode.
                                if self.active().editor.vim_mode() != VimMode::Visual {
                                    self.active_mut().editor.mouse_begin_drag();
                                }
                                self.active_mut()
                                    .editor
                                    .mouse_extend_drag_doc(doc_row, doc_col);
                                self.sync_after_engine_mutation();
                            }
                        }
                        // Up: vim stays in Visual after drag-release — no-op.
                        MouseEventKind::Up(MouseButton::Left) => {}
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
