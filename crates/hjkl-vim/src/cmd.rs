/// Controller commands the host engine implements. hjkl-vim never mutates
/// the editor directly — it emits a command and the host (apps/hjkl) calls
/// the corresponding `Editor` method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCmd {
    ReplaceChar {
        ch: char,
        count: usize,
    },
    /// Emitted by `PendingState::Find` when the user completes `f<x>` / `F<x>`
    /// / `t<x>` / `T<x>`. The host calls `Editor::find_char`.
    FindChar {
        ch: char,
        forward: bool,
        till: bool,
        count: usize,
    },
    /// Emitted by `PendingState::AfterG` when the user completes `g<x>`. The
    /// host calls `Editor::after_g(ch, count)`.
    AfterGChord {
        ch: char,
        count: usize,
    },
    /// Emitted by `PendingState::AfterZ` when the user completes `z<x>`. The
    /// host calls `Editor::after_z(ch, count)`.
    AfterZChord {
        ch: char,
        count: usize,
    },
    /// `d<motion>` / `y<motion>` / `c<motion>` / `><motion>` / `<<motion>` —
    /// apply operator over a single-key motion. `motion_key` is the raw key
    /// char (e.g. `'w'`, `'$'`, `'G'`). Engine parses via `parse_motion` and
    /// applies. `total_count = count1 * inner_count`.
    ApplyOpMotion {
        op: crate::operator::OperatorKind,
        motion_key: char,
        total_count: usize,
    },
    /// `dd` / `yy` / `cc` / `>>` / `<<` — doubled-letter line op.
    ApplyOpDouble {
        op: crate::operator::OperatorKind,
        total_count: usize,
    },
    /// `diw` etc. — apply operator over text-object range. The reducer owns the
    /// `i`/`a` key via `PendingState::OpTextObj`; on the next char it emits this
    /// command. Host calls `Editor::apply_op_text_obj`.
    ApplyOpTextObj {
        op: crate::operator::OperatorKind,
        ch: char,
        inner: bool,
        total_count: usize,
    },
    /// `dgg` etc. — apply operator over g-chord motion or case-op linewise form.
    /// The reducer owns the `g` key via `PendingState::OpG`; on the next char it
    /// emits this command. Host calls `Editor::apply_op_g`.
    ApplyOpG {
        op: crate::operator::OperatorKind,
        ch: char,
        total_count: usize,
    },
    /// `df<x>` / `dF<x>` / `dt<x>` / `dT<x>` — apply operator over find
    /// motion. Engine builds `Motion::Find { ch, forward, till }` and applies
    /// it. `total_count` is `count1 * inner_count` folded at transition time.
    ///
    /// Replaces `EnterOpFind` (removed in 0.7.0). The reducer no longer sets
    /// engine `Pending::OpFind`; instead it transitions to
    /// `PendingState::OpFind` and emits this command on the next char.
    ApplyOpFind {
        op: crate::operator::OperatorKind,
        ch: char,
        forward: bool,
        till: bool,
        total_count: usize,
    },
    /// `"<reg>` chord completion. Engine validates `reg` against
    /// `[a-zA-Z0-9"+*_]` and sets `vim.pending_register` if valid. Invalid
    /// chars are silently ignored (no-op), matching the engine FSM behaviour.
    SetPendingRegister {
        reg: char,
    },
    /// `m<ch>` chord completion. Engine validates `ch` and records the mark at
    /// the current cursor position. Invalid chars are silently ignored (no-op).
    SetMark {
        ch: char,
    },
    /// `'<ch>` chord completion. Engine validates `ch` and jumps to the mark
    /// linewise (row only, cursor lands on first non-blank column). Invalid or
    /// unset marks are silently ignored (no-op).
    GotoMarkLine {
        ch: char,
    },
    /// `` `<ch> `` chord completion. Engine validates `ch` and jumps to the
    /// mark charwise (exact row + col). Invalid or unset marks are silently
    /// ignored (no-op).
    GotoMarkChar {
        ch: char,
    },
    /// `q{reg}` chord completion. Host calls `Editor::start_macro_record(reg)`
    /// to begin capturing keystrokes into the named register. Invalid chars
    /// (non-alphabetic, non-digit) are silently ignored by the host.
    StartMacroRecord {
        reg: char,
    },
    /// `@{reg}` / `@@` chord completion. Host calls `Editor::play_macro(reg)`
    /// to obtain ONE iteration of the decoded `Input` stream and re-feeds it
    /// through `route_chord_key` `count` times (iteratively — never
    /// materializing `keys × count`). `reg == '@'` means "repeat last
    /// macro" — the host resolves the actual register via
    /// `Editor::play_macro`'s internal logic.
    PlayMacro {
        reg: char,
        count: usize,
    },
}
