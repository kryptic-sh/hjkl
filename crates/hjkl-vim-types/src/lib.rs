//! Vim vocabulary types for the hjkl editor.

// ─── Modes & parser state ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
    /// Column-oriented selection (`Ctrl-V`). Unlike the other visual
    /// modes this one doesn't use tui-textarea's single-range selection
    /// — the block corners live in [`VimState::block_anchor`] and the
    /// live cursor. Operators read the rectangle off those two points.
    VisualBlock,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Pending {
    #[default]
    None,
    /// Operator seen; still waiting for a motion / text-object / double-op.
    /// `count1` is any count pressed before the operator.
    Op { op: Operator, count1: usize },
    /// Operator + 'i' or 'a' seen; waiting for the text-object character.
    OpTextObj {
        op: Operator,
        count1: usize,
        inner: bool,
    },
    /// Operator + 'g' seen (for `dgg`).
    OpG { op: Operator, count1: usize },
    /// Bare `g` seen in normal/visual — looking for `g`, `e`, `E`, …
    G,
    /// Bare `f`/`F`/`t`/`T` — looking for the target char.
    Find { forward: bool, till: bool },
    /// Operator + `f`/`F`/`t`/`T` — looking for target char.
    OpFind {
        op: Operator,
        count1: usize,
        forward: bool,
        till: bool,
    },
    /// `r` pressed — waiting for the replacement char.
    Replace,
    /// Visual mode + `i` or `a` pressed — waiting for the text-object
    /// character to extend the selection over.
    VisualTextObj { inner: bool },
    /// Bare `z` seen — looking for `z` (center), `t` (top), `b` (bottom).
    Z,
    /// `m` pressed — waiting for the mark letter to set.
    SetMark,
    /// `'` pressed — waiting for the mark letter to jump to its line
    /// (lands on first non-blank, linewise for operators).
    GotoMarkLine,
    /// `` ` `` pressed — waiting for the mark letter to jump to the
    /// exact `(row, col)` stored at set time (charwise for operators).
    GotoMarkChar,
    /// `"` pressed — waiting for the register selector. The next char
    /// (`a`–`z`, `A`–`Z`, `0`–`9`, or `"`) sets `pending_register`.
    SelectRegister,
    /// `q` pressed (not currently recording) — waiting for the macro
    /// register name. The macro records every key after the chord
    /// resolves, until a bare `q` ends the recording.
    RecordMacroTarget,
    /// `@` pressed — waiting for the macro register name to play.
    /// `count` is the prefix multiplier (`3@a` plays the macro 3
    /// times); 0 means "no prefix" and is treated as 1.
    PlayMacroTarget { count: usize },
    /// `[` pressed in Normal/Visual mode — waiting for the second key.
    /// Resolves `[[` → `SectionBackward`, `[]` → `SectionEndBackward`.
    SquareBracketOpen,
    /// `]` pressed in Normal/Visual mode — waiting for the second key.
    /// Resolves `]]` → `SectionForward`, `][` → `SectionEndForward`.
    SquareBracketClose,
    /// Operator + `[` pending — waiting for second key to pick section motion.
    OpSquareBracketOpen { op: Operator, count1: usize },
    /// Operator + `]` pending — waiting for second key to pick section motion.
    OpSquareBracketClose { op: Operator, count1: usize },
    /// `s` / `S` in Normal mode with `motion_sneak=true` — waiting for
    /// the first character of the two-char digraph.
    /// `forward=true` → `s`; `forward=false` → `S` (backward).
    SneakFirst { forward: bool, count: usize },
    /// First sneak char captured; waiting for the second char to complete
    /// the digraph and jump.
    SneakSecond {
        c1: char,
        forward: bool,
        count: usize,
    },
    /// Operator + `s` / `S` pending — waiting for the first char of the
    /// two-char sneak digraph (e.g. `d` then `s` then `a` then `b` = `dsab`).
    OpSneakFirst {
        op: Operator,
        count1: usize,
        forward: bool,
    },
    /// Operator + sneak first char captured; waiting for the second char.
    OpSneakSecond {
        op: Operator,
        count1: usize,
        c1: char,
        forward: bool,
    },
}

// ─── Operator / Motion / TextObject ────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
    Yank,
    /// `gU{motion}` — uppercase the range. Entered via the `g` prefix
    /// in normal mode or `U` in visual mode.
    Uppercase,
    /// `gu{motion}` — lowercase the range. `u` in visual mode.
    Lowercase,
    /// `g~{motion}` — toggle case of the range. `~` in visual mode
    /// (character at the cursor for the single-char `~` command stays
    /// its own code path in normal mode).
    ToggleCase,
    /// `>{motion}` — indent the line range by `shiftwidth` spaces.
    /// Always linewise, even when the motion is char-wise — mirrors
    /// vim's behaviour where `>w` indents the current line, not the
    /// word on it.
    Indent,
    /// `<{motion}` — outdent the line range (remove up to
    /// `shiftwidth` leading spaces per line).
    Outdent,
    /// `zf{motion}` / `zf{textobj}` / Visual `zf` — create a closed
    /// fold spanning the row range. Doesn't mutate the buffer text;
    /// cursor restores to the operator's start position.
    Fold,
    /// `gq{motion}` — reflow the row range to `settings.textwidth`.
    /// Greedy word-wrap: collapses each paragraph (blank-line-bounded
    /// run) into space-separated words, then re-emits lines whose
    /// width stays under `textwidth`. Always linewise, like indent.
    Reflow,
    /// `gw{motion}` — same reflow as `gq` but cursor stays at the
    /// pre-reflow `(row, col)`. If the reflow shrinks the line so the
    /// original col is past the new EOL, the col is clamped to the last
    /// char of the line (vim's behaviour). Always linewise.
    ReflowKeepCursor,
    /// `={motion}` — auto-indent the line range using shiftwidth-based
    /// bracket depth counting (v1 dumb reindent). Always linewise.
    /// See `auto_indent_range` for the algorithm and its limitations.
    AutoIndent,
    /// `!{motion}` — filter the line range through an external shell command.
    /// The range text is piped to the command's stdin; stdout replaces the
    /// range in the buffer. Non-zero exit or spawn failure returns an error
    /// to the caller without mutating the buffer.
    Filter,
    /// `gc{motion}` / `gcc` — toggle line comments on the row range.
    /// Dispatched through `Editor::toggle_comment_range` rather than the
    /// normal `run_operator_over_range` pipeline (same pattern as `Filter`).
    Comment,
    /// `g?{motion}` / `g??` / visual `g?` — ROT13 the range. Same operator
    /// shape as the case ops; only the per-char transform differs.
    Rot13,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    /// `<Space>` — right-motion that wraps to the next line at EOL (vim's
    /// default `whichwrap=b,s`). Distinct from `Right`/`l` which never wrap.
    SpaceFwd,
    /// `<BS>` — left-motion that wraps to the previous line's last char at BOL
    /// (`whichwrap=b`). Distinct from `Left`/`h` which never wrap.
    BackspaceBack,
    Up,
    Down,
    WordFwd,
    BigWordFwd,
    WordBack,
    BigWordBack,
    WordEnd,
    BigWordEnd,
    /// `ge` — backward word end.
    WordEndBack,
    /// `gE` — backward WORD end.
    BigWordEndBack,
    LineStart,
    FirstNonBlank,
    LineEnd,
    FileTop,
    FileBottom,
    Find {
        ch: char,
        forward: bool,
        till: bool,
    },
    FindRepeat {
        reverse: bool,
    },
    MatchBracket,
    /// `[(` / `])` / `[{` / `]}` — jump to the previous/next unmatched bracket
    /// of the given kind. `open` is the open char (`(` or `{`); `forward` picks
    /// the close (`)`/`}`) when true, the open when false.
    UnmatchedBracket {
        forward: bool,
        open: char,
    },
    WordAtCursor {
        forward: bool,
        /// `*` / `#` use `\bword\b` boundaries; `g*` / `g#` drop them so
        /// the search hits substrings (e.g. `foo` matches inside `foobar`).
        whole_word: bool,
    },
    /// `n` / `N` — repeat the last `/` or `?` search.
    SearchNext {
        reverse: bool,
    },
    /// `H` — cursor to viewport top (plus `count - 1` rows down).
    ViewportTop,
    /// `M` — cursor to viewport middle.
    ViewportMiddle,
    /// `L` — cursor to viewport bottom (minus `count - 1` rows up).
    ViewportBottom,
    /// `g_` — last non-blank char on the line.
    LastNonBlank,
    /// `gM` — cursor to the middle char column of the current line
    /// (`floor(chars / 2)`). Vim's variant ignoring screen wrap.
    LineMiddle,
    /// `gm` — cursor to the middle of the *screen* line: column
    /// `min(viewport_width / 2, last_col)`. Differs from `gM` (char-middle).
    ScreenLineMiddle,
    /// `{` — previous paragraph (preceding blank line, or top).
    ParagraphPrev,
    /// `}` — next paragraph (following blank line, or bottom).
    ParagraphNext,
    /// `(` — previous sentence boundary.
    SentencePrev,
    /// `)` — next sentence boundary.
    SentenceNext,
    /// `gj` — `count` visual rows down (one screen segment per step
    /// under `:set wrap`; falls back to `Down` otherwise).
    ScreenDown,
    /// `gk` — `count` visual rows up; mirror of [`Motion::ScreenDown`].
    ScreenUp,
    /// `[[` — backward to the previous `{` at column 0 (C section header).
    /// Charwise exclusive; count-aware.
    SectionBackward,
    /// `]]` — forward to the next `{` at column 0. Charwise exclusive.
    SectionForward,
    /// `[]` — backward to the previous `}` at column 0 (C section end).
    /// Charwise exclusive; count-aware.
    SectionEndBackward,
    /// `][` — forward to the next `}` at column 0. Charwise exclusive.
    SectionEndForward,
    /// `+` / `<CR>` — first non-blank of the next line. Linewise.
    FirstNonBlankNextLine,
    /// `-` — first non-blank of the previous line. Linewise.
    FirstNonBlankPrevLine,
    /// `_` — first non-blank of `count-1` lines down (count=1 = current line). Linewise.
    FirstNonBlankLine,
    /// `{count}|` — jump to column `count` on the current line (1-based;
    /// no count or count=0 → column 1 → index 0). Clamped to line length.
    GotoColumn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextObject {
    Word {
        big: bool,
    },
    Quote(char),
    Bracket(char),
    Paragraph,
    /// `it` / `at` — XML/HTML-style tag pair. `inner = true` covers
    /// content between `>` and `</`; `inner = false` covers the open
    /// tag through the close tag inclusive.
    XmlTag,
    /// `is` / `as` — sentence: a run ending at `.`, `?`, or `!`
    /// followed by whitespace or end-of-line. `inner = true` covers
    /// the sentence text only; `inner = false` includes trailing
    /// whitespace.
    Sentence,
}

/// Classification determines how operators treat the range end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeKind {
    /// Range end is exclusive (end column not included). Typical: h, l, w, 0, $.
    Exclusive,
    /// Range end is inclusive. Typical: e, f, t, %.
    Inclusive,
    /// Whole lines from top row to bottom row. Typical: j, k, gg, G.
    Linewise,
}

// ─── Dot-repeat storage ────────────────────────────────────────────────────

/// Information needed to replay a mutating change via `.`.
#[derive(Debug, Clone)]
pub enum LastChange {
    /// Operator over a motion.
    OpMotion {
        op: Operator,
        motion: Motion,
        count: usize,
        inserted: Option<String>,
    },
    /// Operator over a text-object.
    OpTextObj {
        op: Operator,
        obj: TextObject,
        inner: bool,
        inserted: Option<String>,
    },
    /// `dd`, `cc`, `yy` with a count.
    LineOp {
        op: Operator,
        count: usize,
        inserted: Option<String>,
        /// The explicit register (`"add`, `"ayy`, ...) the original change
        /// used, if any. `.` must reuse it (`:h redo-register`) rather than
        /// falling back to the unnamed register.
        register: Option<char>,
    },
    /// `x`, `X` with a count.
    CharDel { forward: bool, count: usize },
    /// `r<ch>` with a count.
    ReplaceChar { ch: char, count: usize },
    /// `~` with a count.
    ToggleCase { count: usize },
    /// `J` with a count.
    JoinLine { count: usize },
    /// `p` / `P` (and `gp`/`gP`, `]p`/`[p`) with a count.
    Paste {
        before: bool,
        count: usize,
        /// `gp` / `gP` — leave the cursor just after the pasted text.
        cursor_after: bool,
        /// `]p` / `[p` — reindent the pasted block to the current line.
        reindent: bool,
    },
    /// `D` (delete to EOL).
    DeleteToEol { inserted: Option<String> },
    /// `o` / `O` + the inserted text.
    OpenLine { above: bool, inserted: String },
    /// `i`/`I`/`a`/`A` + inserted text.
    InsertAt {
        entry: InsertEntry,
        inserted: String,
        count: usize,
    },
    /// `dgn` / `cgn` (and `gN` forms) — operate on the next search match.
    /// `inserted` is filled on Esc for the `cgn` change form so `.` retypes it.
    GnOp {
        op: Operator,
        forward: bool,
        inserted: Option<String>,
    },
    /// `R{text}<Esc>` — replace (overstrike) mode. `.` re-overtypes `text`.
    ReplaceMode { text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertEntry {
    I,
    A,
    ShiftI,
    ShiftA,
}

/// Tracks which kind of horizontal jump was last performed so `;` / `,`
/// can dispatch to the correct repeat handler.
///
/// - `FindChar` — last horizontal motion was `f`/`F`/`t`/`T`; `;`/`,`
///   repeats via `Motion::FindRepeat`.
/// - `Sneak` — last horizontal motion was `s`/`S` sneak; `;`/`,` repeats
///   via `apply_sneak` with the stored digraph.
/// - `None` — no horizontal motion yet; `;`/`,` are no-ops for both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LastHorizontalMotion {
    #[default]
    None,
    FindChar,
    Sneak,
}

#[derive(Debug, Clone)]
pub struct InsertSession {
    pub count: usize,
    /// Min/max row visited during this session. Widens on every key.
    pub row_min: usize,
    pub row_max: usize,
    /// O(1) rope snapshot of the full buffer at session entry. Used to
    /// diff the affected row window at finish without being fooled by
    /// cursor navigation through rows the user never edited.
    /// `ropey::Rope::clone` is Arc-clone — no byte copying.
    pub before_rope: ropey::Rope,
    pub reason: InsertReason,
    /// (row, col) where the insert session began (char-indexed). Abbreviation
    /// expansion uses `start_col` as `mincol` — only chars at or after this
    /// column on `start_row` are eligible as part of the `lhs` match, so
    /// pre-existing buffer text is never consumed by expansion.
    pub start_row: usize,
    pub start_col: usize,
}

#[derive(Debug, Clone)]
pub enum InsertReason {
    /// Plain entry via i/I/a/A — recorded as `InsertAt`.
    Enter(InsertEntry),
    /// Entry via `o`/`O` — records OpenLine on Esc.
    Open { above: bool },
    /// Entry via an operator's change side-effect. Retro-fills the
    /// stored last-change's `inserted` field on Esc.
    AfterChange,
    /// Entry via `C` (delete to EOL + insert).
    DeleteToEol,
    /// Entry via an insert triggered during dot-replay — don't touch
    /// last_change because the outer replay will restore it.
    ReplayOnly,
    /// `I` or `A` from VisualBlock: insert the typed text at `col` on
    /// rows in `top..=bot`. `col` is the start column for `I`, the
    /// one-past-block-end column for `A`.
    ///
    /// `pad` distinguishes the two vim behaviours at rows shorter than
    /// `col` (`:h v_b_I` vs `:h v_b_A`): `A` pads short rows with spaces
    /// so the appended text still lines up (`pad: true`); `I` skips rows
    /// that don't reach `col` entirely — no padding, no insert on that
    /// row (`pad: false`).
    BlockEdge {
        top: usize,
        bot: usize,
        col: usize,
        pad: bool,
    },
    /// `c` from VisualBlock: block content deleted, then user types
    /// replacement text replicated across all block rows on Esc. Cursor
    /// advances to the last typed char after replication (unlike BlockEdge
    /// which leaves cursor at the insertion column).
    BlockChange { top: usize, bot: usize, col: usize },
    /// `R` — Replace mode. Each typed char overwrites the cell under
    /// the cursor instead of inserting; at end-of-line the session
    /// falls through to insert (same as vim).
    Replace,
}

/// Saved visual-mode anchor + cursor for `gv` (re-enters the last
/// visual selection). `mode` carries which visual flavour to
/// restore; `anchor` / `cursor` mean different things per flavour:
///
/// - `Visual`     — `anchor` is the char-wise visual anchor.
/// - `VisualLine` — `anchor.0` is the `visual_line_anchor` row;
///   `anchor.1` is unused.
/// - `VisualBlock`— `anchor` is `block_anchor`, `block_vcol` is the
///   sticky vcol that survives j/k clamping.
#[derive(Debug, Clone, Copy)]
pub struct LastVisual {
    pub mode: Mode,
    pub anchor: (usize, usize),
    pub cursor: (usize, usize),
    pub block_vcol: usize,
}
