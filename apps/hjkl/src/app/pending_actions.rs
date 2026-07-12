//! Pending-state sub-dispatcher for `App::dispatch_action`.
//!
//! Handles variants that set up `App::pending_state` for a two-key chord:
//!   - BeginPendingReplace
//!   - BeginPendingFind
//!   - BeginPendingAfterG
//!   - BeginPendingAfterZ
//!   - BeginPendingAfterOp
//!   - BeginPendingSelectRegister
//!   - BeginPendingSetMark
//!   - BeginPendingGotoMarkLine
//!   - BeginPendingGotoMarkChar
//!   - QChord
//!   - BeginPendingPlayMacro

use crate::keymap_actions::AppAction;

use super::App;
use hjkl_vim::VimEditorExt;

impl App {
    /// Dispatch a pending-state-initiating [`AppAction`].
    ///
    /// Each arm stores a `PendingState` variant into `self.pending_state`.
    /// The next key is then routed through `hjkl_vim::step` in
    /// `route_chord_key_inner` instead of the trie or engine FSM.
    pub(crate) fn dispatch_pending_state_action(&mut self, action: AppAction) {
        match action {
            AppAction::BeginPendingReplace {
                count: action_count,
            } => {
                // Use buffered count-prefix if present, otherwise the action count.
                let n = self.pending_count.take_or(action_count) as usize;
                self.pending_state = Some(hjkl_vim::PendingState::Replace { count: n });
            }
            AppAction::BeginPendingFind {
                forward,
                till,
                count: action_count,
            } => {
                // Use buffered count-prefix if present, otherwise the action count.
                let n = self.pending_count.take_or(action_count) as usize;
                self.pending_state = Some(hjkl_vim::PendingState::Find {
                    count: n,
                    forward,
                    till,
                });
            }
            AppAction::BeginPendingAfterG {
                count: action_count,
            } => {
                // Use buffered count-prefix if present, otherwise the action count.
                let n = self.pending_count.take_or(action_count) as usize;
                self.pending_state = Some(hjkl_vim::PendingState::AfterG { count: n });
            }
            AppAction::BeginPendingAfterZ {
                count: action_count,
            } => {
                // Use buffered count-prefix if present, otherwise the action count.
                let n = self.pending_count.take_or(action_count) as usize;
                self.pending_state = Some(hjkl_vim::PendingState::AfterZ { count: n });
            }
            AppAction::BeginPendingAfterOp {
                op,
                count1: action_count,
            } => {
                // Use buffered count-prefix if present, otherwise the action count.
                let n = self.pending_count.take_or(action_count) as usize;
                self.pending_state = Some(hjkl_vim::PendingState::AfterOp {
                    op,
                    count1: n,
                    inner_count: 0,
                });
            }
            AppAction::BeginPendingSelectRegister => {
                // `"<reg>` register-prefix chord. The register char is captured
                // by the second key. Do NOT reset pending_count here — a count
                // typed before `"` (e.g. `5"add`) must survive through register
                // selection so the subsequent operator (`d`) can consume it.
                // Example: `5"add` → pending_count=5, `"` → SelectRegister (count
                // preserved), `a` → SetPendingRegister, `dd` → delete 5 lines
                // into register `a`.
                self.pending_state = Some(hjkl_vim::PendingState::SelectRegister);
            }
            AppAction::BeginPendingSetMark => {
                // `m<x>` mark-set chord. No count consumed — char captured by
                // second key. Discard any buffered count (not meaningful here).
                self.pending_count.reset();
                self.pending_state = Some(hjkl_vim::PendingState::SetMark);
            }
            AppAction::BeginPendingGotoMarkLine => {
                // `'<x>` mark-goto-line chord. No count consumed.
                self.pending_count.reset();
                self.pending_state = Some(hjkl_vim::PendingState::GotoMarkLine);
            }
            AppAction::BeginPendingGotoMarkChar => {
                // `` `<x> `` mark-goto-char chord. No count consumed.
                self.pending_count.reset();
                self.pending_state = Some(hjkl_vim::PendingState::GotoMarkChar);
            }
            AppAction::QChord { .. } => {
                // `q` in Normal mode. Branch: stop recording if active, else
                // open the RecordMacroTarget chord to wait for the register char.
                self.pending_count.reset();
                if self.active_editor().is_recording_macro() {
                    // Bare `q` ends the active recording.
                    self.active_editor_mut().stop_macro_record();
                } else {
                    self.pending_state = Some(hjkl_vim::PendingState::RecordMacroTarget);
                }
            }
            AppAction::BeginPendingPlayMacro {
                count: action_count,
            } => {
                // `@` in Normal mode. Capture count and wait for register char.
                let n = self.pending_count.take_or(action_count) as usize;
                self.pending_state =
                    Some(hjkl_vim::PendingState::PlayMacroTarget { count: n.max(1) });
            }

            // Any non-pending-state action routed here is a logic error — ignore silently.
            _ => return,
        }
        // Arm the which-key idle timer so the popup can fire after
        // `which_key_delay` while the user contemplates the second key
        // of an engine-FSM chord (g/z/d/y/c/etc.). Without this the
        // popup only ever fires for app_keymap trie prefixes.
        self.note_prefix_set();
    }
}
