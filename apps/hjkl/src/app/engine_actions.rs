//! Engine-action sub-dispatcher for `App::dispatch_action`.
//!
//! Handles variants that mutate the active editor engine directly:
//!   - DotRepeat
//!   - Motion
//!   - VisualOp (charwise / linewise / block visual operators)
//!   - EnterInsertI / EnterInsertShiftI / EnterInsertA / EnterInsertShiftA
//!   - EnterInsertO / EnterInsertShiftO / EnterReplace
//!   - DeleteCharForward / DeleteCharBackward
//!   - SubstituteChar / SubstituteLine
//!   - DeleteToEol / ChangeToEol / YankToEol
//!   - JoinLine / ToggleCase / PasteAfter / PasteBefore
//!   - Undo / Redo
//!   - JumpBack / JumpForward
//!   - ScrollFullPage / ScrollHalfPage / ScrollLine
//!   - SearchRepeat / WordSearch
//!   - EnterVisualChar / EnterVisualLine / EnterVisualBlock
//!   - ReenterLastVisual / VisualToggleAnchor
//!   - Replay

use crate::keymap_actions::AppAction;

use super::{App, IndentFlash};

use std::time::Instant;

impl App {
    /// Dispatch an engine-mutating [`AppAction`] with the given (already-clamped)
    /// count. Called by the top-level `dispatch_action` for all variants that
    /// directly call into `Editor` or the vim-FSM replay path.
    pub(crate) fn dispatch_engine_action(&mut self, action: AppAction, _count: usize) {
        match action {
            AppAction::DotRepeat {
                count: action_count,
            } => {
                // `.` dot-repeat. Combine pending count prefix with action count.
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.replay_last_change(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::Motion {
                kind,
                count: action_count,
            } => {
                // Use buffered count-prefix if present, otherwise the action count.
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.apply_motion(kind, n);
            }
            AppAction::VisualOp {
                op,
                count: action_count,
            } => {
                // Use buffered count-prefix if present, otherwise the action default.
                let n = self.pending_count.take_or(action_count) as usize;
                // Resolve the active visual range from the engine. The RangeKind
                // must match the visual mode so the range-mutation primitives apply
                // the correct inclusion semantics.
                //
                // Phase 4e follow-ups: all three visual modes now route through the
                // public range-mutation primitives rather than falling back to the
                // engine FSM:
                //   - Visual: pending_register now read from engine getter (gap fixed)
                //   - VisualLine: guard fix in run_operator_over_range allows single-
                //     row linewise, so FSM fallback for d/y/c is removed
                //   - VisualBlock: delete_block / yank_block / change_block / indent_block
                //     now exposed (gap fixed), FSM fallback removed
                use hjkl_engine::{RangeKind, VimMode};
                let vim_mode = self.active().editor.vim_mode();
                // Read the user's pending register selection BEFORE the match so all
                // three mode arms can use it. pending_register() does not clear the
                // selection — the engine clears it when the next operator fires.
                let register = self.active().editor.pending_register().unwrap_or('"');
                match vim_mode {
                    VimMode::VisualBlock => {
                        // Rectangular selection — use block-shape primitives.
                        let Some((top_row, bot_row, left_col, right_col)) =
                            self.active().editor.block_highlight()
                        else {
                            return;
                        };
                        match op {
                            hjkl_vim::OperatorKind::Delete => {
                                self.active_mut()
                                    .editor
                                    .delete_block(top_row, bot_row, left_col, right_col, register);
                            }
                            hjkl_vim::OperatorKind::Yank => {
                                self.active_mut()
                                    .editor
                                    .yank_block(top_row, bot_row, left_col, right_col, register);
                            }
                            hjkl_vim::OperatorKind::Change => {
                                self.active_mut()
                                    .editor
                                    .change_block(top_row, bot_row, left_col, right_col, register);
                                // change_block enters Insert (BlockChange reason);
                                // no Esc needed.
                                return;
                            }
                            hjkl_vim::OperatorKind::Indent => {
                                self.active_mut()
                                    .editor
                                    .indent_block(top_row, bot_row, left_col, right_col, n as i32);
                            }
                            hjkl_vim::OperatorKind::Outdent => {
                                self.active_mut().editor.indent_block(
                                    top_row,
                                    bot_row,
                                    left_col,
                                    right_col,
                                    -(n as i32),
                                );
                            }
                            hjkl_vim::OperatorKind::AutoIndent => {
                                // Visual-block =: submit async formatter with the
                                // visual selection row range; fall back to dumb algo.
                                let range = hjkl_mangler::RangeSpec {
                                    start_row: top_row,
                                    end_row: bot_row,
                                };
                                if !self.submit_external_format(Some(range)) {
                                    self.active_mut()
                                        .editor
                                        .auto_indent_range((top_row, 0), (bot_row, 0));
                                    if let Some((top, bot)) =
                                        self.active_mut().editor.take_last_indent_range()
                                    {
                                        self.indent_flash = Some(IndentFlash {
                                            top,
                                            bot,
                                            started_at: Instant::now(),
                                        });
                                    }
                                }
                            }
                            hjkl_vim::OperatorKind::Filter => {
                                // Visual-block !: filter the row range through a shell command.
                                // Exit visual mode first, then open the filter prompt.
                                use crossterm::event::{
                                    KeyCode, KeyEvent as CtKeyEvent, KeyModifiers,
                                };
                                hjkl_vim_tui::handle_key(
                                    &mut self.active_mut().editor,
                                    CtKeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                                );
                                self.open_filter_prompt(top_row, bot_row);
                                return;
                            }
                            _ => return,
                        }
                        // Exit visual mode after the op (except Change above).
                        use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
                        hjkl_vim_tui::handle_key(
                            &mut self.active_mut().editor,
                            CtKeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                        );
                    }
                    VimMode::Visual => {
                        // Charwise visual selection — inclusive on both ends.
                        let Some((start, end)) = self.active().editor.char_highlight() else {
                            return;
                        };
                        let kind = RangeKind::Inclusive;
                        match op {
                            hjkl_vim::OperatorKind::Delete => {
                                self.active_mut()
                                    .editor
                                    .delete_range(start, end, kind, register);
                            }
                            hjkl_vim::OperatorKind::Yank => {
                                self.active_mut()
                                    .editor
                                    .yank_range(start, end, kind, register);
                            }
                            hjkl_vim::OperatorKind::Change => {
                                self.active_mut()
                                    .editor
                                    .change_range(start, end, kind, register);
                                // change_range transitions to Insert via
                                // begin_insert_noundo — no explicit mode-set needed.
                                return;
                            }
                            hjkl_vim::OperatorKind::Indent => {
                                self.active_mut()
                                    .editor
                                    .indent_range(start, end, n as i32, 0);
                            }
                            hjkl_vim::OperatorKind::Outdent => {
                                self.active_mut()
                                    .editor
                                    .indent_range(start, end, -(n as i32), 0);
                            }
                            hjkl_vim::OperatorKind::AutoIndent => {
                                // Visual-charwise =: submit async formatter with the
                                // visual selection row range; fall back to dumb algo.
                                let (min_r, max_r) = (start.0.min(end.0), start.0.max(end.0));
                                let range = hjkl_mangler::RangeSpec {
                                    start_row: min_r,
                                    end_row: max_r,
                                };
                                if !self.submit_external_format(Some(range)) {
                                    self.active_mut().editor.auto_indent_range(start, end);
                                    if let Some((top, bot)) =
                                        self.active_mut().editor.take_last_indent_range()
                                    {
                                        self.indent_flash = Some(IndentFlash {
                                            top,
                                            bot,
                                            started_at: Instant::now(),
                                        });
                                    }
                                }
                            }
                            hjkl_vim::OperatorKind::Filter => {
                                // Visual-charwise !: filter the row range through a shell command.
                                let (min_r, max_r) = (start.0.min(end.0), start.0.max(end.0));
                                // Exit visual mode first, then open the filter prompt.
                                use crossterm::event::{
                                    KeyCode, KeyEvent as CtKeyEvent, KeyModifiers,
                                };
                                hjkl_vim_tui::handle_key(
                                    &mut self.active_mut().editor,
                                    CtKeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                                );
                                self.open_filter_prompt(min_r, max_r);
                                return;
                            }
                            _ => return,
                        }
                        // Exit visual mode after the op (except Change, which already
                        // transitioned to Insert above).
                        use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
                        hjkl_vim_tui::handle_key(
                            &mut self.active_mut().editor,
                            CtKeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                        );
                    }
                    VimMode::VisualLine => {
                        // Linewise visual selection — full rows.
                        // Option (a): pass (top_row, 0) and (bot_row, usize::MAX)
                        // with RangeKind::Linewise. The engine's run_operator_over_range
                        // handles Linewise semantics; read_vim_range / cut_vim_range
                        // snap to full line boundaries regardless of the col values.
                        // The Phase 4e guard fix allows single-row (top==bot) Linewise
                        // ranges, so this path works for both single and multi-line.
                        let Some((top_row, bot_row)) = self.active().editor.line_highlight() else {
                            return;
                        };
                        let kind = RangeKind::Linewise;
                        match op {
                            hjkl_vim::OperatorKind::Delete => {
                                self.active_mut().editor.delete_range(
                                    (top_row, 0),
                                    (bot_row, usize::MAX),
                                    kind,
                                    register,
                                );
                            }
                            hjkl_vim::OperatorKind::Yank => {
                                self.active_mut().editor.yank_range(
                                    (top_row, 0),
                                    (bot_row, usize::MAX),
                                    kind,
                                    register,
                                );
                            }
                            hjkl_vim::OperatorKind::Change => {
                                self.active_mut().editor.change_range(
                                    (top_row, 0),
                                    (bot_row, usize::MAX),
                                    kind,
                                    register,
                                );
                                // change_range enters Insert mode.
                                return;
                            }
                            hjkl_vim::OperatorKind::Indent => {
                                self.active_mut().editor.indent_range(
                                    (top_row, 0),
                                    (bot_row, 0),
                                    n as i32,
                                    0,
                                );
                            }
                            hjkl_vim::OperatorKind::Outdent => {
                                self.active_mut().editor.indent_range(
                                    (top_row, 0),
                                    (bot_row, 0),
                                    -(n as i32),
                                    0,
                                );
                            }
                            hjkl_vim::OperatorKind::AutoIndent => {
                                // Visual-line =: submit async formatter with the
                                // visual selection row range; fall back to dumb algo.
                                let range = hjkl_mangler::RangeSpec {
                                    start_row: top_row,
                                    end_row: bot_row,
                                };
                                if !self.submit_external_format(Some(range)) {
                                    self.active_mut()
                                        .editor
                                        .auto_indent_range((top_row, 0), (bot_row, 0));
                                    if let Some((top, bot)) =
                                        self.active_mut().editor.take_last_indent_range()
                                    {
                                        self.indent_flash = Some(IndentFlash {
                                            top,
                                            bot,
                                            started_at: Instant::now(),
                                        });
                                    }
                                }
                            }
                            hjkl_vim::OperatorKind::Filter => {
                                // Visual-line !: filter the row range through a shell command.
                                // Exit visual mode first, then open the filter prompt.
                                use crossterm::event::{
                                    KeyCode, KeyEvent as CtKeyEvent, KeyModifiers,
                                };
                                hjkl_vim_tui::handle_key(
                                    &mut self.active_mut().editor,
                                    CtKeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                                );
                                self.open_filter_prompt(top_row, bot_row);
                                return;
                            }
                            _ => return,
                        }
                        // Exit visual mode after the op (except Change above).
                        use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
                        hjkl_vim_tui::handle_key(
                            &mut self.active_mut().editor,
                            CtKeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                        );
                    }
                    _ => {
                        // Not in a visual mode — keymap bound VisualOp but
                        // engine is in Normal/Insert/etc. Shouldn't happen;
                        // bail silently.
                    }
                }
            }
            // ── Phase 6.4: insert-mode entry ──────────────────────────────
            AppAction::EnterInsertI {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.enter_insert_i(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::EnterInsertShiftI {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.enter_insert_shift_i(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::EnterInsertA {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.enter_insert_a(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::EnterInsertShiftA {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.enter_insert_shift_a(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::EnterInsertO {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.open_line_below(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::EnterInsertShiftO {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.open_line_above(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::EnterReplace {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.enter_replace_mode(n.max(1));
                self.sync_after_engine_mutation();
            }

            // ── Phase 6.4: char / line mutation ops ───────────────────────
            AppAction::DeleteCharForward {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.delete_char_forward(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::DeleteCharBackward {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.delete_char_backward(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::SubstituteChar {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.substitute_char(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::SubstituteLine {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.substitute_line(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::DeleteToEol => {
                self.pending_count.reset();
                self.active_mut().editor.delete_to_eol();
                self.sync_after_engine_mutation();
            }
            AppAction::ChangeToEol => {
                self.pending_count.reset();
                self.active_mut().editor.change_to_eol();
                self.sync_after_engine_mutation();
            }
            AppAction::YankToEol {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.yank_to_eol(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::JoinLine {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                // Vim join default is 2 (join current + 1 following line).
                self.active_mut().editor.join_line(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::ToggleCase {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.toggle_case_at_cursor(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::PasteAfter {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.paste_after(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::PasteBefore {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.paste_before(n.max(1));
                self.sync_after_engine_mutation();
            }

            // ── Phase 6.4: undo / redo ────────────────────────────────────
            AppAction::Undo => {
                self.pending_count.reset();
                self.active_mut().editor.undo();
                self.sync_after_engine_mutation();
            }
            AppAction::Redo => {
                self.pending_count.reset();
                self.active_mut().editor.redo();
                self.sync_after_engine_mutation();
            }

            // ── Phase 6.4: jumplist ───────────────────────────────────────
            AppAction::JumpBack {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.jump_back(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::JumpForward {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.jump_forward(n.max(1));
                self.sync_after_engine_mutation();
            }

            // ── Phase 6.4: scroll ops ──────────────────────────────────────
            AppAction::ScrollFullPage {
                dir,
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.scroll_full_page(dir, n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::ScrollHalfPage {
                dir,
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.scroll_half_page(dir, n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::ScrollLine {
                dir,
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.scroll_line(dir, n.max(1));
                self.sync_after_engine_mutation();
            }

            // ── Phase 6.4: search repeat ───────────────────────────────────
            AppAction::SearchRepeat {
                forward,
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.search_repeat(forward, n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::WordSearch {
                forward,
                whole_word,
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut()
                    .editor
                    .word_search(forward, whole_word, n.max(1));
                self.sync_after_engine_mutation();
            }

            // ── Phase 6.4: visual entry / toggle ──────────────────────────
            AppAction::EnterVisualChar => {
                self.pending_count.reset();
                self.active_mut().editor.enter_visual_char();
            }
            AppAction::EnterVisualLine => {
                self.pending_count.reset();
                self.active_mut().editor.enter_visual_line();
            }
            AppAction::EnterVisualBlock => {
                self.pending_count.reset();
                self.active_mut().editor.enter_visual_block();
            }
            AppAction::ReenterLastVisual => {
                self.pending_count.reset();
                self.active_mut().editor.reenter_last_visual();
                self.sync_viewport_from_editor();
            }
            AppAction::VisualToggleAnchor => {
                self.pending_count.reset();
                self.active_mut().editor.visual_o_toggle();
                self.sync_viewport_from_editor();
            }

            AppAction::Replay { keys, recursive } => {
                if recursive {
                    // Re-feed each key through the chord FSM. The queue is
                    // processed FIFO so we use a VecDeque.
                    //
                    // Two guards against runaway recursion:
                    //   - `steps` caps the queue iteration count per frame —
                    //     catches horizontal cycles (`:nmap a bbbbb…` etc).
                    //   - `replay_depth` caps re-entrant dispatch_action stack
                    //     depth — catches vertical cycles (`:nmap a a`) which
                    //     would otherwise stack-overflow.
                    use std::collections::VecDeque;
                    const MAX_STEPS: usize = 1024;
                    // Vertical recursion depth cap. Sized to fit comfortably
                    // within macOS's 512 KB per-thread stack default (cargo
                    // nextest spawns tests on non-main threads): each frame
                    // of this arm carries a VecDeque, sub_replay Vec, and the
                    // recursive call into dispatch_action. 128 frames is far
                    // beyond any realistic nested-map depth and leaves plenty
                    // of stack headroom on all platforms.
                    const MAX_DEPTH: usize = 128;
                    if self.replay_depth >= MAX_DEPTH {
                        self.bus.error("E223: recursive mapping (depth limit)");
                        return;
                    }
                    self.replay_depth += 1;
                    let mut queue: VecDeque<hjkl_keymap::KeyEvent> = keys.into();
                    let mut steps = 0usize;
                    while let Some(ev) = queue.pop_front() {
                        steps += 1;
                        if steps > MAX_STEPS {
                            self.bus.error("E223: recursive mapping (1024-step limit)");
                            break;
                        }
                        let mode = super::current_km_mode(self);
                        let Some(mode) = mode else {
                            continue;
                        };
                        let mut sub_replay = Vec::new();
                        let consumed = self.dispatch_keymap_in_mode(ev, 1, &mut sub_replay, mode);
                        if !consumed && sub_replay.len() <= 1 {
                            self.replay_km_events_to_engine(&sub_replay);
                        }
                    }
                    self.replay_depth -= 1;
                } else {
                    // Non-recursive: bypass the trie and go straight to the engine.
                    for ev in keys {
                        self.replay_km_events_to_engine(std::slice::from_ref(&ev));
                    }
                }
            }

            // Any non-engine action routed here is a logic error — ignore silently.
            _ => {}
        }
    }
}
