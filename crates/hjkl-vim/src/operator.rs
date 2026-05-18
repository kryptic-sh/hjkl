/// Operator identity carried by the reducer. Kept independent of
/// `hjkl-engine` so `hjkl-vim` has no upstream dependency.
///
/// The five operators that can be entered directly from Normal mode via bare
/// `d` / `y` / `c` / `>` / `<`, plus the four g-prefix case/reflow operators
/// (`gU` / `gu` / `g~` / `gq`) bridged through the reducer in chunk 2c-v.
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
    /// `gq` — reflow / format text.
    Reflow,
    /// `=` — auto-indent (v1 dumb shiftwidth bracket counting).
    AutoIndent,
}

impl OperatorKind {
    /// The doubled-letter char for this operator.
    ///
    /// Used by the `AfterOp` reducer arm to detect the line-op doubled form:
    /// `dd`, `yy`, `cc`, `>>`, `<<`, `gUU`, `guu`, `g~~`, `gqq`.
    pub(crate) fn double_char(self) -> char {
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
            OperatorKind::AutoIndent => '=',
        }
    }
}
