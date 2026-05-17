//! Chord routing — converts raw crossterm key events into app actions or
//! engine key events, driving the pending-state FSM and keymap trie.
//!
//! Owns: [`App::km_to_crossterm`], [`App::replay_to_engine`],
//! [`App::route_chord_key`], [`App::route_chord_key_inner`].

use super::App;
use super::keymap_build::engine_input_to_key_event;

impl App {
    /// Convert a `hjkl_keymap::KeyEvent` back to a `crossterm::event::KeyEvent`
    /// for replaying unbound sequences to the engine.
    ///
    /// Moved here from `event_loop.rs` (option A) so that both the event loop
    /// and tests can replay keymap events without touching file-local functions.
    pub(crate) fn km_to_crossterm(ev: &hjkl_keymap::KeyEvent) -> crossterm::event::KeyEvent {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hjkl_keymap::{KeyCode as KmKeyCode, KeyModifiers as KmKeyMods};
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

    /// Replay a slice of `hjkl_keymap::KeyEvent`s to the engine via crossterm
    /// `KeyEvent`s. Each keymap event is converted back to a crossterm event
    /// and forwarded to `editor.handle_key`.
    ///
    /// Moved here from `event_loop.rs` (option A) for testability.
    pub(crate) fn replay_to_engine(&mut self, events: &[hjkl_keymap::KeyEvent]) {
        for km_ev in events {
            let ct_ev = Self::km_to_crossterm(km_ev);
            hjkl_vim::handle_key(&mut self.active_mut().editor, ct_ev);
        }
    }

    /// Single canonical chord-routing entry. Called by the event loop's key
    /// handler and by tests. Returns `true` if the key was consumed at any
    /// stage of the chord routing; `false` if it should fall through to the
    /// engine `handle_key` path.
    ///
    /// Order (matches production event loop exactly — this IS the production
    /// routing now, not a test mirror):
    ///   1. pending_state reducer (all modes, when `pending_state.is_some()`)
    ///   2. Non-Normal trie dispatch (mode != Normal AND pending_state.is_none())
    ///   3. Normal-mode keymap dispatch (mode == Normal AND pending_state.is_none())
    ///
    /// Out of scope (run BEFORE this method in event_loop.rs):
    ///   - command-field overlay (`self.command_field.is_some()`)
    ///   - search-field overlay (`self.search_field.is_some()`)
    ///   - picker overlay (`self.picker.is_some()`)
    ///   - info-popup dismissal
    ///   - Visual-mode `:` intercept (must precede pending_state reducer)
    ///   - Insert-mode completion handling
    ///   - tmux-navigator Ctrl-h/j/k/l (Phase 3 — issue #120)
    ///   - count-prefix buffering (digits `0`–`9` in Normal mode)
    ///   - Shift-H / Shift-L buffer cycle (Phase 3 — issue #120)
    ///   - Esc chord-reset and which-key Backspace navigate-up
    ///
    /// Migrated to keymap trie (issue #120 Phase 2 — now dispatch_action arms):
    ///   - `K` → `AppAction::LspHover`
    ///   - `:` → `AppAction::OpenCommandPrompt`
    ///   - `/` / `?` → `AppAction::OpenSearchPrompt`
    ///   - `<C-^>` / `<C-6>` → `AppAction::BufferAlt`
    pub(crate) fn route_chord_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        // Snapshot recording state BEFORE dispatch so we can detect the moment
        // a new recording starts (StartMacroRecord arm) — the register-name key
        // that triggered the start is a bookkeeping key and must NOT be recorded.
        // Similarly, if we were not recording before and are now, skip this key.
        //
        // The @{reg} register-name key (PlayMacro arm) also must not be recorded;
        // that arm returns early in route_chord_key_inner so is_recording_macro()
        // state doesn't change between before/after — BUT recording may be active
        // before the @{reg} key (recording a macro that includes a @a call). In
        // that case we ALSO skip the register name (pending_was_macro_chord logic).
        let was_recording_before = self.active().editor.is_recording_macro();
        let was_play_macro_pending = matches!(
            self.pending_state,
            Some(hjkl_vim::PendingState::PlayMacroTarget { .. })
        );
        let consumed = self.route_chord_key_inner(key);
        // Recorder hook: append the consumed key to the active macro recording
        // (if any) so replays reproduce the same sequence. Skip:
        //   1. When not consumed (key was not processed).
        //   2. When replaying (is_replaying_macro).
        //   3. When the key just started a new recording (was_recording_before
        //      was false but is_recording_macro() is now true — the `a` in `qa`
        //      is the register-name bookkeeping key).
        //   4. When the key was the second half of a @{reg} chord
        //      (was_play_macro_pending) — the register name is bookkeeping.
        let is_recording_now = self.active().editor.is_recording_macro();
        let is_replaying_now = self.active().editor.is_replaying_macro();
        let just_started_recording = !was_recording_before && is_recording_now;
        let register_name_of_play = was_play_macro_pending;
        if consumed
            && is_recording_now
            && !is_replaying_now
            && !just_started_recording
            && !register_name_of_play
        {
            let input = hjkl_engine::Input::from(key);
            if input.key != hjkl_engine::Key::Null {
                self.active_mut().editor.record_input(input);
            }
        }
        consumed
    }

    /// Inner implementation of `route_chord_key`. Returns `true` if the key
    /// was consumed. The public wrapper adds the recorder hook on top.
    fn route_chord_key_inner(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        use hjkl_vim::{Key as VimKey, Outcome};

        // (1) pending_state reducer — fires in all modes when state is Some.
        // Must precede the Non-Normal trie dispatch so the second key of a
        // chord (e.g. second `g` of `gg` in VisualLine) reaches the commit
        // arm instead of re-firing BeginPendingAfterG via the trie.
        if let Some(state) = self.pending_state {
            let vim_key = match key.code {
                KeyCode::Char(c) => Some(VimKey::Char(c)),
                KeyCode::Esc => Some(VimKey::Esc),
                KeyCode::Enter => Some(VimKey::Enter),
                KeyCode::Backspace => Some(VimKey::Backspace),
                KeyCode::Tab => Some(VimKey::Tab),
                _ => None,
            };
            if let Some(vk) = vim_key {
                match hjkl_vim::step(state, vk) {
                    Outcome::Wait(new_state) => {
                        self.pending_state = Some(new_state);
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::ReplaceChar { ch, count }) => {
                        self.pending_state = None;
                        self.active_mut().editor.replace_char_at(ch, count);
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::FindChar {
                        ch,
                        forward,
                        till,
                        count,
                    }) => {
                        self.pending_state = None;
                        self.active_mut().editor.find_char(ch, forward, till, count);
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::AfterGChord { ch, count }) => {
                        self.pending_state = None;
                        // App-level g-prefix actions dispatched before falling
                        // through to the engine.
                        match ch {
                            // Phase 6.4: `gv` — reenter last visual selection.
                            'v' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::ReenterLastVisual,
                                    count as u32,
                                );
                                return true;
                            }
                            // Phase 6.4: `g*` / `g#` — word search without whole-word anchors.
                            '*' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::WordSearch {
                                        forward: true,
                                        whole_word: false,
                                        count: count as u32,
                                    },
                                    count as u32,
                                );
                                return true;
                            }
                            '#' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::WordSearch {
                                        forward: false,
                                        whole_word: false,
                                        count: count as u32,
                                    },
                                    count as u32,
                                );
                                return true;
                            }
                            't' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::Tabnext,
                                    count as u32,
                                );
                                return true;
                            }
                            'T' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::Tabprev,
                                    count as u32,
                                );
                                return true;
                            }
                            'd' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::LspGotoDef,
                                    count as u32,
                                );
                                return true;
                            }
                            'D' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::LspGotoDecl,
                                    count as u32,
                                );
                                return true;
                            }
                            'r' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::LspGotoRef,
                                    count as u32,
                                );
                                return true;
                            }
                            'i' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::LspGotoImpl,
                                    count as u32,
                                );
                                return true;
                            }
                            'y' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::LspGotoTypeDef,
                                    count as u32,
                                );
                                return true;
                            }
                            _ => {}
                        }
                        // Chord-init case-ops: intercept u/U/~/q and set
                        // reducer AfterOp instead of calling after_g (which
                        // would set engine Pending::Op). This keeps the full
                        // gU/gu/g~/gq op-pending path inside the reducer.
                        let case_op_kind = match ch {
                            'u' => Some(hjkl_vim::OperatorKind::Lowercase),
                            'U' => Some(hjkl_vim::OperatorKind::Uppercase),
                            '~' => Some(hjkl_vim::OperatorKind::ToggleCase),
                            'q' => Some(hjkl_vim::OperatorKind::Reflow),
                            _ => None,
                        };
                        if let Some(op) = case_op_kind {
                            self.pending_state = Some(hjkl_vim::PendingState::AfterOp {
                                op,
                                count1: count,
                                inner_count: 0,
                            });
                            return true;
                        }
                        // All other g-chords: delegate to engine.
                        self.active_mut().editor.after_g(ch, count);
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::AfterZChord { ch, count }) => {
                        self.pending_state = None;
                        // All z-chords delegate directly to the engine.
                        self.active_mut().editor.after_z(ch, count);
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpMotion {
                        op,
                        motion_key,
                        total_count,
                    }) => {
                        self.pending_state = None;
                        // AutoIndent with motion (=<motion>): dry-run the motion to
                        // find the row range, then submit the async formatter.
                        // Falls back to dumb algo when no formatter is registered.
                        let used_formatter = op == hjkl_vim::OperatorKind::AutoIndent && {
                            let range = self
                                .active_mut()
                                .editor
                                .range_for_op_motion(motion_key, total_count)
                                .map(|(r0, r1)| hjkl_mangler::RangeSpec {
                                    start_row: r0,
                                    end_row: r1,
                                });
                            self.submit_external_format(range)
                        };
                        if !used_formatter {
                            self.active_mut().editor.apply_op_motion(
                                super::event_loop::op_kind_to_operator(op),
                                motion_key,
                                total_count,
                            );
                            if let Some((top, bot)) =
                                self.active_mut().editor.take_last_indent_range()
                            {
                                self.indent_flash = Some(super::IndentFlash {
                                    top,
                                    bot,
                                    started_at: std::time::Instant::now(),
                                });
                            }
                        }
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpDouble { op, total_count }) => {
                        self.pending_state = None;
                        // AutoIndent (==): submit async formatter with cursor-row range.
                        // Falls back to dumb algo when no formatter is registered.
                        let used_formatter = op == hjkl_vim::OperatorKind::AutoIndent && {
                            let cursor_row = self.active().editor.cursor().0;
                            let end_row = cursor_row.saturating_add(total_count).saturating_sub(1);
                            let range = hjkl_mangler::RangeSpec {
                                start_row: cursor_row,
                                end_row,
                            };
                            self.submit_external_format(Some(range))
                        };
                        if !used_formatter {
                            self.active_mut().editor.apply_op_double(
                                super::event_loop::op_kind_to_operator(op),
                                total_count,
                            );
                            if let Some((top, bot)) =
                                self.active_mut().editor.take_last_indent_range()
                            {
                                self.indent_flash = Some(super::IndentFlash {
                                    top,
                                    bot,
                                    started_at: std::time::Instant::now(),
                                });
                            }
                        }
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpTextObj {
                        op,
                        ch,
                        inner,
                        total_count,
                    }) => {
                        self.pending_state = None;
                        // AutoIndent text-obj (=ap, =i{, etc): dry-run text-object
                        // range query, then submit the async formatter if applicable.
                        let used_formatter = op == hjkl_vim::OperatorKind::AutoIndent && {
                            let range = self
                                .active()
                                .editor
                                .range_for_op_text_obj(ch, inner, total_count)
                                .map(|(r0, r1)| hjkl_mangler::RangeSpec {
                                    start_row: r0,
                                    end_row: r1,
                                });
                            self.submit_external_format(range)
                        };
                        if used_formatter {
                            self.sync_after_engine_mutation();
                            return true;
                        }
                        self.active_mut().editor.apply_op_text_obj(
                            super::event_loop::op_kind_to_operator(op),
                            ch,
                            inner,
                            total_count,
                        );
                        if let Some((top, bot)) = self.active_mut().editor.take_last_indent_range()
                        {
                            self.indent_flash = Some(super::IndentFlash {
                                top,
                                bot,
                                started_at: std::time::Instant::now(),
                            });
                        }
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpG {
                        op,
                        ch,
                        total_count,
                    }) => {
                        self.pending_state = None;
                        // AutoIndent g-motion (=gg, =gj, etc): dry-run g-motion range
                        // query, then submit the async formatter if applicable.
                        let used_formatter = op == hjkl_vim::OperatorKind::AutoIndent && {
                            let range = self
                                .active_mut()
                                .editor
                                .range_for_op_g(ch, total_count)
                                .map(|(r0, r1)| hjkl_mangler::RangeSpec {
                                    start_row: r0,
                                    end_row: r1,
                                });
                            self.submit_external_format(range)
                        };
                        if used_formatter {
                            self.sync_after_engine_mutation();
                            return true;
                        }
                        self.active_mut().editor.apply_op_g(
                            super::event_loop::op_kind_to_operator(op),
                            ch,
                            total_count,
                        );
                        if let Some((top, bot)) = self.active_mut().editor.take_last_indent_range()
                        {
                            self.indent_flash = Some(super::IndentFlash {
                                top,
                                bot,
                                started_at: std::time::Instant::now(),
                            });
                        }
                        self.sync_after_engine_mutation();
                        return true;
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
                            super::event_loop::op_kind_to_operator(op),
                            ch,
                            forward,
                            till,
                            total_count,
                        );
                        if let Some((top, bot)) = self.active_mut().editor.take_last_indent_range()
                        {
                            self.indent_flash = Some(super::IndentFlash {
                                top,
                                bot,
                                started_at: std::time::Instant::now(),
                            });
                        }
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::SetPendingRegister { reg }) => {
                        self.pending_state = None;
                        self.active_mut().editor.set_pending_register(reg);
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::SetMark { ch }) => {
                        self.pending_state = None;
                        self.active_mut().editor.set_mark_at_cursor(ch);
                        // No sync needed — set_mark_at_cursor does not move cursor.
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::GotoMarkLine { ch }) => {
                        self.pending_state = None;
                        self.active_mut().editor.goto_mark_line(ch);
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::GotoMarkChar { ch }) => {
                        self.pending_state = None;
                        self.active_mut().editor.goto_mark_char(ch);
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::StartMacroRecord { reg }) => {
                        // `q{reg}` chord completed — begin recording. The
                        // bookkeeping key (`q` itself) was already excluded from
                        // the recording by QChord's pending-count reset path;
                        // this register-char is also a bookkeeping key (it names
                        // the register, not a replay action), so the recorder hook
                        // below must skip it. We set pending_state = None before
                        // returning so the hook sees None and skips naturally.
                        self.pending_state = None;
                        self.active_mut().editor.start_macro_record(reg);
                        // Do NOT call the recorder hook here — the register char is
                        // bookkeeping, not a recorded keystroke. Return immediately.
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::PlayMacro { reg, count }) => {
                        self.pending_state = None;
                        if reg == ':' {
                            // `@:` — repeat last ex command. App-side storage,
                            // NOT routed through engine.play_macro (which would
                            // look in a register). count > 1 → repeat N times
                            // (vim semantics). Phase 5d of kryptic-sh/hjkl#71.
                            for _ in 0..count.max(1) {
                                self.replay_last_ex();
                            }
                            return true;
                        }
                        // `@{reg}` chord completed — decode and re-feed the macro.
                        let inputs = self.active_mut().editor.play_macro(reg, count);
                        // Re-feed each Input through route_chord_key by converting
                        // it back to a crossterm KeyEvent. During replay,
                        // is_replaying_macro() == true so the recorder hook skips
                        // the replayed inputs.
                        for input in inputs {
                            let ct_key = engine_input_to_key_event(input);
                            if ct_key.code != KeyCode::Null {
                                self.route_chord_key(ct_key);
                            }
                        }
                        self.active_mut().editor.end_macro_replay();
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Cancel => {
                        self.pending_state = None;
                        return true;
                    }
                    Outcome::Forward => {
                        // State stays alive; fall through to step (2) below.
                    }
                }
            }
        }

        // (2) Non-Normal trie dispatch — gated on pending_state.is_none().
        // Step (1) above already returns early when pending_state.is_some(),
        // so this gate is logically redundant but documents intent: the second
        // key of a chord (e.g. second `g` of `gg` in VisualLine) must reach
        // the reducer's commit arm above, not re-fire the trie.
        if self.pending_state.is_none()
            && self.active().editor.vim_mode() != hjkl_engine::VimMode::Normal
            && let Some(km_ev) = crate::keymap_translate::from_crossterm(&key)
            && let Some(km_mode) = super::current_km_mode(self)
        {
            let mut replay: Vec<hjkl_keymap::KeyEvent> = Vec::new();
            let consumed = self.dispatch_keymap_in_mode(km_ev, 1, &mut replay, km_mode);
            if consumed {
                self.sync_after_engine_mutation();
                return true;
            }
            // Unbound — fall through to engine.
        }

        // (3) Normal-mode keymap dispatch — only the trie step; count-prefix
        // buffering and engine-pending bypass run in event_loop.rs before this
        // call and set up the correct state for dispatch_keymap to read.
        if self.pending_state.is_none()
            && self.active().editor.vim_mode() == hjkl_engine::VimMode::Normal
            && let Some(km_ev) = crate::keymap_translate::from_crossterm(&key)
        {
            let engine_pending = self.active().editor.is_chord_pending();
            if !engine_pending {
                let count = self.pending_count.peek().max(1);
                let mut replay: Vec<hjkl_keymap::KeyEvent> = Vec::new();
                let consumed = self.dispatch_keymap(km_ev, count, &mut replay);
                if !consumed {
                    if !self.pending_count.is_empty() {
                        self.flush_pending_count_to_engine();
                    }
                    self.replay_to_engine(&replay);
                }
                self.sync_after_engine_mutation();
                return true;
            }
        }

        false
    }
}
