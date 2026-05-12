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
