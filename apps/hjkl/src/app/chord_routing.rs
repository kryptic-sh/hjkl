//! Chord routing — converts raw crossterm key events into app actions or
//! engine key events, driving the pending-state FSM and keymap trie.
//!
//! Owns: [`App::km_to_crossterm`], [`App::replay_to_engine`],
//! [`App::route_chord_key`], [`App::route_chord_key_inner`].

use super::App;
use super::keymap_build::engine_input_to_key_event;
use hjkl_vim::VimEditorExt;

/// Maximum nested `@{reg}` expansion depth during a single macro replay.
/// Mirrors vim's `'maxmapdepth'`-style guard: a register that (transitively)
/// plays itself pushes one level per expansion; at the cap the whole replay
/// chain aborts with E169 instead of overflowing the stack. The buffer is
/// left in whatever state the replay reached (vim semantics — no rollback).
const MACRO_MAX_DEPTH: usize = 100;

/// Cap on the total number of inputs fed by one top-level `@{reg}`
/// invocation (all repetitions and nested expansions included). The replay
/// loop is synchronous — the UI cannot repaint or take Ctrl-C until it
/// finishes — so a runaway `999999999@a` must abort with an error rather
/// than freeze the editor. 200k inputs is far beyond any interactive macro
/// workload while still aborting a runaway replay in well under a second.
const MACRO_MAX_INPUTS: usize = 200_000;

/// One entry in the macro-replay work queue.
enum ReplayItem {
    /// A decoded keystroke to feed through the routing stack.
    Input(hjkl_engine::input::Input),
    /// Sentinel marking the end of a nested `@{reg}` expansion: popping it
    /// decrements the nesting depth counter.
    DepthPop,
}

/// Explicit work queue for `@{reg}` macro replay (audit R2).
///
/// Replay is iterative: the top-level `PlayMacro` commit arm drains this
/// queue in a flat loop, and a nested `@{reg}` encountered *during* the
/// drain splices the callee's keys into the FRONT of the queue instead of
/// recursing into `route_chord_key`'s replay arm. The Rust call stack
/// therefore stays O(1) in replay depth; `depth`/`fed` bound the expansion
/// (see [`MACRO_MAX_DEPTH`] / [`MACRO_MAX_INPUTS`]).
#[derive(Default)]
pub(crate) struct MacroReplayState {
    /// Pending items, drained front-to-back.
    queue: std::collections::VecDeque<ReplayItem>,
    /// Current nested `@` expansion depth (top-level splice is depth 0).
    depth: usize,
    /// Total inputs fed to the routing stack this top-level invocation.
    fed: usize,
    /// Set when a cap fires; the drain loop stops and discards the queue.
    aborted: bool,
}

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
            hjkl_vim_tui::handle_key(self.active_editor_mut(), ct_ev);
        }
    }

    /// Replay a set of unbound `KeyEvent`s from the explorer keymap through
    /// the normal routing path, bypassing the explorer keymap (step 2b).
    ///
    /// When `pending_state` is set (e.g. after the first `g` of `gg` fires
    /// `BeginPendingAfterG`) or the engine already has a chord pending, the
    /// key goes straight to the engine / pending_state reducer via
    /// `route_chord_key` — which correctly processes the second `g` of `gg`
    /// through the `AfterG` reducer and emits GoToFirstLine.
    ///
    /// When no state is pending, the key goes through `dispatch_keymap` (the
    /// global `app_keymap` trie) and falls through to the engine on `Unbound`.
    /// This handles `<leader>e`, `<C-w>l`, and single-key engine motions
    /// without re-entering step 2b (the explorer keymap).
    fn replay_explorer_unbound(&mut self, events: Vec<hjkl_keymap::KeyEvent>) {
        for ev in events {
            if self.pending_state.is_some() || self.active_editor().is_chord_pending() {
                // A chord is in flight — send the key through the full routing
                // stack which includes the pending_state reducer in step 1.
                // Step 2b is gated on `pending_state.is_none()` so it is
                // skipped automatically, preventing re-entry into the explorer
                // keymap.
                let ct_ev = Self::km_to_crossterm(&ev);
                if !self.route_chord_key(ct_ev) {
                    hjkl_vim_tui::handle_key(self.active_editor_mut(), ct_ev);
                }
            } else {
                // No chord in flight — go through the global app_keymap trie
                // directly (skips step 2b; explorer keymap not consulted again).
                let mut replay = Vec::new();
                if !self.dispatch_keymap(ev, self.pending_count.peek().max(1), &mut replay) {
                    self.replay_to_engine(&replay);
                }
            }
        }
    }

    /// `@{reg}` / `@@` — canonical entry for the `PlayMacro` commit arm
    /// (production routing AND the `drive_key` test helper).
    ///
    /// Top-level invocation (no replay active) starts the iterative drain
    /// loop; a nested invocation (a replayed input was itself `@{reg}`)
    /// splices the callee's keys into the front of the active work queue.
    /// Either way the Rust call stack stays O(1) in replay depth.
    pub(crate) fn play_macro_chord(&mut self, reg: char, count: usize) {
        if self.macro_replay.is_some() {
            self.splice_macro_replay(reg, count);
        } else {
            self.run_macro_replay(reg, count);
        }
    }

    /// Top-level `@{reg}` replay: drain the work queue iteratively.
    ///
    /// `count` repetitions are replayed by re-splicing `keys` once per round
    /// (memory O(keys.len())), NOT by materializing `keys × count`. Replayed
    /// inputs are re-fed through `route_chord_key`, falling through to the
    /// engine for keys the chord layer does not consume — the same pattern
    /// as the live event-loop key path. During replay
    /// `is_replaying_macro() == true` so the recorder hook skips the
    /// replayed inputs; `end_macro_replay` runs exactly once at the end, as
    /// does `sync_after_engine_mutation` (including on abort — vim
    /// semantics: the buffer keeps whatever state the replay reached).
    fn run_macro_replay(&mut self, reg: char, count: usize) {
        use crossterm::event::KeyCode;
        debug_assert!(
            self.macro_replay.is_none(),
            "run_macro_replay must only start at top level"
        );
        let keys = self.active_editor_mut().play_macro(reg);
        if keys.is_empty() {
            // Unset / empty register — nothing to replay (play_macro leaves
            // the replaying flag untouched in this case; clearing it anyway
            // is harmless and preserves the previous arm's behavior).
            self.active_editor_mut().end_macro_replay();
            self.sync_after_engine_mutation();
            return;
        }
        let reps = count.clamp(1, hjkl_vim::vim::MAX_COUNT);
        self.macro_replay = Some(MacroReplayState::default());
        'reps: for _ in 0..reps {
            // Splice one repetition into the (empty-between-rounds) queue.
            self.macro_replay
                .as_mut()
                .expect("macro_replay alive during drain")
                .queue
                .extend(keys.iter().copied().map(ReplayItem::Input));
            while let Some(item) = self
                .macro_replay
                .as_mut()
                .expect("macro_replay alive during drain")
                .queue
                .pop_front()
            {
                let input = match item {
                    ReplayItem::DepthPop => {
                        let st = self
                            .macro_replay
                            .as_mut()
                            .expect("macro_replay alive during drain");
                        st.depth = st.depth.saturating_sub(1);
                        continue;
                    }
                    ReplayItem::Input(input) => input,
                };
                let over_cap = {
                    let st = self
                        .macro_replay
                        .as_mut()
                        .expect("macro_replay alive during drain");
                    st.fed += 1;
                    st.fed > MACRO_MAX_INPUTS
                };
                if over_cap {
                    self.bus
                        .error("E169: Command too recursive (macro replay input cap)");
                    break 'reps;
                }
                let ct_key = engine_input_to_key_event(input);
                if ct_key.code == KeyCode::Null {
                    continue;
                }
                if !self.route_chord_key(ct_key) {
                    hjkl_vim_tui::handle_key(self.active_editor_mut(), ct_key);
                }
                // A nested `@{reg}` splice may have tripped a cap — abort the
                // entire replay chain (vim stops the whole replay on error).
                if self
                    .macro_replay
                    .as_ref()
                    .expect("macro_replay alive during drain")
                    .aborted
                {
                    break 'reps;
                }
            }
        }
        self.macro_replay = None;
        self.active_editor_mut().end_macro_replay();
        self.sync_after_engine_mutation();
    }

    /// Nested `@{reg}` during an active replay: splice the callee's keys
    /// into the FRONT of the work queue (so they play before the caller's
    /// remaining keys — the order recursion would have produced), bounded by
    /// the depth cap and the total-input budget.
    fn splice_macro_replay(&mut self, reg: char, count: usize) {
        if self.macro_replay.as_ref().is_none_or(|st| st.aborted) {
            return;
        }
        // Resolve first: play_macro re-sets `last_macro` (so a later `@@`
        // resolves correctly) and keeps `replaying_macro = true`.
        let keys = self.active_editor_mut().play_macro(reg);
        if keys.is_empty() {
            return; // unset / empty register — replays nothing, no error
        }
        let reps = count.clamp(1, hjkl_vim::vim::MAX_COUNT);
        let (too_deep, over_budget) = {
            let st = self
                .macro_replay
                .as_ref()
                .expect("splice only runs during an active replay");
            let need = reps.saturating_mul(keys.len());
            (
                st.depth >= MACRO_MAX_DEPTH,
                // Budget check BEFORE materializing: a nested `999999999@b`
                // must abort here, not allocate count × keys.len() items.
                st.fed.saturating_add(st.queue.len()).saturating_add(need) > MACRO_MAX_INPUTS,
            )
        };
        if too_deep || over_budget {
            self.macro_replay
                .as_mut()
                .expect("splice only runs during an active replay")
                .aborted = true;
            self.bus.error(if too_deep {
                "E169: Command too recursive"
            } else {
                "E169: Command too recursive (macro replay input cap)"
            });
            return;
        }
        let st = self
            .macro_replay
            .as_mut()
            .expect("splice only runs during an active replay");
        st.depth += 1;
        st.queue.push_front(ReplayItem::DepthPop);
        for _ in 0..reps {
            for &k in keys.iter().rev() {
                st.queue.push_front(ReplayItem::Input(k));
            }
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
        let was_recording_before = self.active_editor().is_recording_macro();
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
        let is_recording_now = self.active_editor().is_recording_macro();
        let is_replaying_now = self.active_editor().is_replaying_macro();
        let just_started_recording = !was_recording_before && is_recording_now;
        let register_name_of_play = was_play_macro_pending;
        if consumed
            && is_recording_now
            && !is_replaying_now
            && !just_started_recording
            && !register_name_of_play
        {
            let input = hjkl_engine_tui::crossterm_to_input(key);
            if input.key != hjkl_engine::Key::Null {
                self.active_editor_mut().record_input(input);
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

        // Issue #37: q: / q/ / q? — intercept before hjkl_vim::step sees the
        // register-name key, so `:`, `/`, `?` open the cmdline window instead
        // of starting a macro.  Must precede the generic pending_state block.
        if matches!(
            self.pending_state,
            Some(hjkl_vim::PendingState::RecordMacroTarget)
        ) && key.modifiers == crossterm::event::KeyModifiers::NONE
        {
            let kind = match key.code {
                KeyCode::Char(':') => Some(crate::app::CmdLineKind::Ex),
                KeyCode::Char('/') => Some(crate::app::CmdLineKind::SearchForward),
                KeyCode::Char('?') => Some(crate::app::CmdLineKind::SearchBackward),
                _ => None,
            };
            if let Some(ck) = kind {
                self.pending_state = None;
                self.open_cmdline_window(ck, None);
                return true;
            }
        }

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
                        self.active_editor_mut().replace_char_at(ch, count);
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
                        self.active_editor_mut().find_char(ch, forward, till, count);
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
                            // `gb` — toggle the read-only git BLAME view/mode.
                            'b' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::GitBlameLine,
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
                        // Chord-init g-prefixed operators: intercept u/U/~/q/w/c.
                        //
                        // - In Normal mode: set the App-level AfterOp reducer
                        //   (gives gU/gu/g~/gq/gw/gc/gcc the same timeout-safe
                        //   pending path as d/y/c — fixes intermittent gcc
                        //   no-ops caused by the 1000ms engine chord-timeout
                        //   firing between keystrokes when the chord went
                        //   through the engine).
                        // - In Visual modes: fire the operator immediately on
                        //   the active selection (no additional keystroke
                        //   needed — that's the vim semantics for visual gc /
                        //   gu / gU / g~ / gq / gw).
                        let case_op_kind = match ch {
                            'u' => Some(hjkl_vim::OperatorKind::Lowercase),
                            'U' => Some(hjkl_vim::OperatorKind::Uppercase),
                            '~' => Some(hjkl_vim::OperatorKind::ToggleCase),
                            'q' => Some(hjkl_vim::OperatorKind::Reflow),
                            'w' => Some(hjkl_vim::OperatorKind::ReflowKeepCursor),
                            'c' => Some(hjkl_vim::OperatorKind::Comment),
                            _ => None,
                        };
                        if let Some(op) = case_op_kind {
                            use hjkl_engine::VimMode;
                            let mode = self.active_editor().vim_mode();
                            if matches!(
                                mode,
                                VimMode::Visual | VimMode::VisualLine | VimMode::VisualBlock
                            ) {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::VisualOp {
                                        op,
                                        count: count as u32,
                                    },
                                    count as u32,
                                );
                            } else {
                                self.pending_state = Some(hjkl_vim::PendingState::AfterOp {
                                    op,
                                    count1: count,
                                    inner_count: 0,
                                });
                            }
                            return true;
                        }
                        // All other g-chords: delegate to engine.
                        self.active_editor_mut().after_g(ch, count);
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::AfterZChord { ch, count }) => {
                        self.pending_state = None;
                        // All z-chords delegate directly to the engine.
                        self.active_editor_mut().after_z(ch, count);
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpMotion {
                        op,
                        motion_key,
                        total_count,
                    }) => {
                        self.pending_state = None;
                        // Filter with motion (!<motion>): resolve the row range, then
                        // open the filter prompt so the user can type the shell command.
                        if op == hjkl_vim::OperatorKind::Filter {
                            if let Some((top, bot)) = self
                                .active_editor_mut()
                                .range_for_op_motion(motion_key, total_count)
                            {
                                tracing::debug!(
                                    top,
                                    bot,
                                    "filter operator: opening prompt for motion range"
                                );
                                self.open_filter_prompt(top, bot);
                            }
                            return true;
                        }
                        // AutoIndent with motion (=<motion>): dry-run the motion to
                        // find the row range, then submit the async formatter.
                        // Falls back to dumb algo when no formatter is registered.
                        let used_formatter = op == hjkl_vim::OperatorKind::AutoIndent && {
                            let range = self
                                .active_editor_mut()
                                .range_for_op_motion(motion_key, total_count)
                                .map(|(r0, r1)| hjkl_mangler::RangeSpec {
                                    start_row: r0,
                                    end_row: r1,
                                });
                            self.submit_external_format(range)
                        };
                        if !used_formatter {
                            self.active_editor_mut().apply_op_motion(
                                super::event_loop::op_kind_to_operator(op),
                                motion_key,
                                total_count,
                            );
                            if let Some((top, bot)) =
                                self.active_editor_mut().take_last_indent_range()
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
                        // Filter (!!): open the filter prompt for the current line range.
                        // `!!` filters `total_count` lines starting at the cursor.
                        if op == hjkl_vim::OperatorKind::Filter {
                            let cursor_row = self.active_editor().cursor().0;
                            let bot_row = cursor_row
                                .saturating_add(total_count.max(1))
                                .saturating_sub(1);
                            tracing::debug!(
                                cursor_row,
                                bot_row,
                                "filter operator: opening prompt for double (!!)"
                            );
                            self.open_filter_prompt(cursor_row, bot_row);
                            return true;
                        }
                        // AutoIndent (==): submit async formatter with cursor-row range.
                        // Falls back to dumb algo when no formatter is registered.
                        let used_formatter = op == hjkl_vim::OperatorKind::AutoIndent && {
                            let cursor_row = self.active_editor().cursor().0;
                            let end_row = cursor_row.saturating_add(total_count).saturating_sub(1);
                            let range = hjkl_mangler::RangeSpec {
                                start_row: cursor_row,
                                end_row,
                            };
                            self.submit_external_format(Some(range))
                        };
                        if !used_formatter {
                            self.active_editor_mut().apply_op_double(
                                super::event_loop::op_kind_to_operator(op),
                                total_count,
                            );
                            if let Some((top, bot)) =
                                self.active_editor_mut().take_last_indent_range()
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
                                .active_editor()
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
                        self.active_editor_mut().apply_op_text_obj(
                            super::event_loop::op_kind_to_operator(op),
                            ch,
                            inner,
                            total_count,
                        );
                        if let Some((top, bot)) = self.active_editor_mut().take_last_indent_range()
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
                                .active_editor_mut()
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
                        self.active_editor_mut().apply_op_g(
                            super::event_loop::op_kind_to_operator(op),
                            ch,
                            total_count,
                        );
                        if let Some((top, bot)) = self.active_editor_mut().take_last_indent_range()
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
                        self.active_editor_mut().apply_op_find(
                            super::event_loop::op_kind_to_operator(op),
                            ch,
                            forward,
                            till,
                            total_count,
                        );
                        if let Some((top, bot)) = self.active_editor_mut().take_last_indent_range()
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
                        self.active_editor_mut().set_pending_register(reg);
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::SetMark { ch }) => {
                        self.pending_state = None;
                        self.active_editor_mut().set_mark_at_cursor(ch);
                        // No sync needed — set_mark_at_cursor does not move cursor.
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::GotoMarkLine { ch }) => {
                        self.pending_state = None;
                        let jump = self.active_editor_mut().try_goto_mark_line(ch);
                        self.apply_mark_jump(jump, true);
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::GotoMarkChar { ch }) => {
                        self.pending_state = None;
                        let jump = self.active_editor_mut().try_goto_mark_char(ch);
                        self.apply_mark_jump(jump, false);
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
                        self.active_editor_mut().start_macro_record(reg);
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
                        // `@{reg}` chord completed — run (or splice into) the
                        // iterative replay work queue. NEVER recurses: a
                        // nested `@{reg}` inside a playing macro splices its
                        // keys into the front of the active queue instead of
                        // re-entering this arm's drain loop (audit R2).
                        self.play_macro_chord(reg, count);
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
            && self.active_editor().vim_mode() != hjkl_engine::VimMode::Normal
            && let Some(km_ev) = crate::keymap_translate::from_crossterm(&key)
            && let Some(km_mode) = super::current_km_mode(self)
        {
            // Visual-mode count prefix: the digit buffer is populated in
            // event_loop's visual-mode arm. Pass it as the dispatch count so
            // visual ops/motions that read `action_count` see it; handlers that
            // read `pending_count` directly (VisualOp / Motion) drain it via
            // `take_or`. Mirrors the Normal-mode dispatch in step (3) below.
            let count = self.pending_count.peek().max(1);
            let mut replay: Vec<hjkl_keymap::KeyEvent> = Vec::new();
            let consumed = self.dispatch_keymap_in_mode(km_ev, count, &mut replay, km_mode);
            if consumed {
                self.sync_after_engine_mutation();
                return true;
            }
            // Unbound — flush any buffered count to the engine so engine-handled
            // visual commands (e.g. `2J`) still receive it, then fall through.
            if !self.pending_count.is_empty() {
                self.flush_pending_count_to_engine();
            }
        }

        // (2b) Explorer-keymap dispatch — when the sidebar is focused, feed the
        // explorer keymap BEFORE the global app_keymap so explorer bindings win
        // over global ones (e.g. `r`, `d`, `y`, `x`, `s`, `v` etc.).
        // Gated identically to step (3): Normal mode, no pending state, no
        // engine-pending chord. Additionally gated on app_keymap having no
        // pending chord: once `<C-w>` or `<leader>` goes pending in app_keymap
        // the next key must complete that global chord, not fire an explorer bind.
        if self.pending_state.is_none()
            && self.explorer_buf_focused()
            && self.active_editor().vim_mode() == hjkl_engine::VimMode::Normal
            && !self.active_editor().is_chord_pending()
            && self.app_keymap.pending(hjkl_vim::Mode::Normal).is_empty()
            && let Some(km_ev) = crate::keymap_translate::from_crossterm(&key)
        {
            use hjkl_keymap::KeyResolve;
            let mode = hjkl_vim::Mode::Normal;
            let count = self.pending_count.peek().max(1);
            let now = std::time::Instant::now();
            match self.explorer_keymap.feed(mode, km_ev, now) {
                KeyResolve::Pending | KeyResolve::Ambiguous => {
                    self.note_prefix_set();
                    self.sync_after_engine_mutation();
                    return true;
                }
                KeyResolve::Match(binding) => {
                    self.clear_prefix_state();
                    self.dispatch_action(binding.action, count);
                    self.sync_after_engine_mutation();
                    return true;
                }
                KeyResolve::Unbound(events) => {
                    // Not an explorer chord — replay through the normal
                    // app_keymap / engine path so that multi-key engine
                    // chords (gg, ge, …) and global app chords (<leader>e,
                    // <C-w>l, …) work correctly from the explorer.
                    self.replay_explorer_unbound(events);
                    self.sync_after_engine_mutation();
                    return true;
                }
            }
        }

        // (3) Normal-mode keymap dispatch — only the trie step; count-prefix
        // buffering and engine-pending bypass run in event_loop.rs before this
        // call and set up the correct state for dispatch_keymap to read.
        if self.pending_state.is_none()
            && self.active_editor().vim_mode() == hjkl_engine::VimMode::Normal
            && let Some(km_ev) = crate::keymap_translate::from_crossterm(&key)
        {
            let engine_pending = self.active_editor().is_chord_pending();
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
