//! Application-level actions dispatched from chord bindings.
//!
//! Each variant corresponds to one app-handled binding currently
//! in `event_loop.rs`. The enum is used as the action type for
//! `Keymap<AppAction>`.

/// Search direction for incremental-search prompts (`/` / `?`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDir {
    Forward,
    Backward,
}

/// Cardinal direction for window navigation (`<C-h/j/k/l>` / `TmuxNavigate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavDir {
    Left,
    Down,
    Up,
    Right,
}

/// Every action the app can perform in response to a chord binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppAction {
    // ── File / buffer pickers ──────────────────────────────────────────
    OpenFilePicker,
    OpenBufferPicker,
    OpenGrepPicker,
    /// `<leader>e` — toggle the left file-explorer pane (#55).
    ToggleExplorer,

    // ── Git commands ───────────────────────────────────────────────────
    GitStatus,
    GitLog,
    GitBranch,
    GitFileHistory,
    GitStashes,
    GitTags,
    GitRemotes,
    /// `<leader>gm` — git blame for the cursor line in a popup (#202).
    GitBlameLine,

    // ── LSP ───────────────────────────────────────────────────────────
    ShowDiagAtCursor,
    LspCodeActions,
    LspRename,
    LspGotoDef,
    LspGotoDecl,
    LspGotoRef,
    LspGotoImpl,
    LspGotoTypeDef,

    // ── Tab navigation ─────────────────────────────────────────────────
    Tabnext,
    Tabprev,

    // ── Buffer navigation ──────────────────────────────────────────────
    BufferNext,
    BufferPrev,

    // ── Diagnostic navigation ─────────────────────────────────────────
    DiagNext,
    DiagPrev,
    DiagNextError,
    DiagPrevError,

    // ── Diff-mode change navigation (#208 Phase 2) ─────────────────────
    DiffNextChange,
    DiffPrevChange,

    // ── Window focus ───────────────────────────────────────────────────
    FocusLeft,
    FocusBelow,
    FocusAbove,
    FocusRight,
    FocusNext,
    FocusPrev,

    // ── Window management ─────────────────────────────────────────────
    CloseFocusedWindow,
    OnlyFocusedWindow,
    SwapWithSibling,
    MoveWindowToNewTab,
    NewSplit,

    // ── Window resize ─────────────────────────────────────────────────
    /// Grow height by count (negative = shrink).
    ResizeHeight(i32),
    /// Grow width by count (negative = shrink).
    ResizeWidth(i32),
    EqualizeLayout,
    MaximizeHeight,
    MaximizeWidth,

    // ── App lifecycle ─────────────────────────────────────────────────
    QuitOrClose,

    // ── Prompt / overlay entry (issue #120) ───────────────────────────
    /// `:` — open the ex command prompt.
    OpenCommandPrompt,
    /// `/` / `?` — open the incremental search prompt.
    OpenSearchPrompt(SearchDir),
    /// `K` — trigger LSP hover at the cursor position.
    LspHover,
    /// `<C-^>` / `<C-6>` — switch to the alternate buffer.
    BufferAlt,

    // ── Predicate-gated buffer / window navigation (issue #120 Phase 3) ──
    /// `H` in Normal mode — cycle to the previous buffer when multiple slots
    /// are open; fall back to `Motion::ViewportTop` otherwise.
    BufferCycleH,
    /// `L` in Normal mode — cycle to the next buffer when multiple slots are
    /// open; fall back to `Motion::ViewportBottom` otherwise.
    BufferCycleL,
    /// `<C-h/j/k/l>` — focus the neighbour window in the given direction.
    /// When no neighbour exists, falls through to the tmux `select-pane`
    /// fallback (if `$TMUX` is set).
    TmuxNavigate(NavDir),

    // ── Pending-state chords (hjkl-vim reducer) ───────────────────────
    /// `r<x>` — begin Replace pending state with the given count.
    /// The app stores `Some(hjkl_vim::PendingState::Replace { count })` and
    /// routes the next key through `hjkl_vim::step` instead of the trie.
    BeginPendingReplace {
        count: u32,
    },
    /// Begin a Find pending state for `f<x>` / `F<x>` / `t<x>` / `T<x>`.
    /// `forward` = true for f/t, false for F/T.
    /// `till` = true for t/T (stop one char before target), false for f/F.
    BeginPendingFind {
        forward: bool,
        till: bool,
        count: u32,
    },
    /// Begin a `g<x>` pending state. The app stores
    /// `Some(hjkl_vim::PendingState::AfterG { count })` and routes the next key
    /// through `hjkl_vim::step` instead of the trie or engine FSM.
    BeginPendingAfterG {
        count: u32,
    },
    /// Begin a `z<x>` pending state. The app stores
    /// `Some(hjkl_vim::PendingState::AfterZ { count })` and routes the next key
    /// through `hjkl_vim::step` instead of the trie or engine FSM.
    BeginPendingAfterZ {
        count: u32,
    },
    /// Begin an op-pending state for `d` / `y` / `c` / `>` / `<` from Normal
    /// mode. The app stores `Some(hjkl_vim::PendingState::AfterOp { op, count1,
    /// inner_count: 0 })` and routes the next key through `hjkl_vim::step`.
    /// `count1` is the prefix count buffered before the operator key.
    BeginPendingAfterOp {
        op: hjkl_vim::OperatorKind,
        count1: u32,
    },
    /// Begin a `"<reg>` register-prefix chord in Normal mode. The app stores
    /// `Some(hjkl_vim::PendingState::SelectRegister)` and routes the next key
    /// through `hjkl_vim::step`. The register char (no fields here — captured
    /// by the second key) is passed to `Editor::set_pending_register`.
    BeginPendingSelectRegister,
    /// Begin a `m<x>` mark-set chord in Normal mode. The app stores
    /// `Some(hjkl_vim::PendingState::SetMark)` and routes the next key
    /// through `hjkl_vim::step`. The mark char is captured by the second key
    /// and passed to `Editor::set_mark_at_cursor`.
    BeginPendingSetMark,
    /// Begin a `'<x>` mark-goto-line chord in Normal mode. The app stores
    /// `Some(hjkl_vim::PendingState::GotoMarkLine)` and routes the next key
    /// through `hjkl_vim::step`. The mark char is captured by the second key
    /// and passed to `Editor::goto_mark_line`.
    BeginPendingGotoMarkLine,
    /// Begin a `` `<x> `` mark-goto-char chord in Normal and Visual modes.
    /// The app stores `Some(hjkl_vim::PendingState::GotoMarkChar)` and routes
    /// the next key through `hjkl_vim::step`. The mark char is captured by the
    /// second key and passed to `Editor::goto_mark_char`.
    BeginPendingGotoMarkChar,
    /// `q` pressed in Normal mode. Branches on `Editor::is_recording_macro()`:
    ///   - If recording: calls `Editor::stop_macro_record()` (bare `q` stop).
    ///   - If not recording: sets `PendingState::RecordMacroTarget` and waits
    ///     for the register char.
    ///
    /// The `count` field is accepted for interface consistency with other pending
    /// actions but is not consumed (macros don't use a count prefix on `q`).
    QChord {
        count: u32,
    },
    /// `@` pressed in Normal mode. Sets `PendingState::PlayMacroTarget { count }`
    /// and waits for the register char. The resolved count (pending_count prefix
    /// or action default) is stored in the state and passed to `PlayMacro` on
    /// the next key.
    BeginPendingPlayMacro {
        count: u32,
    },

    /// `.` pressed in Normal mode — dot-repeat. Replays the last buffered
    /// change `count` times via `Editor::replay_last_change`. Phase 5c of
    /// kryptic-sh/hjkl#71: chord moves from engine FSM `.` arm into the app
    /// keymap. Engine FSM `.` arm stays for macro-replay defensive coverage;
    /// `LastChange` storage stays on engine.
    DotRepeat {
        count: u32,
    },

    // ── Cursor motions (Phase 3a — hjkl-vim keymap path) ──────────────
    /// Engine-level cursor motion executed via the hjkl-vim keymap path.
    ///
    /// Bypasses the engine FSM — the host calls `Editor::apply_motion(kind,
    /// count)` directly. `count` is the action-default multiplier; the
    /// dispatch arm combines it with any buffered `pending_count` prefix.
    ///
    /// The engine FSM arms for the same keys are kept intact for macro-replay
    /// defensive coverage (macros re-feed raw keys through the FSM). This
    /// variant becomes authoritative for user input.
    Motion {
        kind: hjkl_vim::MotionKind,
        count: u32,
    },

    // ── Visual-mode inline operators (Phase 4e — hjkl#70) ─────────────
    /// Visual-mode operator fired inline against the current selection.
    ///
    /// When dispatched the active visual selection range is resolved from
    /// the engine, a range-mutation primitive is called directly, and the
    /// engine exits visual mode. Bound for `d` / `c` / `y` / `>` / `<` in
    /// `HjklMode::Visual` (which covers Visual, VisualLine, and VisualBlock
    /// per the mode-collapse in `keymap.rs`).
    ///
    /// VisualBlock ops fall back to the engine FSM because block-shape
    /// range-mutation requires `apply_block_operator`, which is not exposed
    /// as a public primitive. That gap is tracked in the Phase 4e notes.
    VisualOp {
        op: hjkl_vim::OperatorKind,
        count: u32,
    },

    // ── Phase 6.4: insert-mode entry ──────────────────────────────────────
    /// `i` — enter Insert mode at the current cursor position.
    /// `count` is stored in the insert session for dot-repeat replay.
    EnterInsertI {
        count: u32,
    },
    /// `I` — move to first non-blank then enter Insert mode.
    /// `count` is stored for dot-repeat.
    EnterInsertShiftI {
        count: u32,
    },
    /// `a` — advance cursor one cell then enter Insert mode (append).
    /// `count` is stored for dot-repeat.
    EnterInsertA {
        count: u32,
    },
    /// `A` — move to end-of-line then enter Insert mode (append at end).
    /// `count` is stored for dot-repeat.
    EnterInsertShiftA {
        count: u32,
    },
    /// `o` — open new line below and enter Insert mode.
    /// `count` is stored for dot-repeat.
    EnterInsertO {
        count: u32,
    },
    /// `O` — open new line above and enter Insert mode.
    /// `count` is stored for dot-repeat.
    EnterInsertShiftO {
        count: u32,
    },
    /// `R` — enter Replace mode (overstrike). `count` is for replay.
    EnterReplace {
        count: u32,
    },

    // ── Phase 6.4: char / line mutation ops ───────────────────────────────
    /// `x` — delete `count` chars forward from the cursor.
    DeleteCharForward {
        count: u32,
    },
    /// `X` — delete `count` chars backward from the cursor.
    DeleteCharBackward {
        count: u32,
    },
    /// `s` — substitute `count` chars (delete then Insert).
    SubstituteChar {
        count: u32,
    },
    /// `S` — substitute the current line (equivalent to `cc`).
    SubstituteLine {
        count: u32,
    },
    /// `D` — delete from cursor to end-of-line.
    DeleteToEol,
    /// `C` — change from cursor to end-of-line (delete to EOL + Insert).
    ChangeToEol,
    /// `Y` — yank from cursor to end-of-line. `count` multiplies the motion.
    YankToEol {
        count: u32,
    },
    /// `J` — join `count` lines (default 2).
    JoinLine {
        count: u32,
    },
    /// `~` — toggle case of `count` chars from the cursor.
    ToggleCase {
        count: u32,
    },
    /// `p` — paste unnamed register (or `"r`) after the cursor.
    PasteAfter {
        count: u32,
    },
    /// `P` — paste unnamed register (or `"r`) before the cursor.
    PasteBefore {
        count: u32,
    },

    // ── Phase 6.4: undo / redo ────────────────────────────────────────────
    /// `u` — undo one step in the undo history.
    Undo,
    /// `<C-r>` in Normal mode — redo one step in the redo history.
    Redo,

    // ── Phase 6.4: jumplist ───────────────────────────────────────────────
    /// `<C-o>` — jump back `count` entries in the jumplist.
    JumpBack {
        count: u32,
    },
    /// `<C-i>` / `Tab` — jump forward `count` entries in the jumplist.
    JumpForward {
        count: u32,
    },

    // ── Phase 6.4: scroll ops ─────────────────────────────────────────────
    /// `<C-f>` / `<C-b>` — scroll cursor by one full viewport height.
    /// `dir = Down` for `<C-f>`, `Up` for `<C-b>`.
    ///
    /// These variants are the keymap-driven scroll bindings dispatched from
    /// `dispatch_action`. The FSM fallthrough was removed in Phase 6.8;
    /// hjkl-vim now handles all inputs, and these variants are the sole
    /// scroll path. The dispatch arm in `dispatch_action` exists but no
    /// keymap binding currently constructs this variant — reserved for a
    /// future binding that routes `<C-f>`/`<C-b>` here instead of as Motion.
    #[allow(dead_code)]
    ScrollFullPage {
        dir: hjkl_engine::ScrollDir,
        count: u32,
    },
    /// `<C-d>` / `<C-u>` — scroll cursor by half the viewport height.
    /// `dir = Down` for `<C-d>`, `Up` for `<C-u>`.
    ///
    /// These variants are the keymap-driven scroll bindings dispatched from
    /// `dispatch_action`. The FSM fallthrough was removed in Phase 6.8;
    /// hjkl-vim now handles all inputs, and these variants are the sole
    /// scroll path. The dispatch arm in `dispatch_action` exists but no
    /// keymap binding currently constructs this variant — reserved for a
    /// future binding that routes `<C-d>`/`<C-u>` here instead of as Motion.
    #[allow(dead_code)]
    ScrollHalfPage {
        dir: hjkl_engine::ScrollDir,
        count: u32,
    },
    /// `<C-e>` / `<C-y>` — scroll viewport `count` lines without moving cursor.
    /// `dir = Down` for `<C-e>`, `Up` for `<C-y>`.
    ScrollLine {
        dir: hjkl_engine::ScrollDir,
        count: u32,
    },

    // ── Phase 6.4: search repeat ──────────────────────────────────────────
    /// `n` / `N` — repeat the last search. `forward = true` keeps direction.
    SearchRepeat {
        forward: bool,
        count: u32,
    },
    /// `*` / `#` / `g*` / `g#` — search for word under cursor.
    /// `forward` chooses direction; `whole_word` wraps with `\b` anchors.
    WordSearch {
        forward: bool,
        whole_word: bool,
        count: u32,
    },

    // ── Phase 6.4: visual entry / exit ────────────────────────────────────
    /// `v` from Normal — enter charwise Visual mode.
    EnterVisualChar,
    /// `V` from Normal — enter linewise Visual mode.
    EnterVisualLine,
    /// `<C-v>` from Normal — enter Visual-block mode.
    EnterVisualBlock,
    /// `gv` — restore the last visual selection.
    ReenterLastVisual,
    /// `o` in Visual / VisualLine / VisualBlock — toggle cursor/anchor.
    VisualToggleAnchor,

    // ── User runtime maps (`:map` / `:noremap` family) ─────────────────
    /// User-defined `:map` / `:noremap` runtime mapping. When the trie matches
    /// the LHS, the dispatcher unrolls `keys` according to `recursive`:
    ///   - `recursive = true`  → feed each key back through `dispatch_keymap_in_mode`
    ///     (the RHS can trigger further chord bindings).
    ///   - `recursive = false` → replay each key straight to the engine.
    Replay {
        keys: Vec<hjkl_keymap::KeyEvent>,
        recursive: bool,
    },

    // ── Command-line window (issue #37) ───────────────────────────────────
    /// `q:` / `q/` / `q?` — open the command-line window for the given
    /// history kind. Splits the current window horizontally (below) and
    /// populates the transient buffer with the relevant history entries.
    OpenCmdLineWindow(CmdLineWindowKind),

    // ── File-explorer actions (sidebar context) ───────────────────────────
    /// `<CR>` — open a file, or toggle (expand/collapse) the directory under
    /// the cursor.
    ExplorerActivate,
    /// `<C-s>` — open the node in a horizontal split.
    ExplorerOpenSplit,
    /// `<C-v>` — open the node in a vertical split.
    ExplorerOpenVsplit,
    /// `<C-t>` — open the node in a new tab.
    ExplorerOpenTab,
    /// `-` — move the explorer root one directory up.
    ExplorerRootUp,
    /// `gh` — toggle display of hidden files.
    ExplorerToggleHidden,
    /// `gi` — toggle gitignore filtering.
    ExplorerToggleGitignore,
    /// `ga` — toggle stage / unstage for the node under the cursor.
    ///
    /// When the node's git status is [`ExplorerGit::Staged`] the file is
    /// unstaged (`git reset -- <path>`); for all other statuses (Modified,
    /// Untracked, Deleted) the file is staged (`git add -- <path>`).
    ExplorerGitStageToggle,
    /// `gr` — open a confirm overlay to discard worktree changes for the node
    /// under the cursor.
    ///
    /// After `y` confirmation, runs `git checkout -- <path>` to restore the
    /// file(s) from the index/HEAD. Untracked files are unaffected. Cancelled
    /// with `n` / `N` / `Esc`.
    ExplorerGitDiscard,
    /// `gc` — open a COMMIT_EDITMSG split for staging and committing changes.
    ///
    /// Opens the repo's `COMMIT_EDITMSG` file in a horizontal split, pre-filled
    /// with a comment template and `git status --short --branch` output.
    /// On window close, runs `git commit --cleanup=strip -F <msg_file>`:
    /// a blank/comment-only message causes git to abort (cancel); a real
    /// message commits staged changes. Explorer git colours refresh after commit.
    ExplorerGitCommit,
}

/// Which history ring to show in the command-line window (issue #37).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdLineWindowKind {
    /// `q:` — ex command history.
    Ex,
    /// `q/` — forward-search history.
    SearchForward,
    /// `q?` — backward-search history.
    SearchBackward,
}
