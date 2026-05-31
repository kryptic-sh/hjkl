//! Keymap construction — builds the app-level chord trie and provides the
//! engine-input-to-crossterm translation used by macro replay.
//!
//! Owns: [`build_app_keymap`], [`engine_input_to_key_event`].

use std::time::Duration;

use hjkl_keymap::Keymap;

use crate::app::{NavDir, SearchDir};
use crate::keymap_actions::AppAction;

use super::keymap;

/// Build the Normal-mode application keymap for the given leader character.
///
/// Every app-handled chord binding is registered here. The resulting
/// `Keymap<AppAction, keymap::HjklMode>` is stored on [`App`] and consulted by the event loop
/// before forwarding keys to the editor engine.
pub(crate) fn build_app_keymap(leader: char) -> Keymap<AppAction, keymap::HjklMode> {
    use keymap::HjklMode as Mode;
    let mut km = Keymap::new(leader);
    // Timeout matches the which-key delay default; overridden by `with_config`.
    km.set_timeout(Duration::from_millis(500));

    let bindings: &[(&str, AppAction, &str)] = &[
        // ── Prompt / overlay entry (Phase 2 — issue #120) ─────────────────
        // These chords are registered in the keymap trie and dispatched via
        // dispatch_action, removing the need for inline intercepts in run().
        // `:` in Normal mode is gated on pending_state.is_none() at the
        // dispatch site (see dispatch_action arm) to preserve `@:` behaviour.
        (":", AppAction::OpenCommandPrompt, "open command prompt"),
        (
            "/",
            AppAction::OpenSearchPrompt(SearchDir::Forward),
            "search forward",
        ),
        (
            "?",
            AppAction::OpenSearchPrompt(SearchDir::Backward),
            "search backward",
        ),
        ("K", AppAction::LspHover, "lsp hover"),
        ("<C-^>", AppAction::BufferAlt, "alt buffer"),
        // <C-6> is the ASCII-terminal alias for <C-^> on some terminals.
        ("<C-6>", AppAction::BufferAlt, "alt buffer"),
        // ── File / buffer / grep pickers ──────────────────────────────────
        ("<leader><leader>", AppAction::OpenFilePicker, "file picker"),
        ("<leader>f", AppAction::OpenFilePicker, "file picker"),
        ("<leader>b", AppAction::OpenBufferPicker, "buffer picker"),
        ("<leader>/", AppAction::OpenGrepPicker, "grep picker"),
        // ── Git sub-commands ───────────────────────────────────────────────
        ("<leader>gs", AppAction::GitStatus, "git status"),
        ("<leader>gl", AppAction::GitLog, "git log"),
        ("<leader>gb", AppAction::GitBranch, "git branches"),
        ("<leader>gB", AppAction::GitFileHistory, "git file history"),
        ("<leader>gS", AppAction::GitStashes, "git stashes"),
        ("<leader>gt", AppAction::GitTags, "git tags"),
        ("<leader>gr", AppAction::GitRemotes, "git remotes"),
        ("<leader>gm", AppAction::GitBlameLine, "git blame line"),
        // ── LSP / diagnostics ─────────────────────────────────────────────
        ("<leader>d", AppAction::ShowDiagAtCursor, "show diagnostic"),
        ("<leader>ca", AppAction::LspCodeActions, "code actions"),
        ("<leader>rn", AppAction::LspRename, "rename symbol"),
        // ── g-prefix ──────────────────────────────────────────────────────
        // NOTE: bare `g` is bound separately below as BeginPendingAfterG.
        // The app-level g-chord actions (gt, gd, etc.) are dispatched from
        // the AfterGChord arm in event_loop.rs rather than the trie, so
        // that a bare `g` can immediately set pending state without waiting
        // for the trie timeout (Ambiguous resolution).
        // ── ] / [ bracket motions ─────────────────────────────────────────
        ("]b", AppAction::BufferNext, "next buffer"),
        ("[b", AppAction::BufferPrev, "prev buffer"),
        ("]d", AppAction::DiagNext, "next diagnostic"),
        ("[d", AppAction::DiagPrev, "prev diagnostic"),
        ("]D", AppAction::DiagNextError, "next error"),
        ("[D", AppAction::DiagPrevError, "prev error"),
        // ── <C-w> window motions ──────────────────────────────────────────
        ("<C-w>h", AppAction::FocusLeft, "focus left"),
        ("<C-w>j", AppAction::FocusBelow, "focus down"),
        ("<C-w>k", AppAction::FocusAbove, "focus up"),
        ("<C-w>l", AppAction::FocusRight, "focus right"),
        ("<C-w>w", AppAction::FocusNext, "focus next"),
        ("<C-w>W", AppAction::FocusPrev, "focus prev"),
        ("<C-w>c", AppAction::CloseFocusedWindow, "close window"),
        ("<C-w>q", AppAction::QuitOrClose, "quit/close"),
        ("<C-w>o", AppAction::OnlyFocusedWindow, "close others"),
        ("<C-w>x", AppAction::SwapWithSibling, "swap with sibling"),
        ("<C-w>r", AppAction::SwapWithSibling, "swap with sibling"),
        ("<C-w>R", AppAction::SwapWithSibling, "swap with sibling"),
        ("<C-w>T", AppAction::MoveWindowToNewTab, "move to new tab"),
        ("<C-w>n", AppAction::NewSplit, "new split"),
        ("<C-w>+", AppAction::ResizeHeight(1), "taller"),
        ("<C-w>-", AppAction::ResizeHeight(-1), "shorter"),
        ("<C-w><gt>", AppAction::ResizeWidth(1), "wider"),
        ("<C-w><lt>", AppAction::ResizeWidth(-1), "narrower"),
        ("<C-w>=", AppAction::EqualizeLayout, "equalize"),
        ("<C-w>_", AppAction::MaximizeHeight, "maximize height"),
        ("<C-w>|", AppAction::MaximizeWidth, "maximize width"),
    ];

    for (chord_str, action, desc) in bindings {
        if let Err(e) = km.add(Mode::Normal, chord_str, action.clone(), desc) {
            // Should never fail with our static strings, but log rather than panic.
            eprintln!("hjkl: keymap.add({chord_str:?}) failed: {e}");
        }
    }

    // ── pending-state chords ───────────────────────────────────────────────
    // `r<x>` — begin Replace pending state. Bound in both Normal and Visual so
    // the trie intercepts `r` before the engine FSM sees it.
    let replace_action = AppAction::BeginPendingReplace { count: 1 };
    for mode in [Mode::Normal, Mode::Visual] {
        if let Err(e) = km.add(mode, "r", replace_action.clone(), "replace char") {
            eprintln!("hjkl: keymap.add(r) failed: {e}");
        }
    }

    // `f<x>` / `F<x>` / `t<x>` / `T<x>` — bare find chords, migrated to
    // hjkl-vim's PendingState::Find reducer. Bound in Normal and Visual only.
    // Operator-pending find (`df<x>`, etc.) still goes through the engine FSM.
    for (key, forward, till, desc) in [
        ("f", true, false, "find char forward"),
        ("F", false, false, "find char backward"),
        ("t", true, true, "till char forward"),
        ("T", false, true, "till char backward"),
    ] {
        let action = AppAction::BeginPendingFind {
            forward,
            till,
            count: 1,
        };
        for mode in [Mode::Normal, Mode::Visual] {
            if let Err(e) = km.add(mode, key, action.clone(), desc) {
                eprintln!("hjkl: keymap.add({key}) failed: {e}");
            }
        }
    }

    // `g<x>` — bare g-prefix chord, migrated to hjkl-vim's
    // PendingState::AfterG reducer. Bound in Normal + all three visual
    // modes so `gg` (and other g-chords) work consistently in
    // visual/visual-line/visual-block. Operator-pending g (`dgU`, etc.)
    // and the engine's internal `Pending::G` / `Pending::OpG` still go
    // through the engine FSM.
    let after_g_action = AppAction::BeginPendingAfterG { count: 1 };
    for mode in [
        Mode::Normal,
        Mode::Visual,
        Mode::VisualLine,
        Mode::VisualBlock,
    ] {
        if let Err(e) = km.add(mode, "g", after_g_action.clone(), "g-prefix chord") {
            eprintln!("hjkl: keymap.add(g) failed: {e}");
        }
    }

    // `z<x>` — bare z-prefix chord, migrated to hjkl-vim's
    // PendingState::AfterZ reducer. Bound in Normal + all three visual
    // modes for parity with `g`. Operator-pending z (`zf{motion}`) and
    // the engine's internal `Pending::Z` still go through the engine
    // FSM for non-visual `zf`.
    let after_z_action = AppAction::BeginPendingAfterZ { count: 1 };
    for mode in [
        Mode::Normal,
        Mode::Visual,
        Mode::VisualLine,
        Mode::VisualBlock,
    ] {
        if let Err(e) = km.add(mode, "z", after_z_action.clone(), "z-prefix chord") {
            eprintln!("hjkl: keymap.add(z) failed: {e}");
        }
    }

    // `d` / `y` / `c` / `>` / `<` — bare op-pending entry from Normal mode,
    // migrated to hjkl-vim's PendingState::AfterOp reducer. Bound in Normal
    // mode only. Visual-mode `d`/`y`/`c`/`>`/`<` execute inline through the
    // engine FSM and are NOT intercepted here.
    //
    // The `>` and `<` chars need quoting in the chord string per hjkl-keymap
    // notation (`<gt>` and `<lt>`).
    for (key, op, desc) in [
        ("d", hjkl_vim::OperatorKind::Delete, "delete operator"),
        ("y", hjkl_vim::OperatorKind::Yank, "yank operator"),
        ("c", hjkl_vim::OperatorKind::Change, "change operator"),
        ("<gt>", hjkl_vim::OperatorKind::Indent, "indent operator"),
        ("<lt>", hjkl_vim::OperatorKind::Outdent, "outdent operator"),
        (
            "=",
            hjkl_vim::OperatorKind::AutoIndent,
            "auto-indent operator",
        ),
        (
            "!",
            hjkl_vim::OperatorKind::Filter,
            "filter through command",
        ),
    ] {
        let action = AppAction::BeginPendingAfterOp { op, count1: 1 };
        if let Err(e) = km.add(Mode::Normal, key, action, desc) {
            eprintln!("hjkl: keymap.add({key}) failed: {e}");
        }
    }

    // Visual-mode operators — fire inline against the current selection.
    // `d` / `y` / `c` / `>` / `<` bound in HjklMode::Visual (covers Visual,
    // VisualLine, and VisualBlock per the mode-collapse in keymap.rs:125).
    //
    // All three modes (Visual, VisualLine, VisualBlock) route through the
    // public range-mutation primitives. Phase 4e follow-ups closed the gaps:
    //   - pending_register() getter exposed (Visual register honors "a prefix)
    //   - run_operator_over_range linewise guard fixed (VisualLine single-row)
    //   - delete_block/yank_block/change_block/indent_block exposed (VisualBlock)
    for (key, op, desc) in [
        ("d", hjkl_vim::OperatorKind::Delete, "delete selection"),
        ("y", hjkl_vim::OperatorKind::Yank, "yank selection"),
        ("c", hjkl_vim::OperatorKind::Change, "change selection"),
        ("<gt>", hjkl_vim::OperatorKind::Indent, "indent selection"),
        ("<lt>", hjkl_vim::OperatorKind::Outdent, "outdent selection"),
        (
            "=",
            hjkl_vim::OperatorKind::AutoIndent,
            "auto-indent selection",
        ),
        (
            "!",
            hjkl_vim::OperatorKind::Filter,
            "filter selection through command",
        ),
    ] {
        let action = AppAction::VisualOp { op, count: 1 };
        if let Err(e) = km.add(Mode::Visual, key, action, desc) {
            eprintln!("hjkl: keymap.add({key} Visual) failed: {e}");
        }
    }

    // `"<reg>` — register-prefix chord in Normal mode only. Visual-mode `"`
    // is not intercepted here; the engine FSM handles any Visual-mode `"`
    // input directly (there is no visual-register-select path in the engine).
    // Bound Normal-only, matching how vim treats `"` in Normal vs Visual mode.
    if let Err(e) = km.add(
        Mode::Normal,
        "\"",
        AppAction::BeginPendingSelectRegister,
        "register-prefix chord",
    ) {
        eprintln!("hjkl: keymap.add(\\\") failed: {e}");
    }

    // `m<x>` — mark-set chord. Normal mode only (vim's `m` is not meaningful
    // in Visual mode). The engine FSM arms for `m` in Normal mode are kept
    // intact for macro-replay defensive coverage (deletion in Phase 6).
    if let Err(e) = km.add(
        Mode::Normal,
        "m",
        AppAction::BeginPendingSetMark,
        "set mark chord",
    ) {
        eprintln!("hjkl: keymap.add(m) failed: {e}");
    }

    // `'<x>` — mark-goto-line chord. Normal mode only.
    if let Err(e) = km.add(
        Mode::Normal,
        "'",
        AppAction::BeginPendingGotoMarkLine,
        "goto mark linewise chord",
    ) {
        eprintln!("hjkl: keymap.add(') failed: {e}");
    }

    // `` `<x> `` — mark-goto-char chord. Normal + all three Visual modes.
    // In Visual mode, `` ` `` jumps the cursor to a mark charwise while keeping
    // the selection active (matches engine's pre-existing vim.rs:2058-2066
    // behaviour). The engine FSM arms for `` ` `` are kept for macro-replay.
    for mode in [
        Mode::Normal,
        Mode::Visual,
        Mode::VisualLine,
        Mode::VisualBlock,
    ] {
        if let Err(e) = km.add(
            mode,
            "`",
            AppAction::BeginPendingGotoMarkChar,
            "goto mark charwise chord",
        ) {
            eprintln!("hjkl: keymap.add(`) failed: {e}");
        }
    }

    // ── Phase 3a: char + line motions via hjkl-vim keymap path ───────────
    // Bound in Normal, Visual, VisualLine, and VisualBlock. Engine FSM arms
    // for these keys are kept intact for macro-replay defensive coverage.
    for (chord, kind, desc) in [
        ("h", hjkl_vim::MotionKind::CharLeft, "char left"),
        ("<BS>", hjkl_vim::MotionKind::CharLeft, "char left"),
        ("l", hjkl_vim::MotionKind::CharRight, "char right"),
        ("<Space>", hjkl_vim::MotionKind::CharRight, "char right"),
        ("j", hjkl_vim::MotionKind::LineDown, "line down"),
        ("k", hjkl_vim::MotionKind::LineUp, "line up"),
        (
            "+",
            hjkl_vim::MotionKind::FirstNonBlankDown,
            "next line first non-blank",
        ),
        (
            "-",
            hjkl_vim::MotionKind::FirstNonBlankUp,
            "prev line first non-blank",
        ),
        ("w", hjkl_vim::MotionKind::WordForward, "word forward"),
        (
            "W",
            hjkl_vim::MotionKind::BigWordForward,
            "BIG word forward",
        ),
        ("b", hjkl_vim::MotionKind::WordBackward, "word back"),
        ("B", hjkl_vim::MotionKind::BigWordBackward, "BIG word back"),
        ("e", hjkl_vim::MotionKind::WordEnd, "word end"),
        ("E", hjkl_vim::MotionKind::BigWordEnd, "BIG word end"),
        // Phase 3c: line-anchor motions.
        ("0", hjkl_vim::MotionKind::LineStart, "line start"),
        ("<Home>", hjkl_vim::MotionKind::LineStart, "line start"),
        ("^", hjkl_vim::MotionKind::FirstNonBlank, "first non-blank"),
        ("$", hjkl_vim::MotionKind::LineEnd, "line end"),
        ("<End>", hjkl_vim::MotionKind::LineEnd, "line end"),
        // Phase 3d: doc-level motion.
        ("G", hjkl_vim::MotionKind::GotoLine, "goto line"),
        // Phase 3e: find-repeat motions.
        (";", hjkl_vim::MotionKind::FindRepeat, "find repeat"),
        (
            ",",
            hjkl_vim::MotionKind::FindRepeatReverse,
            "find repeat reverse",
        ),
        // Phase 3f: bracket-match motion.
        ("%", hjkl_vim::MotionKind::BracketMatch, "match bracket"),
        // Phase 3g: scroll / viewport motions.
        // NOTE: H and L are registered separately below (BufferCycleH/L for
        // Normal mode; Motion::Viewport* for Visual modes). Removed from this
        // array so they don't accidentally bind in Normal via the four-mode loop.
        ("M", hjkl_vim::MotionKind::ViewportMiddle, "viewport middle"),
        (
            "<C-d>",
            hjkl_vim::MotionKind::HalfPageDown,
            "half page down",
        ),
        ("<C-u>", hjkl_vim::MotionKind::HalfPageUp, "half page up"),
        (
            "<C-f>",
            hjkl_vim::MotionKind::FullPageDown,
            "full page down",
        ),
        ("<C-b>", hjkl_vim::MotionKind::FullPageUp, "full page up"),
    ] {
        let action = AppAction::Motion { kind, count: 1 };
        for mode in [
            Mode::Normal,
            Mode::Visual,
            Mode::VisualLine,
            Mode::VisualBlock,
        ] {
            if let Err(e) = km.add(mode, chord, action.clone(), desc) {
                eprintln!("hjkl: keymap.add({chord:?}) failed: {e}");
            }
        }
    }

    // ── H / L viewport motions for Visual modes (issue #120 Phase 3) ─────
    // In Normal mode H/L are registered below as BufferCycleH/L (the action
    // checks slots.len() at dispatch time). In all Visual modes H/L remain
    // viewport motions — no buffer-cycle semantics in Visual.
    for (chord, kind, desc) in [
        ("H", hjkl_vim::MotionKind::ViewportTop, "viewport top"),
        ("L", hjkl_vim::MotionKind::ViewportBottom, "viewport bottom"),
    ] {
        let action = AppAction::Motion { kind, count: 1 };
        for mode in [Mode::Visual, Mode::VisualLine, Mode::VisualBlock] {
            if let Err(e) = km.add(mode, chord, action.clone(), desc) {
                eprintln!("hjkl: keymap.add({chord:?} visual) failed: {e}");
            }
        }
    }

    // ── H / L buffer cycle (Normal mode, issue #120 Phase 3) ─────────────
    // BufferCycleH/L dispatch checks slots.len() at call time:
    //   slots > 1  → buffer_prev / buffer_next
    //   single slot → apply_motion(ViewportTop/Bottom, count) directly
    // This replaces the inline H/L intercept that checked slots.len() before
    // forwarding to the engine.
    if let Err(e) = km.add(
        Mode::Normal,
        "H",
        AppAction::BufferCycleH,
        "prev buffer or viewport top",
    ) {
        eprintln!("hjkl: keymap.add(H Normal) failed: {e}");
    }
    if let Err(e) = km.add(
        Mode::Normal,
        "L",
        AppAction::BufferCycleL,
        "next buffer or viewport bottom",
    ) {
        eprintln!("hjkl: keymap.add(L Normal) failed: {e}");
    }

    // ── <C-h/j/k/l> window focus + tmux fallback (issue #120 Phase 3) ───
    // TmuxNavigate dispatch checks whether a neighbour exists:
    //   neighbour present → focus_left/below/above/right
    //   no neighbour, $TMUX set → tmux select-pane
    //   no neighbour, no tmux → no-op
    // <C-Backspace> is an alias for <C-h> on some terminals (mirrors the
    // original inline intercept's `key.code == KeyCode::Backspace` arm).
    for (chord, dir, desc) in [
        ("<C-h>", NavDir::Left, "focus left or tmux left"),
        ("<C-j>", NavDir::Down, "focus down or tmux down"),
        ("<C-k>", NavDir::Up, "focus up or tmux up"),
        ("<C-l>", NavDir::Right, "focus right or tmux right"),
        // <C-BS> is the alias for <C-h> delivered by some terminals (crossterm
        // decodes it as Backspace+CONTROL rather than Char('h')+CONTROL).
        ("<C-BS>", NavDir::Left, "focus left or tmux left"),
    ] {
        if let Err(e) = km.add(Mode::Normal, chord, AppAction::TmuxNavigate(dir), desc) {
            eprintln!("hjkl: keymap.add({chord:?}) failed: {e}");
        }
    }

    // ── Phase 5b: macro record / play chord entry points ─────────────────
    // `q` — record-macro or stop-recording gate (QChord handles the branch).
    // Normal-mode only: macros cannot be started or stopped in Visual mode.
    // Engine FSM arms for `q` are kept for macro-replay defensive coverage.
    if let Err(e) = km.add(
        Mode::Normal,
        "q",
        AppAction::QChord { count: 1 },
        "record macro / stop recording",
    ) {
        eprintln!("hjkl: keymap.add(q) failed: {e}");
    }

    // ── Issue #37: q: / q/ / q? are handled inside the QChord pending ────
    // dispatch (pending_actions.rs) to avoid making bare `q` Ambiguous.
    // No trie entries needed here.

    // `@` — begin play-macro chord. Normal-mode only.
    // Engine FSM arms for `@` are kept for macro-replay defensive coverage.
    if let Err(e) = km.add(
        Mode::Normal,
        "@",
        AppAction::BeginPendingPlayMacro { count: 1 },
        "play macro chord",
    ) {
        eprintln!("hjkl: keymap.add(@) failed: {e}");
    }

    // ── Phase 5c: dot-repeat ─────────────────────────────────────────────
    // `.` replays the last buffered change. Normal-mode only.
    // Engine FSM `.` arm stays for macro-replay defensive coverage.
    if let Err(e) = km.add(
        Mode::Normal,
        ".",
        AppAction::DotRepeat { count: 1 },
        "repeat last change",
    ) {
        eprintln!("hjkl: keymap.add(.) failed: {e}");
    }

    // ── Phase 6.4: insert-mode entry ─────────────────────────────────────
    // Normal mode only. Engine FSM arms kept for macro-replay coverage.
    for (chord, action, desc) in [
        (
            "i",
            AppAction::EnterInsertI { count: 1 },
            "insert before cursor",
        ),
        (
            "I",
            AppAction::EnterInsertShiftI { count: 1 },
            "insert at line start",
        ),
        (
            "a",
            AppAction::EnterInsertA { count: 1 },
            "append after cursor",
        ),
        (
            "A",
            AppAction::EnterInsertShiftA { count: 1 },
            "append at line end",
        ),
        ("o", AppAction::EnterInsertO { count: 1 }, "open line below"),
        (
            "O",
            AppAction::EnterInsertShiftO { count: 1 },
            "open line above",
        ),
        (
            "R",
            AppAction::EnterReplace { count: 1 },
            "enter replace mode",
        ),
    ] {
        if let Err(e) = km.add(Mode::Normal, chord, action, desc) {
            eprintln!("hjkl: keymap.add({chord:?}) failed: {e}");
        }
    }

    // ── Phase 6.4: char / line mutation ops ──────────────────────────────
    // Normal mode only. Engine FSM arms kept for macro-replay coverage.
    for (chord, action, desc) in [
        (
            "x",
            AppAction::DeleteCharForward { count: 1 },
            "delete char forward",
        ),
        (
            "X",
            AppAction::DeleteCharBackward { count: 1 },
            "delete char backward",
        ),
        (
            "s",
            AppAction::SubstituteChar { count: 1 },
            "substitute char",
        ),
        (
            "S",
            AppAction::SubstituteLine { count: 1 },
            "substitute line",
        ),
        ("D", AppAction::DeleteToEol, "delete to end of line"),
        ("C", AppAction::ChangeToEol, "change to end of line"),
        (
            "Y",
            AppAction::YankToEol { count: 1 },
            "yank to end of line",
        ),
        ("J", AppAction::JoinLine { count: 1 }, "join lines"),
        ("~", AppAction::ToggleCase { count: 1 }, "toggle case"),
        (
            "p",
            AppAction::PasteAfter { count: 1 },
            "paste after cursor",
        ),
        (
            "P",
            AppAction::PasteBefore { count: 1 },
            "paste before cursor",
        ),
    ] {
        if let Err(e) = km.add(Mode::Normal, chord, action, desc) {
            eprintln!("hjkl: keymap.add({chord:?}) failed: {e}");
        }
    }

    // ── Phase 6.4: undo / redo ────────────────────────────────────────────
    // `u` undo in Normal mode. `<C-r>` redo in Normal mode only —
    // Insert-mode `<C-r>` goes through the engine FSM and is not intercepted.
    if let Err(e) = km.add(Mode::Normal, "u", AppAction::Undo, "undo") {
        eprintln!("hjkl: keymap.add(u) failed: {e}");
    }
    if let Err(e) = km.add(Mode::Normal, "<C-r>", AppAction::Redo, "redo") {
        eprintln!("hjkl: keymap.add(<C-r>) failed: {e}");
    }

    // ── Phase 6.4: jumplist ───────────────────────────────────────────────
    // `<C-o>` / `<C-i>` bound in Normal mode only.
    // Engine FSM arms kept for macro-replay coverage.
    if let Err(e) = km.add(
        Mode::Normal,
        "<C-o>",
        AppAction::JumpBack { count: 1 },
        "jump back",
    ) {
        eprintln!("hjkl: keymap.add(<C-o>) failed: {e}");
    }
    // Tab in Normal mode = <C-i> (vim aliases them). Crossterm delivers the
    // actual Tab key as KeyCode::Tab, not as Char('i')+CTRL, so we bind <Tab>
    // here. The engine FSM also handles the Tab code path for macro-replay
    // defensive coverage.
    if let Err(e) = km.add(
        Mode::Normal,
        "<Tab>",
        AppAction::JumpForward { count: 1 },
        "jump forward",
    ) {
        eprintln!("hjkl: keymap.add(<Tab>) failed: {e}");
    }

    // ── Phase 6.4: scroll-line ops ────────────────────────────────────────
    // `<C-e>` / `<C-y>` — scroll viewport without moving cursor.
    // Bound in Normal mode only. (Phase 3g already bound <C-d>/<C-u>/<C-f>/<C-b>
    // as Motion variants; those are kept intact — no conflict.)
    use hjkl_engine::ScrollDir;
    if let Err(e) = km.add(
        Mode::Normal,
        "<C-e>",
        AppAction::ScrollLine {
            dir: ScrollDir::Down,
            count: 1,
        },
        "scroll line down",
    ) {
        eprintln!("hjkl: keymap.add(<C-e>) failed: {e}");
    }
    if let Err(e) = km.add(
        Mode::Normal,
        "<C-y>",
        AppAction::ScrollLine {
            dir: ScrollDir::Up,
            count: 1,
        },
        "scroll line up",
    ) {
        eprintln!("hjkl: keymap.add(<C-y>) failed: {e}");
    }

    // ── Phase 6.4: search repeat ──────────────────────────────────────────
    // `n` / `N` — repeat last search. Normal + all Visual modes.
    // `*` / `#` / `g*` / `g#` — word-search. Normal mode only
    // (g* / g# are dispatched through AfterG reducer via BeginPendingAfterG).
    for (chord, forward, desc) in [
        ("n", true, "search forward repeat"),
        ("N", false, "search backward repeat"),
    ] {
        let action = AppAction::SearchRepeat { forward, count: 1 };
        for mode in [
            Mode::Normal,
            Mode::Visual,
            Mode::VisualLine,
            Mode::VisualBlock,
        ] {
            if let Err(e) = km.add(mode, chord, action.clone(), desc) {
                eprintln!("hjkl: keymap.add({chord:?}) failed: {e}");
            }
        }
    }
    // `*` / `#` whole-word search. Normal mode only.
    for (chord, forward, desc) in [
        ("*", true, "search word under cursor forward"),
        ("#", false, "search word under cursor backward"),
    ] {
        let action = AppAction::WordSearch {
            forward,
            whole_word: true,
            count: 1,
        };
        if let Err(e) = km.add(Mode::Normal, chord, action, desc) {
            eprintln!("hjkl: keymap.add({chord:?}) failed: {e}");
        }
    }

    // ── Phase 6.4: visual entry from Normal ──────────────────────────────
    // `v` / `V` / `<C-v>` — enter visual from Normal. `gv` is dispatched
    // through the AfterG reducer (BeginPendingAfterG) — not bound here.
    if let Err(e) = km.add(
        Mode::Normal,
        "v",
        AppAction::EnterVisualChar,
        "enter visual charwise",
    ) {
        eprintln!("hjkl: keymap.add(v) failed: {e}");
    }
    if let Err(e) = km.add(
        Mode::Normal,
        "V",
        AppAction::EnterVisualLine,
        "enter visual linewise",
    ) {
        eprintln!("hjkl: keymap.add(V) failed: {e}");
    }
    if let Err(e) = km.add(
        Mode::Normal,
        "<C-v>",
        AppAction::EnterVisualBlock,
        "enter visual block",
    ) {
        eprintln!("hjkl: keymap.add(<C-v>) failed: {e}");
    }

    // ── Phase 6.4: gv — reenter last visual ──────────────────────────────
    // `gv` is routed through AfterG → the AfterGChord arm in event_loop.rs
    // dispatches ReenterLastVisual. We do NOT bind `gv` directly in the trie
    // because `g` is already bound as BeginPendingAfterG (pending state chord).

    // ── Phase 6.4: visual-mode anchor toggle ─────────────────────────────
    // `o` in Visual / VisualLine / VisualBlock — toggle cursor/anchor.
    // Normal `o` is bound above as EnterInsertO. Mode discrimination is
    // handled automatically by the trie (different mode → different action).
    for mode in [Mode::Visual, Mode::VisualLine, Mode::VisualBlock] {
        if let Err(e) = km.add(
            mode,
            "o",
            AppAction::VisualToggleAnchor,
            "visual toggle anchor",
        ) {
            eprintln!("hjkl: keymap.add(o Visual) failed: {e}");
        }
    }

    km
}

/// Translate an `hjkl_engine::Input` back to a `crossterm::event::KeyEvent`
/// for re-feeding through `route_chord_key` during macro replay.
///
/// This is the inverse of `Editor::handle_key`'s `crossterm_to_input` path.
/// Modifier flags (ctrl, alt, shift) are preserved. Keys that have no
/// crossterm equivalent (e.g. `Key::Null`, `Key::PageUp` without a standard
/// mapping) produce a `KeyCode::Null` sentinel that the replay loop skips.
pub(crate) fn engine_input_to_key_event(input: hjkl_engine::Input) -> crossterm::event::KeyEvent {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use hjkl_engine::Key;

    let code = match input.key {
        Key::Char(c) => KeyCode::Char(c),
        Key::Backspace => KeyCode::Backspace,
        Key::Delete => KeyCode::Delete,
        Key::Enter => KeyCode::Enter,
        Key::Left => KeyCode::Left,
        Key::Right => KeyCode::Right,
        Key::Up => KeyCode::Up,
        Key::Down => KeyCode::Down,
        Key::Home => KeyCode::Home,
        Key::End => KeyCode::End,
        Key::Tab => KeyCode::Tab,
        Key::Esc => KeyCode::Esc,
        Key::PageUp => KeyCode::PageUp,
        Key::PageDown => KeyCode::PageDown,
        Key::Null => KeyCode::Null,
    };
    let mut mods = KeyModifiers::NONE;
    if input.ctrl {
        mods |= KeyModifiers::CONTROL;
    }
    if input.alt {
        mods |= KeyModifiers::ALT;
    }
    if input.shift {
        mods |= KeyModifiers::SHIFT;
    }
    KeyEvent::new(code, mods)
}
