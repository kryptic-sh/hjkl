//! App-level chord and action dispatch — `dispatch_action`, `dispatch_keymap`, and chord timeout.

use super::App;
use super::keymap;

impl App {
    /// Dispatch an [`crate::keymap_actions::AppAction`] with an optional repeat count.
    ///
    /// This is the single authoritative dispatch site for all chord-triggered
    /// app actions. Routing by domain — each cluster delegates to a focused
    /// sub-dispatcher that lives in the corresponding glue module:
    ///   - picker opens    → inline (3 one-liners)
    ///   - git actions     → `picker_glue::dispatch_git_action`
    ///   - LSP actions     → `lsp_glue::dispatch_lsp_action`
    ///   - window actions  → `window::dispatch_window_action` (incl. TmuxNavigate)
    ///   - buffer actions  → `buffer_ops::dispatch_buffer_action`
    ///   - prompt actions  → `prompt::dispatch_prompt_action`
    ///   - pending-state   → `pending_actions::dispatch_pending_state_action`
    ///   - engine actions  → `engine_actions::dispatch_engine_action`
    ///   - QuitOrClose     → inline (app-lifecycle, 5 LOC)
    pub fn dispatch_action(&mut self, action: crate::keymap_actions::AppAction, count: u32) {
        use crate::keymap_actions::AppAction;
        let count = count.max(1) as usize;
        match action {
            // ── File / buffer pickers (open) ───────────────────────────────
            AppAction::OpenFilePicker => self.open_picker(),
            AppAction::OpenBufferPicker => self.open_buffer_picker(),
            AppAction::OpenGrepPicker => self.open_grep_picker(None),
            AppAction::ToggleExplorer => self.toggle_explorer(),

            // ── Git picker openers ─────────────────────────────────────────
            AppAction::GitStatus
            | AppAction::GitLog
            | AppAction::GitBranch
            | AppAction::GitFileHistory
            | AppAction::GitStashes
            | AppAction::GitTags
            | AppAction::GitRemotes
            | AppAction::GitBlameLine => self.dispatch_git_action(action),

            // ── LSP + diagnostic navigation ────────────────────────────────
            AppAction::ShowDiagAtCursor
            | AppAction::LspCodeActions
            | AppAction::LspRename
            | AppAction::LspGotoDef
            | AppAction::LspGotoDecl
            | AppAction::LspGotoRef
            | AppAction::LspGotoImpl
            | AppAction::LspGotoTypeDef
            | AppAction::LspHover
            | AppAction::DiagNext
            | AppAction::DiagPrev
            | AppAction::DiagNextError
            | AppAction::DiagPrevError => self.dispatch_lsp_action(action),

            // ── Window / layout management ─────────────────────────────────
            AppAction::FocusLeft
            | AppAction::FocusBelow
            | AppAction::FocusAbove
            | AppAction::FocusRight
            | AppAction::FocusNext
            | AppAction::FocusPrev
            | AppAction::CloseFocusedWindow
            | AppAction::OnlyFocusedWindow
            | AppAction::SwapWithSibling
            | AppAction::MoveWindowToNewTab
            | AppAction::NewSplit
            | AppAction::ResizeHeight(_)
            | AppAction::ResizeWidth(_)
            | AppAction::EqualizeLayout
            | AppAction::MaximizeHeight
            | AppAction::MaximizeWidth
            | AppAction::TmuxNavigate(_) => self.dispatch_window_action(action, count),

            // ── Buffer / tab navigation ────────────────────────────────────
            AppAction::Tabnext
            | AppAction::Tabprev
            | AppAction::BufferNext
            | AppAction::BufferPrev
            | AppAction::BufferAlt
            | AppAction::BufferCycleH
            | AppAction::BufferCycleL => self.dispatch_buffer_action(action, count),

            // ── Prompt / overlay entry ─────────────────────────────────────
            AppAction::OpenCommandPrompt | AppAction::OpenSearchPrompt(_) => {
                self.dispatch_prompt_action(action)
            }

            // ── Pending-state chords ───────────────────────────────────────
            AppAction::BeginPendingReplace { .. }
            | AppAction::BeginPendingFind { .. }
            | AppAction::BeginPendingAfterG { .. }
            | AppAction::BeginPendingAfterZ { .. }
            | AppAction::BeginPendingAfterOp { .. }
            | AppAction::BeginPendingSelectRegister
            | AppAction::BeginPendingSetMark
            | AppAction::BeginPendingGotoMarkLine
            | AppAction::BeginPendingGotoMarkChar
            | AppAction::QChord { .. }
            | AppAction::BeginPendingPlayMacro { .. } => self.dispatch_pending_state_action(action),

            // ── App lifecycle ──────────────────────────────────────────────
            AppAction::QuitOrClose => {
                if self.layout().leaves().len() > 1 {
                    self.close_focused_window();
                } else {
                    self.exit_requested = true;
                }
            }

            // ── Command-line window (issue #37) ────────────────────────────
            AppAction::OpenCmdLineWindow(kind) => self.open_cmdline_window(kind.into(), None),

            // ── Engine-mutating actions ────────────────────────────────────
            _ => self.dispatch_engine_action(action, count),
        }
    }

    /// Feed a crossterm key event through the app-level chord keymap and
    /// dispatch any resolved action. Returns `true` if the key was consumed
    /// (either resolved or still pending), `false` if the keymap returned
    /// `Unbound` and the caller should replay the events to the engine.
    ///
    /// Replayed events are stored in `out_replay` (never `None`-cleared).
    ///
    /// This is a thin shim over [`dispatch_keymap_in_mode`] fixed to Normal mode.
    pub fn dispatch_keymap(
        &mut self,
        km_ev: hjkl_keymap::KeyEvent,
        count: u32,
        out_replay: &mut Vec<hjkl_keymap::KeyEvent>,
    ) -> bool {
        self.dispatch_keymap_in_mode(km_ev, count, out_replay, keymap::HjklMode::Normal)
    }

    /// Mode-generalized chord dispatch. Feed `km_ev` into the trie for `mode`
    /// and dispatch any resolved action.
    ///
    /// Returns `true` if consumed (Pending / Ambiguous / Match),
    /// `false` if Unbound (events stored in `out_replay`).
    pub fn dispatch_keymap_in_mode(
        &mut self,
        km_ev: hjkl_keymap::KeyEvent,
        count: u32,
        out_replay: &mut Vec<hjkl_keymap::KeyEvent>,
        mode: keymap::HjklMode,
    ) -> bool {
        use hjkl_keymap::KeyResolve;
        let now = std::time::Instant::now();
        match self.app_keymap.feed(mode, km_ev, now) {
            KeyResolve::Pending => {
                self.note_prefix_set();
                true
            }
            KeyResolve::Ambiguous => {
                self.note_prefix_set();
                true
            }
            KeyResolve::Match(binding) => {
                self.clear_prefix_state();
                self.dispatch_action(binding.action, count);
                true
            }
            KeyResolve::Unbound(events) => {
                self.clear_prefix_state();
                out_replay.extend(events);
                false
            }
        }
    }

    /// Force-resolve a pending chord buffer after the keymap timeout has
    /// elapsed. Called from the event loop's poll-timeout branch when a chord
    /// is pending (typically `Ambiguous`: e.g. both `g` and `gd` bound — the
    /// shorter binding fires after `timeoutlen`).
    ///
    /// Returns:
    /// - `Some(events)` to be replayed to the engine for `Unbound` with
    ///   drained events (real dead-end case).
    /// - `Some(empty)` after a `Match` (the action was already dispatched).
    /// - `None` when the buffer was empty OR when the buffer is a pure prefix
    ///   (user is mid-chord and `timeout_resolve` left the buffer in place —
    ///   needed so the which-key popup stays visible past the timeout).
    pub fn resolve_chord_timeout(
        &mut self,
        mode: keymap::HjklMode,
    ) -> Option<Vec<hjkl_keymap::KeyEvent>> {
        use hjkl_keymap::KeyResolve;
        if self.app_keymap.pending(mode).is_empty() {
            return None;
        }
        match self.app_keymap.timeout_resolve(mode) {
            KeyResolve::Match(binding) => {
                self.clear_prefix_state();
                self.dispatch_action(binding.action, 1);
                Some(Vec::new())
            }
            KeyResolve::Unbound(events) if events.is_empty() => {
                // Pure-prefix: timeout_resolve was a no-op. Keep prefix state
                // alive so the which-key popup stays visible.
                None
            }
            KeyResolve::Unbound(events) => {
                self.clear_prefix_state();
                Some(events)
            }
            // timeout_resolve only returns Match or Unbound; defensive fallthrough.
            _ => None,
        }
    }
}
