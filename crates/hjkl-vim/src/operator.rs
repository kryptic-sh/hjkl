/// Operator identity carried by the reducer. Kept independent of
/// `hjkl-engine` so `hjkl-vim` has no upstream dependency.
///
/// The five operators that can be entered directly from Normal mode via bare
/// `d` / `y` / `c` / `>` / `<`, plus the g-prefix case/reflow operators
/// (`gU` / `gu` / `g~` / `gq` / `gw`) bridged through the reducer in chunk 2c-v.
/// Fold (`zf`) does not enter bare op-pending so it is omitted entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorKind {
    /// `d` — delete.
    Delete,
    /// `y` — yank.
    Yank,
    /// `c` — change (delete + enter Insert mode).
    Change,
    /// `>` — indent.
    Indent,
    /// `<` — outdent.
    Outdent,
    /// `gU` — uppercase.
    Uppercase,
    /// `gu` — lowercase.
    Lowercase,
    /// `g~` — toggle case.
    ToggleCase,
    /// `gq` — reflow / format text. Cursor moves to end of reflowed range.
    Reflow,
    /// `gw` — reflow like `gq` but cursor stays at its pre-reflow position.
    ReflowKeepCursor,
    /// `=` — auto-indent (v1 dumb shiftwidth bracket counting).
    AutoIndent,
    /// `!` — filter through external shell command. After the motion fixes the
    /// range the grammar transitions to `PendingFilter` and waits for the
    /// app to supply a command string before emitting `EngineCmd::ApplyFilter`.
    Filter,
    /// `gc` — toggle line comments on the range. `gcc` = current line (doubled-
    /// char convention, like `dd`). `gc{motion}` = motion variant. All three
    /// visual modes resolve to a row range and call `toggle_comment_range`.
    Comment,
}

impl OperatorKind {
    /// The doubled-letter char for this operator.
    ///
    /// Used by the `AfterOp` reducer arm to detect the line-op doubled form:
    /// `dd`, `yy`, `cc`, `>>`, `<<`, `gUU`, `guu`, `g~~`, `gqq`, `gcc`.
    /// Also used by the app's which-key popup to synthesise the prefix
    /// shown when an operator is pending its motion.
    pub fn double_char(self) -> char {
        match self {
            OperatorKind::Delete => 'd',
            OperatorKind::Yank => 'y',
            OperatorKind::Change => 'c',
            OperatorKind::Indent => '>',
            OperatorKind::Outdent => '<',
            OperatorKind::Uppercase => 'U',
            OperatorKind::Lowercase => 'u',
            OperatorKind::ToggleCase => '~',
            OperatorKind::Reflow => 'q',
            // `gww` — doubled 'w' after `gw` enters the keep-cursor reflow.
            OperatorKind::ReflowKeepCursor => 'w',
            OperatorKind::AutoIndent => '=',
            OperatorKind::Filter => '!',
            // `gcc` — doubled 'c' after `gc` enters the comment operator.
            OperatorKind::Comment => 'c',
        }
    }
}
