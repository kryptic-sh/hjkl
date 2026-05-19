/// Cursor motion identity for the hjkl keymap layer.
///
/// Moved from `hjkl-vim` into `hjkl-engine` (Phase 6.6 cycle-break) so that
/// `hjkl-vim` can depend on `hjkl-engine` without a circular dependency.
/// `hjkl-vim` re-exports this type as `hjkl_vim::MotionKind` for back-compat.
///
/// The host converts a `MotionKind` to the appropriate `Editor::apply_motion`
/// call. Designed for extensibility: future phases may add variants without
/// breaking existing match arms — callers must use `..` or add the new arms
/// when they bump the hjkl-engine minor version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MotionKind {
    /// `h` / `<Backspace>` — move the cursor one character to the left.
    /// Clamps at column 0 (no line-wrap), matching vim's normal-mode `h`.
    CharLeft,
    /// `l` / `<Space>` — move the cursor one character to the right.
    /// Clamps at the last character of the line (no line-wrap), matching
    /// vim's normal-mode `l`.
    CharRight,
    /// `j` — move the cursor one line down, restoring the sticky column.
    LineDown,
    /// `k` — move the cursor one line up, restoring the sticky column.
    LineUp,
    /// `+` / `<CR>` (not yet bound) — move down one line and land on the
    /// first non-blank character. Sets the sticky column.
    FirstNonBlankDown,
    /// `-` — move up one line and land on the first non-blank character.
    /// Sets the sticky column.
    FirstNonBlankUp,
    /// `w` — move the cursor forward to the start of the next small word.
    /// Counts repeat the motion; wraps across lines matching vim's `w`.
    WordForward,
    /// `W` — move the cursor forward to the start of the next BIG word
    /// (whitespace-delimited). Counts repeat; wraps across lines.
    BigWordForward,
    /// `b` — move the cursor backward to the start of the current or previous
    /// small word. Counts repeat; wraps across lines matching vim's `b`.
    WordBackward,
    /// `B` — move the cursor backward to the start of the current or previous
    /// BIG word (whitespace-delimited). Counts repeat; wraps across lines.
    BigWordBackward,
    /// `e` — move the cursor forward to the end of the current or next small
    /// word. Counts repeat; wraps across lines matching vim's `e`.
    WordEnd,
    /// `E` — move the cursor forward to the end of the current or next BIG
    /// word (whitespace-delimited). Counts repeat; wraps across lines.
    BigWordEnd,
    /// `0` / `<Home>` — move the cursor to the first column of the current
    /// line (column 0). Count is ignored (vim `0` is always a plain motion).
    LineStart,
    /// `^` — move the cursor to the first non-blank character on the current
    /// line. On a blank/all-whitespace line, lands at column 0.
    FirstNonBlank,
    /// `$` / `<End>` — move the cursor to the last character on the current
    /// line. On an empty line, stays at column 0. Count-aware: `count$`
    /// moves down `count-1` lines, then lands at the end of that line.
    LineEnd,
    /// `G` — go to line. Count semantics match vim:
    /// - count 0 or 1 (bare `G`) → last line of buffer.
    /// - count > 1 → jump to that line number (1-based).
    ///
    /// Note: `gg` (first line) is dispatched via the G-chord path
    /// (`Editor::after_g`), not via this variant.
    GotoLine,
    /// `;` — repeat last `f`/`F`/`t`/`T` in the same direction.
    /// No-op if no prior find exists.
    FindRepeat,
    /// `,` — repeat last `f`/`F`/`t`/`T` in the reverse direction.
    /// No-op if no prior find exists.
    FindRepeatReverse,
    /// `%` — jump to matching bracket. With a count `N%`, vim normally
    /// jumps to the `N%` line of the file (percentage). The engine
    /// currently implements only the matching-bracket semantic; count is
    /// passed through and handled engine-side. Bracket types: `()`, `[]`,
    /// `{}`, plus C-style block comments `/* */` (engine detail).
    BracketMatch,
    /// `H` — jump to the top of the visible viewport. With a count, moves
    /// to the viewport top and then `count - 1` rows further down (matching
    /// vim's `H` count semantics). Lands on the first non-blank character.
    ViewportTop,
    /// `M` — jump to the middle row of the visible viewport. Count is
    /// ignored (vim's `M` is always a plain motion). Lands on the first
    /// non-blank character.
    ViewportMiddle,
    /// `L` — jump to the bottom of the visible viewport. With a count,
    /// moves to the viewport bottom and then `count - 1` rows further up
    /// (matching vim's `L` count semantics). Lands on the first non-blank
    /// character.
    ViewportBottom,
    /// `<C-d>` — move the cursor half a page down. Count multiplies the
    /// half-page distance (e.g. `2<C-d>` = one full page). Lands on the
    /// first non-blank of the target row.
    HalfPageDown,
    /// `<C-u>` — move the cursor half a page up. Count multiplies the
    /// half-page distance. Lands on the first non-blank of the target row.
    HalfPageUp,
    /// `<C-f>` — move the cursor a full page down (with 2-line overlap).
    /// Count multiplies the full-page distance. Lands on the first non-blank
    /// of the target row.
    FullPageDown,
    /// `<C-b>` — move the cursor a full page up (with 2-line overlap).
    /// Count multiplies the full-page distance. Lands on the first non-blank
    /// of the target row.
    FullPageUp,
    /// `_` — first non-blank of `count-1` lines below the cursor (count=1 = current line).
    /// Linewise. Vim's `_` motion.
    FirstNonBlankLine,
    /// `[[` — backward to the previous `{` at column 0 (C section header).
    /// Charwise exclusive; count-aware.
    SectionBackward,
    /// `]]` — forward to the next `{` at column 0. Charwise exclusive; count-aware.
    SectionForward,
    /// `[]` — backward to the previous `}` at column 0 (C section end).
    /// Charwise exclusive; count-aware.
    SectionEndBackward,
    /// `][` — forward to the next `}` at column 0. Charwise exclusive; count-aware.
    SectionEndForward,
}
