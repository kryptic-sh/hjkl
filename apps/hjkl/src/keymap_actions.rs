//! Application-level actions dispatched from chord bindings.
//!
//! Each variant corresponds to one app-handled binding currently
//! in `event_loop.rs`. The enum is used as the action type for
//! `Keymap<AppAction>`.

/// Every action the app can perform in response to a chord binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppAction {
    // ── File / buffer pickers ──────────────────────────────────────────
    OpenFilePicker,
    OpenBufferPicker,
    OpenGrepPicker,

    // ── Git commands ───────────────────────────────────────────────────
    GitStatus,
    GitLog,
    GitBranch,
    GitFileHistory,
    GitStashes,
    GitTags,
    GitRemotes,

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
}
