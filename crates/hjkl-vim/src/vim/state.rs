//! Vim FSM: state.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{
    InsertSession, LastChange, LastHorizontalMotion, LastVisual, Mode, Operator, Pending,
};

use hjkl_engine::VimMode;

/// Vim caps count prefixes at 999,999,999 (`:h count`); mirror that cap on
/// every value that can feed a `0..count` apply loop so a pathological digit
/// stream (`<20 nines>x`) saturating toward `usize::MAX` can't freeze the
/// editor. Matches `CountAccumulator::MAX_COUNT` in hjkl-vim.
pub const MAX_COUNT: usize = 999_999_999;
/// ROT13 a string: rotate ASCII letters by 13, leave everything else.
pub(crate) fn rot13_str(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a' + 13) % 26) + b'a') as char,
            'A'..='Z' => (((c as u8 - b'A' + 13) % 26) + b'A') as char,
            _ => c,
        })
        .collect()
}
#[derive(Debug, Default)]
pub struct VimState {
    /// Internal FSM mode. Kept in sync with `current_mode` after every
    /// `step`. Phase 6.6b: promoted from private to `pub` so the FSM
    /// body (moving to hjkl-vim in 6.6c–6.6g) can read/write it directly
    /// until the migration is complete.
    pub mode: Mode,
    /// Two-key chord in progress. `Pending::None` when idle.
    pub pending: Pending,
    /// Digit prefix accumulated before an operator or motion. `0` means
    /// no prefix was typed (treated as 1 by most commands).
    pub count: usize,
    /// Last `f`/`F`/`t`/`T` target, for `;` / `,` repeat.
    pub last_find: Option<(char, bool, bool)>,
    /// Transient: set while resolving a `;` / `,` repeat so the following
    /// `t`/`T` find skips an immediately-adjacent match (vim's repeat quirk).
    /// Read and cleared by the `Motion::Find` cursor dispatch.
    pub find_repeat_skip: bool,
    /// Most-recent mutating command for `.` dot-repeat.
    pub last_change: Option<LastChange>,
    /// Captured on insert-mode entry: count, buffer snapshot, entry kind.
    pub insert_session: Option<InsertSession>,
    /// (row, col) anchor for char-wise Visual mode. Set on entry, used
    /// to compute the highlight range and the operator range without
    /// relying on tui-textarea's live selection.
    pub visual_anchor: (usize, usize),
    /// Row anchor for VisualLine mode.
    pub visual_line_anchor: usize,
    /// (row, col) anchor for VisualBlock mode. The live cursor is the
    /// opposite corner.
    pub block_anchor: (usize, usize),
    /// Intended "virtual" column for the block's active corner. j/k
    /// clamp cursor.col to shorter rows, which would collapse the
    /// block across ragged content — so we remember the desired column
    /// separately and use it for block bounds / insert-column
    /// computations. Updated by h/l only.
    pub block_vcol: usize,
    /// Track whether the last yank/cut was linewise (drives `p`/`P` layout).
    /// Active register selector — set by `"reg` prefix, consumed by
    /// the next y / d / c / p. `None` falls back to the unnamed `"`.
    pub pending_register: Option<char>,
    /// Recording target — set by `q{reg}`, cleared by a bare `q`.
    /// While `Some`, every consumed `Input` is appended to
    /// `recording_keys`.
    pub recording_macro: Option<char>,
    /// Keys recorded into the in-progress macro. On `q` finish, these
    /// are encoded via [`hjkl_engine::input::encode_macro`] and written to
    /// the matching named register slot, so macros and yanks share a
    /// single store.
    pub recording_keys: Vec<hjkl_engine::input::Input>,
    /// Set during `@reg` replay so the recorder doesn't capture the
    /// replayed keystrokes a second time.
    pub replaying_macro: bool,
    /// Last register played via `@reg`. `@@` re-plays this one.
    pub last_macro: Option<char>,
    /// Position where the cursor was when insert mode last exited (Esc).
    /// Used by `gi` to return to the exact (row, col) where the user
    /// last typed, matching vim's `:h gi`.
    pub last_insert_pos: Option<(usize, usize)>,
    /// Snapshot of the last visual selection for `gv` re-entry.
    /// Stored on every Visual / VisualLine / VisualBlock exit.
    pub last_visual: Option<LastVisual>,
    /// `zz` / `zt` / `zb` set this so the end-of-step scrolloff
    /// pass doesn't override the user's explicit viewport pinning.
    /// Cleared every step.
    /// Set by the 7 smooth-scrollable motions (C-d/u/f/b, zz/zt/zb) so the
    /// app can animate the viewport jump. Drained via Editor::take_scroll_anim_hint.
    /// Set while replaying `.` / last-change so we don't re-record it.
    pub replaying: bool,
    /// Entered Normal from Insert via `Ctrl-o`; after the next complete
    /// normal-mode command we return to Insert.
    pub one_shot_normal: bool,
    /// Live `/` or `?` prompt. `None` outside search-prompt mode.
    /// Most recent committed search pattern. Surfaced to host apps via
    /// [`Editor::last_search`] so their status line can render a hint
    /// and so `n` / `N` have something to repeat.
    /// Direction of the last committed search. `n` repeats this; `N`
    /// inverts it. Defaults to forward so a never-searched buffer's
    /// `n` still walks downward.
    /// Text of the most recent insert session — vim's `".` register, pasted
    /// via `<C-r>.` in insert mode (and `".p` in normal mode).
    pub last_insert_text: Option<String>,
    /// Back half of the jumplist — `Ctrl-o` pops from here. Populated
    /// with the pre-motion cursor when a "big jump" motion fires
    /// (`gg`/`G`, `%`, `*`/`#`, `n`/`N`, `H`/`M`/`L`, committed `/` or
    /// `?`). Capped at 100 entries.
    /// Forward half — `Ctrl-i` pops from here. Cleared by any new big
    /// jump, matching vim's "branch off trims forward history" rule.
    /// Set by `Ctrl-R` in insert mode while waiting for the register
    /// selector. The next typed char names the register; its contents
    /// are inserted inline at the cursor and the flag clears.
    pub insert_pending_register: bool,
    /// Stashed start position for the `[` mark on a Change operation.
    /// Set to `top` before the cut in `run_operator_over_range` (Change
    /// arm); consumed by `finish_insert_session` on Esc-from-insert
    /// when the reason is `AfterChange`. Mirrors vim's `:h '[` / `:h ']`
    /// rule that `[` = start of change, `]` = last typed char on exit.
    pub change_mark_start: Option<(usize, usize)>,
    /// Bounded history of committed `/` / `?` search patterns. Newest
    /// entries are at the back; capped at [`SEARCH_HISTORY_MAX`] to
    /// avoid unbounded growth on long sessions.
    /// Index into `search_history` while the user walks past patterns
    /// in the prompt via `Ctrl-P` / `Ctrl-N`. `None` outside that walk
    /// — typing or backspacing in the prompt resets it so the next
    /// `Ctrl-P` starts from the most recent entry again.
    /// Wall-clock instant of the last keystroke. Drives the
    /// `:set timeoutlen` multi-key timeout — if `now() - last_input_at`
    /// exceeds the configured budget, any pending prefix is cleared
    /// before the new key dispatches. `None` before the first key.
    /// 0.0.29 (Patch B): `:set timeoutlen` math now reads
    /// [`hjkl_engine::types::Host::now`] via `last_input_host_at`. This
    /// `Instant`-flavoured field stays for snapshot tests that still
    /// observe it directly.
    /// `Host::now()` reading at the last keystroke. Drives
    /// `:set timeoutlen` so macro replay / headless drivers stay
    /// deterministic regardless of wall-clock skew.
    /// Canonical current mode. Mirrors `mode` (the FSM-internal field)
    /// AND is written by every Phase 6.3 primitive (`set_mode`,
    /// `enter_visual_char_bridge`, …). Once the FSM is gone this is the
    /// sole source of truth; until then both fields are kept in sync.
    /// Initialized to `Normal` via `#[derive(Default)]`.
    pub current_mode: hjkl_engine::VimMode,
    /// Most recent successful :s invocation. Stored so :& / :&& can repeat it.
    /// Stack of auto-inserted closing characters awaiting skip-over.
    ///
    /// Each entry `(row, col, ch)` records where autopair placed a close
    /// character. When the next typed char matches `ch` AND the cursor is
    /// immediately before that position, the engine advances past it
    /// ("skip-over") instead of inserting. The stack is cleared on any
    /// cursor motion, mode change, or out-of-pair edit.
    /// Last sneak digraph and direction: `Some(((c1, c2), forward))`.
    /// Used by `;` / `,` sneak-repeat when `last_horizontal_motion == Sneak`.
    pub last_sneak: Option<((char, char), bool)>,
    /// Tracks which kind of horizontal motion was last performed, so `;` / `,`
    /// can dispatch to sneak-repeat vs. find-char-repeat as appropriate.
    pub last_horizontal_motion: LastHorizontalMotion,
}
impl VimState {
    pub fn public_mode(&self) -> VimMode {
        match self.mode {
            Mode::Normal => VimMode::Normal,
            Mode::Insert => VimMode::Insert,
            Mode::Visual => VimMode::Visual,
            Mode::VisualLine => VimMode::VisualLine,
            Mode::VisualBlock => VimMode::VisualBlock,
        }
    }

    /// Project the current vim mode onto the discipline-agnostic
    /// [`hjkl_engine::CoarseMode`] (the seam app chrome reads — #265 G3 / #267).
    /// Shared by [`hjkl_engine::DisciplineState::coarse_mode`] and
    /// [`hjkl_engine::Editor::coarse_mode`].
    pub fn coarse_mode(&self) -> hjkl_engine::CoarseMode {
        use hjkl_engine::CoarseMode;
        match self.current_mode {
            VimMode::Normal => CoarseMode::Normal,
            VimMode::Insert => CoarseMode::Insert,
            VimMode::Visual => CoarseMode::Select,
            VimMode::VisualLine => CoarseMode::SelectLine,
            VimMode::VisualBlock => CoarseMode::SelectBlock,
        }
    }

    pub fn force_normal(&mut self) {
        self.mode = Mode::Normal;
        self.pending = Pending::None;
        self.count = 0;
        self.insert_session = None;
        // Phase 6.3: keep current_mode in sync for callers that bypass step().
        self.current_mode = hjkl_engine::VimMode::Normal;
    }

    /// Reset every prefix-tracking field so the next keystroke starts
    /// a fresh sequence. Drives `:set timeoutlen` — when the user
    /// pauses past the configured budget, `hjkl_vim::dispatch_input` calls
    /// this before dispatching the new key.
    ///
    /// Resets: `pending`, `count`, `pending_register`,
    /// `insert_pending_register`. Does NOT touch `mode`,
    /// `insert_session`, marks, jump list, or visual anchors —
    /// those aren't part of the in-flight chord.
    pub fn clear_pending_prefix(&mut self) {
        self.pending = Pending::None;
        self.count = 0;
        self.pending_register = None;
        self.insert_pending_register = false;
    }

    /// Widen the active insert session's row window to include `row`. Called
    /// by the Phase 6.1 public `Editor::insert_*` methods after each
    /// mutation so `finish_insert_session` diffs the right range on Esc.
    /// No-op when no insert session is active (e.g. calling from Normal mode).
    pub(crate) fn widen_insert_row(&mut self, row: usize) {
        if let Some(ref mut session) = self.insert_session {
            session.row_min = session.row_min.min(row);
            session.row_max = session.row_max.max(row);
        }
    }

    pub fn is_visual(&self) -> bool {
        matches!(
            self.mode,
            Mode::Visual | Mode::VisualLine | Mode::VisualBlock
        )
    }

    pub fn is_visual_char(&self) -> bool {
        self.mode == Mode::Visual
    }

    /// The pending repeat count (typed digits before a motion/operator),
    /// or `None` when no digits are pending. Zero is treated as absent.
    pub(crate) fn pending_count_val(&self) -> Option<u32> {
        if self.count == 0 {
            None
        } else {
            Some(self.count as u32)
        }
    }

    /// `true` when an in-flight chord is awaiting more keys. Inverse of
    /// `matches!(self.pending, Pending::None)`.
    pub(crate) fn is_chord_pending(&self) -> bool {
        !matches!(self.pending, Pending::None)
    }

    /// Return a single char representing the pending operator, if any.
    /// Used by host apps (status line "showcmd" area) to display e.g.
    /// `d`, `y`, `c` while waiting for a motion.
    pub(crate) fn pending_op_char(&self) -> Option<char> {
        let op = match &self.pending {
            Pending::Op { op, .. }
            | Pending::OpTextObj { op, .. }
            | Pending::OpG { op, .. }
            | Pending::OpFind { op, .. }
            | Pending::OpSquareBracketOpen { op, .. }
            | Pending::OpSquareBracketClose { op, .. } => Some(*op),
            _ => None,
        };
        op.map(|o| match o {
            Operator::Delete => 'd',
            Operator::Change => 'c',
            Operator::Yank => 'y',
            Operator::Uppercase => 'U',
            Operator::Lowercase => 'u',
            Operator::ToggleCase => '~',
            Operator::Indent => '>',
            Operator::Outdent => '<',
            Operator::Fold => 'z',
            Operator::Reflow => 'q',
            Operator::ReflowKeepCursor => 'w',
            Operator::AutoIndent => '=',
            Operator::Filter => '!',
            // `gc` prefix — doubled as `gcc`.
            Operator::Comment => 'c',
            // `g?` prefix — doubled as `g??`.
            Operator::Rot13 => '?',
        })
    }
}
/// The vim FSM state is the vim discipline's [`hjkl_engine::DisciplineState`]: the
/// engine reaches it type-erased and asks only for its [`hjkl_engine::CoarseMode`]
/// (#265 G3 / #267). Until `VimState` physically moves into `hjkl-vim`, this
/// impl lives here alongside the struct.
impl hjkl_engine::DisciplineState for VimState {
    fn coarse_mode(&self) -> hjkl_engine::CoarseMode {
        VimState::coarse_mode(self)
    }
    /// Vim's idle state is Normal mode with no pending chord, count or insert
    /// session — exactly what `force_normal` establishes.
    fn reset_to_idle(&mut self) {
        self.force_normal();
    }
    /// Only the FSM mode — `current_mode`, `pending`, `count` and any open
    /// insert session are deliberately left alone (see the trait docs).
    fn reset_mode_after_history(&mut self) {
        self.mode = Mode::Normal;
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
