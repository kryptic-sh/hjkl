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

use super::{App, STATUS_LINE_HEIGHT, prompt_cursor_shape};
use crate::render;

/// How long the mouse must rest on a Code zone before the LSP hover RPC fires.
const HOVER_DELAY: Duration = Duration::from_millis(500);

/// Outcome returned by [`App::handle_keypress`].
pub(crate) enum KeyOutcome {
    /// Continue the event loop (equivalent to `continue`).
    Continue,
    /// Break out of the event loop (equivalent to `break`).
    Break,
    /// Key was not consumed by any overlay/prefix handler; fall through to
    /// the engine (Insert or Normal/Visual via hjkl_vim_tui::handle_key).
    FallThrough,
}

/// Outcome returned by [`App::handle_mouse`].
pub(crate) enum MouseOutcome {
    /// Continue the event loop.
    Continue,
    /// Fall through (no explicit `continue` needed but loop iterates).
    FallThrough,
}

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
        hjkl_vim::OperatorKind::AutoIndent => hjkl_engine::Operator::AutoIndent,
        hjkl_vim::OperatorKind::Filter => hjkl_engine::Operator::Filter,
        hjkl_vim::OperatorKind::Comment => hjkl_engine::Operator::Comment,
    }
}

impl App {
    /// Insert-mode key dispatcher. Calls `Editor::insert_*` primitives
    /// directly, bypassing the engine FSM for Insert-mode keys.
    ///
    /// This is called from the main event loop whenever the editor is in
    /// `VimMode::Insert` and the key has not been consumed by an overlay
    /// (completion popup, etc.). Normal / Visual modes still route through
    /// `hjkl_vim_tui::handle_key`.
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

        // Macro recording for keys that reach this dispatcher happens upstream
        // in `handle_keypress`'s Insert-mode block (the single hook there
        // covers consume-and-return Continue paths AND fall-through paths so
        // we don't double-record). Don't add a hook here.

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

    /// Poll in-flight grammar loads, git signs, format results, and anvil jobs.
    /// Called once per event loop tick before the poll wait.
    pub(crate) fn drain_async_polls(&mut self) {
        // Poll any in-flight async grammar loads each tick so a freshly
        // compiled grammar installs without needing a keypress.
        if self.poll_grammar_loads() {
            self.recompute_and_install();
        }

        // Install any git diff-sign results that arrived from the worker.
        // Redraw request is implicit: if signs changed the next frame picks
        // them up; we don't force a dedicated redraw here to avoid a busy loop.
        let _ = self.poll_git_signs();

        // Install any completed async format results (#118).
        let _ = self.poll_format_results();

        // Poll any in-flight anvil install jobs and surface status toasts.
        let _ = self.poll_anvil_jobs();
    }

    /// Compute how long to wait for the next event.
    ///
    /// Normally 120 ms (splash animation cadence), but shortened to the soonest
    /// of (a) which-key popup deadline, (b) chord-timeout deadline (Ambiguous →
    /// timeout_resolve), (c) active indent-flash so each 75 ms phase paints.
    pub(crate) fn compute_poll_timeout(&self) -> Duration {
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
        if self.indent_flash.is_some() {
            t = t.min(Duration::from_millis(30));
        }
        t
    }

    /// Handle a single key event. Returns a [`KeyOutcome`] that tells `run()`
    /// whether to `continue`, `break`, or fall through to the engine dispatch.
    ///
    /// All overlay handling, Normal-mode pre-routing (count prefix, Esc,
    /// which-key Backspace), and keymap chord routing live here.
    pub(crate) fn handle_keypress(&mut self, key: KeyEvent) -> KeyOutcome {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.command_field.is_some() {
                self.command_field = None;
                return KeyOutcome::Continue;
            }
            if self.search_field.is_some() {
                self.cancel_search_prompt();
                return KeyOutcome::Continue;
            }
            // <C-c> in the cmdline window closes it.
            if self.is_cmdline_win_focused() {
                self.close_cmdline_window();
                return KeyOutcome::Continue;
            }
            return KeyOutcome::Break;
        }

        // ── Cmdline window <CR> intercept (issue #37) ─────────────
        // Must run BEFORE normal-mode routing so `<Enter>` in the
        // cmdline window commits the line rather than opening a new line
        // below (o) or doing nothing in Normal mode.
        if self.is_cmdline_win_focused()
            && key.code == KeyCode::Enter
            && key.modifiers == KeyModifiers::NONE
        {
            self.commit_cmdline_window();
            if self.exit_requested {
                return KeyOutcome::Break;
            }
            return KeyOutcome::Continue;
        }

        // Dismiss the start screen on any non-Ctrl-C keypress and
        // let the key fall through to normal handling so `:`,
        // `/`, `i`, etc. take effect on the same press.
        if self.start_screen.is_some() {
            self.start_screen = None;
        }

        // Any keypress clears the which-key popup immediately. The
        // prefix resolution branches below call note_prefix_set() again
        // when chaining into a sub-prefix, which re-arms the timer.
        self.which_key_active = false;

        // ── Info popup dismissal ──────────────────────────────────
        if self.info_popup.is_some() {
            self.info_popup = None;
            return KeyOutcome::Continue;
        }

        // ── Hover popup dismissal (Phase 5 mouse support) ─────────
        if self.hover_popup.is_some() {
            self.hover_popup = None;
            self.hover_timer = None;
            // fall through — key still takes effect
        }

        // ── Context menu keyboard navigation (Phase 2, Round A) ───
        if self.context_menu.is_some() {
            let consumed = self.handle_context_menu_key(key);
            if consumed {
                return KeyOutcome::Continue;
            }
            // Any non-nav key dismisses the menu and falls through.
            self.context_menu = None;
        }

        // ── Command palette (`:` prompt) ─────────────────────────
        if self.command_field.is_some() {
            self.handle_command_field_key(key);
            if self.exit_requested {
                return KeyOutcome::Break;
            }
            return KeyOutcome::Continue;
        }

        // ── Filter prompt (`!` operator) ──────────────────────────
        if self.filter_field.is_some() {
            self.handle_filter_field_key(key);
            return KeyOutcome::Continue;
        }

        // ── Search prompt (`/` `?`) ──────────────────────────────
        if self.search_field.is_some() {
            self.handle_search_field_key(key);
            if self.exit_requested {
                return KeyOutcome::Break;
            }
            return KeyOutcome::Continue;
        }

        // ── Picker overlay ────────────────────────────────────────
        if self.picker.is_some() {
            self.handle_picker_key(key);
            if self.exit_requested {
                return KeyOutcome::Break;
            }
            return KeyOutcome::Continue;
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
            hjkl_vim_tui::handle_key(
                &mut self.active_mut().editor,
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            );
            self.open_command_prompt_with("'<,'>");
            return KeyOutcome::Continue;
        }

        // ── Normal-mode app-level pre-routing ────────────────────
        // These run BEFORE route_chord_key below. They may return Continue
        // (consuming the key) or fall through to route_chord_key.
        // Out of scope for route_chord_key: count-prefix, Esc, which-key BS.
        // Migrated to keymap (issue #120):
        //   Phase 2: Ctrl-^, K, `:`, `/`, `?`
        //   Phase 3: H/L buffer cycle (BufferCycleH/L),
        //            Ctrl-h/j/k/l window focus + tmux (TmuxNavigate)
        if self.active().editor.vim_mode() == VimMode::Normal {
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
                        return KeyOutcome::Continue;
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
                    let could_start_chord = !self.app_keymap.pending(Mode::Normal).is_empty()
                        || to_km_event(key).is_some_and(|km_ev| {
                            let root = self
                                .app_keymap
                                .children_all(Mode::Normal, &KmChord::from_events(vec![]));
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
                let is_ctrl_char =
                    key.modifiers == KeyModifiers::CONTROL && matches!(key.code, KeyCode::Char(_));
                if !is_ctrl_char && !self.pending_count.is_empty() {
                    self.flush_pending_count_to_engine();
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
                    return KeyOutcome::Continue;
                }
                if self.which_key_sticky {
                    // At root in sticky mode — noop per spec.
                    return KeyOutcome::Continue;
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
                return KeyOutcome::Break;
            }
            return KeyOutcome::Continue;
        }

        // ── Insert-mode completion key handling ──────────────────
        // This block intercepts specific keys in insert mode to
        // manage the completion popup, before forwarding to the engine.
        if self.active().editor.vim_mode() == VimMode::Insert {
            // Recorder hook for Insert-mode keys that this block consumes
            // (printable chars routed to insert_char, popup-open Backspace
            // routed to insert_backspace, etc). Those paths return
            // KeyOutcome::Continue without ever reaching dispatch_insert_key
            // or the engine FSM step wrapper, so the engine end_step
            // recorder doesn't fire. Skipped during replay so played-back
            // inputs don't append to the active recording. Keys that
            // fall through this block to dispatch_insert_key get their
            // recording from dispatch_insert_key's own hook.
            if self.active().editor.is_recording_macro()
                && !self.active().editor.is_replaying_macro()
            {
                let input = hjkl_engine_tui::crossterm_to_input(key);
                if input.key != hjkl_engine::Key::Null {
                    self.active_mut().editor.record_input(input);
                }
            }

            // <C-x><C-o> manual omni-completion trigger.
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('x') {
                self.pending_ctrl_x = true;
                return KeyOutcome::Continue;
            }
            if self.pending_ctrl_x {
                self.pending_ctrl_x = false;
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('o') {
                    self.lsp_request_completion();
                    return KeyOutcome::Continue;
                }
                // Any other key: fall through normally (consume pending_ctrl_x).
            }

            // Keys that navigate/accept/dismiss the popup (popup must be open).
            if self.completion.is_some() {
                match key.code {
                    // <C-n> / <C-p> navigate selection.
                    KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(ref mut p) = self.completion {
                            p.select_next();
                        }
                        return KeyOutcome::Continue;
                    }
                    KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(ref mut p) = self.completion {
                            p.select_prev();
                        }
                        return KeyOutcome::Continue;
                    }
                    // <Tab> or <C-y> accept selected item.
                    KeyCode::Tab => {
                        self.accept_completion();
                        self.sync_after_engine_mutation();
                        return KeyOutcome::Continue;
                    }
                    KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.accept_completion();
                        self.sync_after_engine_mutation();
                        return KeyOutcome::Continue;
                    }
                    // <C-e> dismiss without accepting.
                    KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.dismiss_completion();
                        return KeyOutcome::Continue;
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
                            let elapsed = self.active_mut().refresh_dirty_against_saved();
                            self.last_signature_us = elapsed;
                            if self.active().dirty {
                                self.active_mut().is_new_file = false;
                            }
                        }
                        let buffer_id = self.active().buffer_id;
                        if self.active_mut().editor.take_content_reset() {
                            self.handle_active_content_reset(buffer_id);
                        }
                        let edits = self.active_mut().editor.take_content_edits();
                        if !edits.is_empty() {
                            self.syntax.apply_edits(buffer_id, &edits);
                        }
                        self.lsp_notify_change_active(&edits);
                        // Defer TS reparse to the end-of-drain flush so a
                        // burst of insert-mode keys folds into one parse
                        // instead of paying per-keystroke sync cost.
                        self.pending_recompute = true;

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
                                // `.line(cur_row)` is O(log N) on rope storage
                                // and clones a single row, not the whole doc.
                                let rope = self.active().editor.buffer().rope();
                                let line = if cur_row < rope.len_lines() {
                                    hjkl_buffer::rope_line_str(&rope, cur_row)
                                } else {
                                    String::new()
                                };
                                line[anchor_col.min(line.len())..cur_col.min(line.len())]
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
                        return KeyOutcome::Continue;
                    }
                    KeyCode::Backspace if key.modifiers == KeyModifiers::NONE => {
                        // Phase 6.5: call insert primitive directly.
                        self.active_mut().editor.insert_backspace();
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
                            self.handle_active_content_reset(buffer_id);
                        }
                        let edits = self.active_mut().editor.take_content_edits();
                        if !edits.is_empty() {
                            self.syntax.apply_edits(buffer_id, &edits);
                        }
                        self.lsp_notify_change_active(&edits);
                        // Defer TS reparse to the end-of-drain flush so a
                        // burst of insert-mode keys folds into one parse
                        // instead of paying per-keystroke sync cost.
                        self.pending_recompute = true;

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
                                let rope = self.active().editor.buffer().rope();
                                let line = if cur_row < rope.len_lines() {
                                    hjkl_buffer::rope_line_str(&rope, cur_row)
                                } else {
                                    String::new()
                                };
                                line[anchor_col.min(line.len())..cur_col.min(line.len())]
                                    .to_string()
                            };
                            if let Some(ref mut popup) = self.completion {
                                popup.set_prefix(&new_prefix);
                                if popup.is_empty() {
                                    self.completion = None;
                                }
                            }
                        }
                        return KeyOutcome::Continue;
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
                    return KeyOutcome::Continue;
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
                        self.handle_active_content_reset(buffer_id);
                    }
                    let edits = self.active_mut().editor.take_content_edits();
                    if !edits.is_empty() {
                        self.syntax.apply_edits(buffer_id, &edits);
                    }
                    self.lsp_notify_change_active(&edits);
                    self.pending_recompute = true;
                    self.maybe_auto_trigger_completion(c);
                    return KeyOutcome::Continue;
                }
            }
        } else {
            // Left insert mode — dismiss popup.
            if self.completion.is_some() {
                self.dismiss_completion();
            }
        }

        KeyOutcome::FallThrough
    }

    /// Handle a single mouse event. Returns a [`MouseOutcome`] indicating
    /// whether the loop should `continue` or fall through.
    pub(crate) fn handle_mouse(&mut self, me: crossterm::event::MouseEvent) -> MouseOutcome {
        use crossterm::event::{MouseButton, MouseEventKind};
        // Skip while overlays are active — Phase 8 will handle
        // mouse in overlays.
        if self.command_field.is_some()
            || self.search_field.is_some()
            || self.picker.is_some()
            || self.info_popup.is_some()
        {
            return MouseOutcome::Continue;
        }
        // P11.3 — gate events by per-mode mouse flags.
        // Command-field overlay already handled above; here we gate
        // on the editor's vim mode for the remaining events.
        {
            let mode = self.active().editor.vim_mode();
            if !crate::app::mouse_enabled_for(mode, &self.mouse_flags) {
                return MouseOutcome::Continue;
            }
        }
        // 3 lines/cols per wheel notch — vim's `mousescroll` default.
        const WHEEL_TICKS: i16 = 3;
        use crossterm::event::KeyModifiers;
        /// Route scroll to the window under the cursor, focusing it
        /// if needed. Returns `false` when the pointer is outside
        /// every window (e.g. over the status bar) — caller should
        /// skip the scroll in that case.
        fn focus_window_under_cursor(app: &mut crate::app::App, col: u16, row: u16) -> bool {
            use crate::app::mouse;
            if let Some(win_id) = mouse::hit_test_window(app, col, row) {
                let current_focus = app.focused_window();
                if win_id != current_focus {
                    app.switch_focus(win_id);
                }
                true
            } else {
                false
            }
        }
        // Scroll arms set `pending_recompute` instead of calling
        // `recompute_and_install` synchronously. The main event loop
        // drains all currently-ready events before firing one recompute,
        // so a burst of mouse-wheel scroll events runs the sync query +
        // install pipeline ONCE per drain instead of N times per burst.
        // Without this the per-event ~2-5ms sync query stacked into
        // visible scroll lag.
        match me.kind {
            MouseEventKind::ScrollDown => {
                if me.modifiers.contains(KeyModifiers::SHIFT) {
                    if focus_window_under_cursor(self, me.column, me.row) {
                        self.active_mut().editor.scroll_right(WHEEL_TICKS);
                        self.sync_viewport_from_editor();
                        self.pending_recompute = true;
                    }
                } else if focus_window_under_cursor(self, me.column, me.row) {
                    self.active_mut().editor.scroll_down(WHEEL_TICKS);
                    self.sync_viewport_from_editor();
                    self.pending_recompute = true;
                }
            }
            MouseEventKind::ScrollUp => {
                if me.modifiers.contains(KeyModifiers::SHIFT) {
                    if focus_window_under_cursor(self, me.column, me.row) {
                        self.active_mut().editor.scroll_left(WHEEL_TICKS);
                        self.sync_viewport_from_editor();
                        self.pending_recompute = true;
                    }
                } else if focus_window_under_cursor(self, me.column, me.row) {
                    self.active_mut().editor.scroll_up(WHEEL_TICKS);
                    self.sync_viewport_from_editor();
                    self.pending_recompute = true;
                }
            }
            MouseEventKind::ScrollLeft if focus_window_under_cursor(self, me.column, me.row) => {
                self.active_mut().editor.scroll_left(WHEEL_TICKS);
                self.sync_viewport_from_editor();
                self.pending_recompute = true;
            }
            MouseEventKind::ScrollRight if focus_window_under_cursor(self, me.column, me.row) => {
                self.active_mut().editor.scroll_right(WHEEL_TICKS);
                self.sync_viewport_from_editor();
                self.pending_recompute = true;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                use crate::app::mouse;

                self.dismiss_hover_popup_on_click();

                // ── Phase 9: border-drag hit-test ─────────────────
                // Check BEFORE context-menu and window-click logic so
                // a border click never accidentally focuses a window.
                if let Some(hit) = mouse::hit_test_border(self, me.column, me.row) {
                    // Encode border position as a synthetic id for the
                    // double-click tracker. We use a large offset beyond
                    // real WindowIds to avoid collisions.
                    let synthetic_id: usize = usize::MAX
                        .wrapping_sub(hit.border_cell.0 as usize)
                        .wrapping_sub((hit.border_cell.1 as usize) << 16);
                    let count = self.mouse_click_tracker.register(synthetic_id, 0, 0);
                    if count == 2 {
                        // Double-click → equalize all splits.
                        self.equalize_split();
                    } else {
                        // Single click → begin drag.
                        let last_pos = match hit.orientation {
                            mouse::SplitOrientation::Vertical => me.column,
                            mouse::SplitOrientation::Horizontal => me.row,
                        };
                        self.border_drag = Some(crate::app::BorderDrag {
                            orientation: hit.orientation,
                            split_origin: hit.split_origin,
                            split_total: hit.split_total,
                            last_pos,
                        });
                    }
                    return MouseOutcome::Continue;
                }

                // ── Context-menu: click-inside → invoke / click-outside → dismiss
                if let Some(ref menu) = self.context_menu {
                    let screen_size = self.screen_rect();
                    let rect = crate::menu::bounding_rect(menu, screen_size);
                    let inside = me.column >= rect.x
                        && me.column < rect.x + rect.width
                        && me.row >= rect.y
                        && me.row < rect.y + rect.height;

                    if inside {
                        // Check whether click landed on a selectable row.
                        if me.row > rect.y && me.row < rect.y + rect.height - 1 {
                            let item_idx = (me.row - rect.y - 1) as usize;
                            let action = menu
                                .items
                                .get(item_idx)
                                .filter(|it| {
                                    it.enabled && it.action != crate::menu::MenuAction::Separator
                                })
                                .map(|it| it.action.clone());
                            self.context_menu = None;
                            if let Some(act) = action {
                                self.invoke_menu_action(act);
                            }
                        }
                        return MouseOutcome::Continue; // Don't fall through to editor click.
                    } else {
                        self.context_menu = None;
                        // Fall through to normal editor click.
                    }
                }

                // ── P4.1: Ctrl+Left-click → goto-definition ──────
                if me.modifiers.contains(KeyModifiers::CONTROL) {
                    if let mouse::Zone::Code {
                        win_id,
                        doc_row,
                        doc_col,
                    } = mouse::hit_test_zone(self, me.column, me.row)
                    {
                        // Focus window if needed.
                        let current_focus = self.focused_window();
                        if win_id != current_focus {
                            self.switch_focus(win_id);
                        }
                        self.active_mut().editor.mouse_click_doc(doc_row, doc_col);
                        self.sync_after_engine_mutation();
                        self.lsp_goto_definition();
                    }
                    // Ctrl+click outside Code zone is a no-op.
                    return MouseOutcome::Continue;
                }

                // ── P4.2: Shift+Left-click → extend visual selection
                if me.modifiers.contains(KeyModifiers::SHIFT) {
                    if let mouse::Zone::Code {
                        win_id,
                        doc_row,
                        doc_col,
                    } = mouse::hit_test_zone(self, me.column, me.row)
                    {
                        // Focus window if needed.
                        let current_focus = self.focused_window();
                        if win_id != current_focus {
                            self.switch_focus(win_id);
                        }
                        // Anchor at current cursor if not already visual.
                        if self.active().editor.vim_mode() != VimMode::Visual {
                            self.active_mut().editor.mouse_begin_drag();
                        }
                        self.active_mut()
                            .editor
                            .mouse_extend_drag_doc(doc_row, doc_col);
                        self.sync_after_engine_mutation();
                    }
                    // Shift+click outside Code zone is a no-op.
                    return MouseOutcome::Continue;
                }

                // Left-click on the tab bar / buffer line switches
                // to that tab or buffer.
                match mouse::hit_test_zone(self, me.column, me.row) {
                    mouse::Zone::TabBar { tab_idx } => {
                        if tab_idx != self.active_tab {
                            self.switch_tab(tab_idx);
                        }
                        return MouseOutcome::Continue;
                    }
                    mouse::Zone::BufferLine { slot_idx } => {
                        if slot_idx != self.focused_slot_idx() {
                            self.switch_to(slot_idx);
                        }
                        return MouseOutcome::Continue;
                    }
                    _ => {}
                }

                if let Some(win_id) = mouse::hit_test_window(self, me.column, me.row) {
                    // Focus the clicked window if it differs.
                    let current_focus = self.focused_window();
                    if win_id != current_focus {
                        self.switch_focus(win_id);
                    }
                    if let Some((doc_row, doc_col)) =
                        mouse::cell_to_doc(self, win_id, me.column, me.row)
                    {
                        let count = self.mouse_click_tracker.register(win_id, doc_row, doc_col);
                        match count {
                            1 => {
                                self.active_mut().editor.mouse_click_doc(doc_row, doc_col);
                            }
                            2 => {
                                // Double-click: select word.
                                self.active_mut().editor.mouse_click_doc(doc_row, doc_col);
                                let line = {
                                    let rope = self.active().editor.buffer().rope();
                                    if doc_row < rope.len_lines() {
                                        hjkl_buffer::rope_line_str(&rope, doc_row)
                                    } else {
                                        String::new()
                                    }
                                };
                                let (ws, we) = mouse::word_bounds(&line, doc_col);
                                // Anchor at word start, cursor at word end - 1.
                                self.active_mut().editor.enter_visual_char();
                                self.active_mut().editor.set_cursor_doc(doc_row, ws);
                                self.active_mut().editor.mouse_begin_drag();
                                self.active_mut()
                                    .editor
                                    .set_cursor_doc(doc_row, we.saturating_sub(1).max(ws));
                            }
                            _ => {
                                // Triple-click (and count≥4 wraps to 1 in tracker,
                                // so this branch only fires at count==3).
                                self.active_mut().editor.mouse_click_doc(doc_row, doc_col);
                                self.active_mut().editor.enter_visual_line();
                            }
                        }
                        self.sync_after_engine_mutation();
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                use crate::app::mouse;

                // ── Phase 9: border drag ──────────────────────────
                if let Some(drag) = self.border_drag {
                    let new_pos = match drag.orientation {
                        mouse::SplitOrientation::Vertical => me.column,
                        mouse::SplitOrientation::Horizontal => me.row,
                    };
                    let split_pos = new_pos.saturating_sub(drag.split_origin);
                    self.resize_split_to(
                        drag.orientation,
                        drag.split_origin,
                        drag.split_total,
                        split_pos,
                    );
                    if let Some(d) = self.border_drag.as_mut() {
                        d.last_pos = new_pos;
                    }
                    return MouseOutcome::Continue;
                }

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
            // Up: clear any active border drag; vim stays in
            // Visual after a text drag-release — no-op otherwise.
            MouseEventKind::Up(MouseButton::Left) if self.border_drag.is_some() => {
                self.border_drag = None;
            }

            // ── P4.3: Middle-click → primary-selection paste ──────
            //
            // X11 / Wayland convention: middle-click pastes the
            // primary selection (whatever is currently highlighted
            // anywhere on screen, independent of the system
            // clipboard).  macOS / Windows have no primary
            // selection; we silently no-op when the clipboard
            // backend does not report `Capabilities::PRIMARY`.
            MouseEventKind::Down(MouseButton::Middle) => {
                self.dismiss_hover_popup_on_click();
                self.middle_click(me.column, me.row);
            }

            // ── Right-click: open context menu (Phase 2 + 7 + 8) ─
            MouseEventKind::Down(MouseButton::Right) => {
                use crate::app::mouse;
                use crate::menu::{
                    ContextMenu, build_code_menu, build_picker_menu, build_split_border_menu,
                    build_status_line_menu, build_tab_menu,
                };

                // Dismiss hover popup — same rationale as left-click.
                self.hover_popup = None;
                self.hover_timer = None;
                let zone = mouse::hit_test_zone(self, me.column, me.row);
                let items = match zone {
                    mouse::Zone::Code { .. } | mouse::Zone::Gutter { .. } => {
                        self.move_cursor_for_right_click(me.column, me.row);
                        let has_sel = matches!(
                            self.active().editor.vim_mode(),
                            VimMode::Visual | VimMode::VisualLine | VimMode::VisualBlock
                        );
                        build_code_menu(has_sel, self.active_has_lsp())
                    }
                    mouse::Zone::TabBar { tab_idx } => {
                        // Switch to the clicked tab first so that
                        // Close-Tab / Close-Right / Close-Left operate on it.
                        if tab_idx != self.active_tab {
                            self.switch_tab(tab_idx);
                        }
                        build_tab_menu(self.tabs.len() > 1)
                    }
                    mouse::Zone::BufferLine { slot_idx } => {
                        // Switch to the clicked buffer first so the
                        // tab menu's actions operate on it. Buffer
                        // line shares the tab menu for v1 — close /
                        // close-others / close-{left,right} have
                        // intuitive buffer-line semantics too.
                        if slot_idx != self.focused_slot_idx() {
                            self.switch_to(slot_idx);
                        }
                        build_tab_menu(self.tabs.len() > 1)
                    }
                    // ── Phase 7: status-line menu ─────────────────
                    mouse::Zone::StatusLine => {
                        let ft = self.active_filetype_label();
                        let lsp_name = self.active_lsp_server_name();
                        build_status_line_menu(&ft, lsp_name.as_deref())
                    }
                    // ── Phase 7: split-border menu ─────────────────
                    mouse::Zone::SplitBorder { .. } => build_split_border_menu(),
                    // ── Phase 8: picker overlay row menu ───────────
                    mouse::Zone::PickerRow { row_idx } => {
                        // Move picker selection to the clicked row.
                        if let Some(ref mut p) = self.picker {
                            p.selected = row_idx;
                        }
                        let has_path = self
                            .picker
                            .as_ref()
                            .and_then(|p| p.path_for_visible_row(p.selected))
                            .is_some();
                        build_picker_menu(has_path)
                    }
                    mouse::Zone::None => return MouseOutcome::Continue,
                };
                self.context_menu = Some(ContextMenu::new(items, (me.column, me.row)));
            }

            // ── Mouse hover: update selected item ────────────────
            MouseEventKind::Moved => {
                // Read viewport dims BEFORE borrowing menu mutably
                // (split-borrow workaround). The previous "anchor +
                // slack" approximation broke hover→item mapping for
                // menus anchored near the screen edges: when
                // `bounding_rect` flipped the popup upward to fit on
                // screen, this handler still used the original
                // anchor as the rect origin and mapped hovers to the
                // wrong items. Use the real terminal area instead.
                let screen_size = self.screen_rect();
                if let Some(menu) = &mut self.context_menu {
                    let rect = crate::menu::bounding_rect(menu, screen_size);
                    // Inner area (strip border row/col).
                    if me.row > rect.y
                        && me.row < rect.y + rect.height - 1
                        && me.column > rect.x
                        && me.column < rect.x + rect.width - 1
                    {
                        // Row inside inner content; map to item index.
                        let item_idx = (me.row - rect.y - 1) as usize;
                        if item_idx < menu.items.len() {
                            let enabled = menu.items[item_idx].enabled
                                && menu.items[item_idx].action
                                    != crate::menu::MenuAction::Separator;
                            if enabled {
                                menu.selected = item_idx;
                            }
                        }
                    }
                }

                // ── Phase 5: hover-popup timer ────────────────────
                let cell = (me.column, me.row);

                // Any mouse move dismisses an open hover popup and
                // resets the timer to track the new cell.
                if self.hover_popup.is_some() {
                    self.hover_popup = None;
                    self.hover_timer = None;
                }

                // Skip arming entirely while an overlay is up —
                // a hover for the doc cell behind the menu/picker
                // would render through the overlay.
                if !self.overlay_active() {
                    let same_cell = self.hover_timer.as_ref().is_some_and(|h| h.cell == cell);
                    if !same_cell {
                        self.hover_timer = Some(crate::app::HoverTimer {
                            cell,
                            started_at: std::time::Instant::now(),
                            request_sent: false,
                        });
                    }
                    // Fire check (also handled in the poll-timeout
                    // tick) so we react immediately on the Moved
                    // event that coincides with the 500ms threshold.
                    self.tick_hover_timer();
                }
            }

            _ => {}
        }
        MouseOutcome::FallThrough
    }

    /// Main event loop. Draws every frame, routes key events through
    /// the vim FSM, handles resize, exits on Ctrl-C.
    pub fn run(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        loop {
            // ── Per-tick setup ────────────────────────────────────
            // NOTE: sync_viewport_to_editor() is NOT called here — it is
            // called only on focus change (switch_focus / close_focused_window
            // / move_window_to_new_tab).  Calling it before every keypress
            // clobbered sticky_col and broke j/k column preservation (#151).
            self.drain_lsp_events();
            {
                let size = terminal.size()?;
                let vp = self.active_mut().editor.host_mut().viewport_mut();
                vp.width = size.width;
                vp.height = size.height.saturating_sub(STATUS_LINE_HEIGHT);
            }

            // ── Cursor shape ──────────────────────────────────────
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

            // ── Draw ──────────────────────────────────────────────
            let t_draw = std::time::Instant::now();
            terminal.draw(|frame| render::frame(frame, self))?;
            tracing::debug!(
                target: "hjkl::profile",
                draw_us = t_draw.elapsed().as_micros(),
                "draw"
            );

            // ── Async polls ───────────────────────────────────────
            self.drain_async_polls();

            // ── Poll timeout ──────────────────────────────────────
            let poll_timeout = self.compute_poll_timeout();

            // ── Wait for event ────────────────────────────────────
            if !event::poll(poll_timeout)? {
                let now = std::time::Instant::now();
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
                }
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
                self.tick_hover_timer();
                if self
                    .hover_popup
                    .as_ref()
                    .is_some_and(|p| p.is_expired(std::time::Instant::now()))
                {
                    self.hover_popup = None;
                    self.hover_timer = None;
                }
                self.indent_flash_active();
                continue;
            }

            // ── Dispatch event ────────────────────────────────────
            match event::read()? {
                Event::Key(key) => {
                    match self.handle_keypress(key) {
                        KeyOutcome::Break => break,
                        KeyOutcome::Continue => continue,
                        KeyOutcome::FallThrough => {}
                    }

                    // ── Normal editor key handling ────────────────
                    // Insert mode uses the inline dispatcher which calls
                    // Editor::insert_* primitives directly. Normal / Visual
                    // modes route through the FSM via hjkl_vim_tui::handle_key.
                    let mode_was_insert = self.active().editor.vim_mode() == VimMode::Insert;
                    if mode_was_insert {
                        self.dispatch_insert_key(key);
                        self.active_mut().editor.emit_cursor_shape_if_changed();
                    } else {
                        hjkl_vim_tui::handle_key(&mut self.active_mut().editor, key);
                    }

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
                        self.handle_active_content_reset(buffer_id);
                    }
                    let edits = self.active_mut().editor.take_content_edits();
                    if !edits.is_empty() {
                        self.syntax.apply_edits(buffer_id, &edits);
                        self.active_mut()
                            .editor
                            .shift_syntax_spans_for_edits(&edits);
                    }
                    self.lsp_notify_change_active(&edits);
                    self.pending_recompute = true;
                }
                Event::Mouse(me) => {
                    let _ = self.handle_mouse(me);
                }
                Event::Resize(w, h) => {
                    let vp = self.active_mut().editor.host_mut().viewport_mut();
                    vp.width = w;
                    vp.height = h.saturating_sub(STATUS_LINE_HEIGHT);
                }
                Event::FocusGained => {
                    self.checktime_all();
                }
                _ => {}
            }

            // Drain any additional events currently ready (e.g. a burst
            // of mouse-wheel scrolls) before running the deferred sync
            // query. Each scroll handler set `pending_recompute = true`
            // instead of firing `recompute_and_install` synchronously,
            // so we collapse the whole burst into one sync query install.
            let t_drain = std::time::Instant::now();
            let mut drained = 0usize;
            while event::poll(Duration::from_millis(0)).unwrap_or(false) {
                drained += 1;
                if let Ok(extra) = event::read() {
                    match extra {
                        Event::Key(k) => match self.handle_keypress(k) {
                            KeyOutcome::Break => {
                                self.exit_requested = true;
                                break;
                            }
                            KeyOutcome::Continue => continue,
                            KeyOutcome::FallThrough => {
                                let mode_was_insert =
                                    self.active().editor.vim_mode() == VimMode::Insert;
                                if mode_was_insert {
                                    self.dispatch_insert_key(k);
                                    self.active_mut().editor.emit_cursor_shape_if_changed();
                                } else {
                                    hjkl_vim_tui::handle_key(&mut self.active_mut().editor, k);
                                }
                                self.sync_viewport_from_editor();
                                if self.active_mut().editor.take_dirty() {
                                    let elapsed = self.active_mut().refresh_dirty_against_saved();
                                    self.last_signature_us = elapsed;
                                    if self.active().dirty {
                                        self.active_mut().is_new_file = false;
                                    }
                                }
                                let bid = self.active().buffer_id;
                                if self.active_mut().editor.take_content_reset() {
                                    self.handle_active_content_reset(bid);
                                }
                                let edits = self.active_mut().editor.take_content_edits();
                                if !edits.is_empty() {
                                    self.syntax.apply_edits(bid, &edits);
                                    self.active_mut()
                                        .editor
                                        .shift_syntax_spans_for_edits(&edits);
                                }
                                self.lsp_notify_change_active(&edits);
                                self.pending_recompute = true;
                            }
                        },
                        Event::Mouse(me2) => {
                            let _ = self.handle_mouse(me2);
                        }
                        Event::Resize(w, h) => {
                            let vp = self.active_mut().editor.host_mut().viewport_mut();
                            vp.width = w;
                            vp.height = h.saturating_sub(STATUS_LINE_HEIGHT);
                        }
                        Event::FocusGained => {
                            self.checktime_all();
                        }
                        _ => {}
                    }
                }
            }

            // Flush deferred recompute once after the drain loop ends.
            // Coalesces burst-scrolls (and rapid keystrokes within one
            // poll tick) into a single sync query + install.
            if drained > 0 {
                tracing::debug!(
                    target: "hjkl::profile",
                    drained,
                    drain_us = t_drain.elapsed().as_micros(),
                    "event drain"
                );
            }
            if self.pending_recompute {
                self.pending_recompute = false;
                self.recompute_and_install();
            }

            if self.exit_requested {
                break;
            }
        }
        Ok(())
    }

    /// Tick the Phase 5 hover timer.
    ///
    /// Called on every poll-timeout tick AND on every `MouseEventKind::Moved`
    /// event so the RPC fires promptly when the mouse has been stationary for
    /// [`HOVER_DELAY`]. If the timer is armed, the cell is in a Code zone, and
    /// the 500ms threshold has elapsed, sends the LSP hover RPC once.
    pub(crate) fn tick_hover_timer(&mut self) {
        // If a popup is already showing, nothing to do.
        if self.hover_popup.is_some() {
            return;
        }
        // Suppress hover firing while any overlay is on top of the editor —
        // a hover RPC for the doc cell BEHIND the overlay would show the
        // popup through the menu/picker/command field. Drop the timer too
        // so it doesn't fire the instant the overlay closes.
        if self.overlay_active() {
            self.hover_timer = None;
            return;
        }
        let (cell, should_fire) = match &self.hover_timer {
            Some(h) if !h.request_sent && h.started_at.elapsed() >= HOVER_DELAY => (h.cell, true),
            _ => return,
        };

        // Hit-test the resting cell for a Code zone.
        let zone = crate::app::mouse::hit_test_zone(self, cell.0, cell.1);
        if let crate::app::mouse::Zone::Code {
            doc_row, doc_col, ..
        } = zone
            && should_fire
        {
            self.lsp_hover_at_doc(doc_row, doc_col);
            if let Some(h) = self.hover_timer.as_mut() {
                h.request_sent = true;
            }
        }
    }
}
