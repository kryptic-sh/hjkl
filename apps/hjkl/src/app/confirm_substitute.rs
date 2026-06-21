//! Interactive `:s/pat/rep/c` confirm-mode handler.
//!
//! When [`App::confirming_substitute`] is `Some`, key presses are routed here
//! instead of the editor engine. The user is prompted once per candidate match:
//!
//! - `y` — accept current match, advance.
//! - `n` — skip current match, advance.
//! - `a` — accept this and all remaining; finish.
//! - `q` / `Esc` — abort (keep any already-accepted matches).
//! - `l` — accept current then finish ("last").

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hjkl_engine::{Query, apply_collected_matches};

use super::{App, ConfirmingSubstitute};

impl App {
    /// Route a keypress to the confirm-substitute prompt.
    ///
    /// Called from `handle_keypress` when `confirming_substitute.is_some()`.
    /// Returns `true` when the key was consumed (always true for this handler).
    pub(crate) fn handle_confirm_substitute_key(&mut self, key: KeyEvent) -> bool {
        let action = match (key.code, key.modifiers) {
            (KeyCode::Char('y'), KeyModifiers::NONE) => ConfirmAction::Accept,
            (KeyCode::Char('n'), KeyModifiers::NONE) => ConfirmAction::Skip,
            (KeyCode::Char('a'), KeyModifiers::NONE) => ConfirmAction::AcceptAll,
            (KeyCode::Char('l'), KeyModifiers::NONE) => ConfirmAction::Last,
            (KeyCode::Char('q'), KeyModifiers::NONE) | (KeyCode::Esc, _) => ConfirmAction::Quit,
            _ => {
                // Any other key: re-show the prompt and consume the key.
                return true;
            }
        };

        // Borrow the session state.
        let cs = match self.confirming_substitute.as_mut() {
            Some(s) => s,
            None => return true,
        };

        match action {
            ConfirmAction::Accept => {
                cs.accepted[cs.idx] = true;
                advance_or_finish(cs);
            }
            ConfirmAction::Skip => {
                advance_or_finish(cs);
            }
            ConfirmAction::AcceptAll => {
                let idx = cs.idx;
                for a in &mut cs.accepted[idx..] {
                    *a = true;
                }
                // Mark as done by pushing idx past end.
                cs.idx = cs.matches.len();
            }
            ConfirmAction::Last => {
                cs.accepted[cs.idx] = true;
                cs.idx = cs.matches.len(); // Force finish.
            }
            ConfirmAction::Quit => {
                cs.idx = cs.matches.len(); // Force finish.
            }
        }

        let done = self
            .confirming_substitute
            .as_ref()
            .is_none_or(|cs| cs.idx >= cs.matches.len());
        if done {
            // Session is done — apply accepted matches.
            self.finish_confirm_substitute();
        } else {
            // Advance cursor to next match.
            self.jump_to_current_confirm_match();
        }

        true
    }

    /// Apply all accepted matches and clear the session.
    fn finish_confirm_substitute(&mut self) {
        let cs = match self.confirming_substitute.take() {
            Some(s) => s,
            None => return,
        };
        let accepted_count = cs.accepted.iter().filter(|&&b| b).count();
        if accepted_count == 0 {
            self.bus.info("0 substitutions on 0 lines");
            return;
        }

        let idx = self.focused_slot_idx();
        self.slots[idx].editor.push_undo();

        let applied =
            apply_collected_matches(&mut self.slots[idx].editor, &cs.matches, &cs.accepted);

        // Count distinct lines changed.
        let lines_changed = {
            let mut rows: Vec<u32> = cs
                .matches
                .iter()
                .zip(cs.accepted.iter())
                .filter_map(|(m, &ok)| if ok { Some(m.row) } else { None })
                .collect();
            rows.sort_unstable();
            rows.dedup();
            rows.len()
        };

        // Propagate dirty state through the usual pipeline.
        if self.slots[idx].editor.take_dirty() {
            let elapsed = self.slots[idx].refresh_dirty_against_saved();
            self.last_signature_us = elapsed;
            let buffer_id = self.slots[idx].buffer_id;
            if self.slots[idx].editor.take_content_reset() {
                self.syntax.reset(buffer_id);
            }
            let edits = self.slots[idx].editor.take_content_edits();
            if !edits.is_empty() {
                self.syntax.apply_edits(buffer_id, &edits);
            }
            self.recompute_and_install();
        }

        self.bus
            .info(format!("{applied} substitutions on {lines_changed} lines"));
    }

    /// Move the cursor to the current confirm match and sync the viewport.
    pub(crate) fn jump_to_current_confirm_match(&mut self) {
        let (row, col) = match self.confirming_substitute.as_ref() {
            Some(cs) if cs.idx < cs.matches.len() => {
                let m = &cs.matches[cs.idx];
                let r = m.row as usize;
                let col = {
                    let rope = Query::rope(self.active_editor().buffer());
                    let line = hjkl_buffer::rope_line_str(&rope, r);
                    line[..m.byte_start as usize].chars().count()
                };
                (r, col)
            }
            _ => return,
        };
        self.active_editor_mut().jump_cursor(row, col);
        self.sync_after_engine_mutation();
    }
}

/// Which action the user took for the current match.
enum ConfirmAction {
    Accept,
    Skip,
    AcceptAll,
    Last,
    Quit,
}

/// Advance `cs.idx` to the next match, or mark session as done.
fn advance_or_finish(cs: &mut ConfirmingSubstitute) {
    cs.idx += 1;
    // idx >= len signals "done" — checked by the caller.
}
