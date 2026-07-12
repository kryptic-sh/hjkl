//! Engine-owned abbreviation types.

/// A single abbreviation entry (insert-mode or cmdline-mode, recursive or noremap).
///
/// Mode flags: `insert` = expand in Insert mode, `cmdline` = expand in Cmdline mode.
/// `noremap` stores whether the definition was made with `noreabbrev`; expansion
/// is always literal text regardless of this flag, but it is preserved for future use.
///
/// NOTE: Abbreviations are currently per-editor (global in vim would share across
/// buffers; per-editor is equivalent for single-buffer use and is acceptable for
/// now — cross-buffer global behaviour is a follow-up).
#[derive(Debug, Clone)]
pub struct Abbrev {
    pub lhs: String,
    pub rhs: String,
    pub insert: bool,
    pub cmdline: bool,
    pub noremap: bool,
}

/// Trigger kind for abbreviation expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbbrevTrigger {
    /// A non-keyword character was typed (e.g. space, punctuation).
    NonKeyword(char),
    /// `<C-]>` was pressed — expand without inserting any character.
    CtrlBracket,
    /// `<CR>` (Enter) was pressed.
    Cr,
    /// `<Esc>` was pressed to leave insert mode.
    Esc,
}

/// Classify a vim abbreviation lhs into its type.
///
/// - **Full**: every char in `lhs` is a keyword char (full-id).
/// - **End**: the last char is a keyword char, at least one other is not (end-id).
/// - **None**: the last char is a non-keyword char (non-id).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbbrevKind {
    /// All keyword chars (full-id).
    Full,
    /// Last char keyword, others include non-keyword (end-id).
    End,
    /// Last char is non-keyword (non-id).
    NonKw,
}
