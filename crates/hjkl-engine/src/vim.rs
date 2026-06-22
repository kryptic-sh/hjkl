//! Vim-mode engine.
//!
//! Implements a command grammar of the form
//!
//! ```text
//! Command := count? (operator count? (motion | text-object)
//!                   | motion
//!                   | insert-entry
//!                   | misc)
//! ```
//!
//! The parser is a small state machine driven by one `Input` at a time.
//! Motions and text objects produce a [`Range`] (with inclusive/exclusive
//! / linewise classification). A single [`Operator`] implementation
//! applies a range — so `dw`, `d$`, `daw`, and visual `d` all go through
//! the same code path.
//!
//! The most recent mutating command is stored in
//! [`VimState::last_change`] so `.` can replay it.
//!
//! # Roadmap
//!
//! Tracked in the original plan at
//! `~/.claude/plans/look-at-the-vim-curried-fern.md`. Phases still
//! outstanding — each one can land as an isolated PR.
//!
//! ## P3 — Registers & marks
//!
//! - TODO: `RegisterBank` indexed by char:
//!     - unnamed `""`, last-yank `"0`, small-delete `"-`
//!     - named `"a-"z` (uppercase `"A-"Z` appends instead of overwriting)
//!     - blackhole `"_`
//!     - system clipboard `"+` / `"*` (wire to `crate::clipboard::Clipboard`)
//!     - read-only `":`, `".`, `"%` — surface in `:reg` output
//! - TODO: route every yank / cut / paste through the bank. Parser needs
//!   a `"{reg}` prefix state that captures the target register before a
//!   count / operator.
//! - TODO: `m{a-z}` sets a mark in a `HashMap<char, (buffer_id, row, col)>`;
//!   `'x` jumps to the line (FirstNonBlank), `` `x `` to the exact cell.
//!   Uppercase marks are global across tabs; lowercase are per-buffer.
//! - TODO: `''` and `` `` `` jump to the last-jump position; `'[` `']`
//!   `'<` `'>` bound the last change / visual region.
//! - TODO: `:reg` and `:marks` ex commands.
//!
//! ## P4 — Macros
//!
//! - TODO: `q{a-z}` starts recording raw `Input`s into the register;
//!   next `q` stops.
//! - TODO: `@{a-z}` replays the register by re-feeding inputs through
//!   `step`. `@@` repeats the last macro. Nested macros need a sane
//!   depth cap (e.g. 100) to avoid runaway loops.
//! - TODO: ensure recording doesn't capture the initial `q{a-z}` itself.
//!
//! ## P6 — Polish (still outstanding)
//!
//! - TODO: indent operators `>` / `<` (with line + text-object targets).
//! - TODO: format operator `=` — map to whatever SQL formatter we wire
//!   up; for now stub that returns the range unchanged with a toast.
//! - TODO: case operators `gU` / `gu` / `g~` on a range (already have
//!   single-char `~`).
//! - TODO: screen motions `H` / `M` / `L` once we track the render
//!   viewport height inside Editor.
//! - TODO: scroll-to-cursor motions `zz` / `zt` / `zb`.
//!
//! ## Known substrate / divergence notes
//!
//! - TODO: insert-mode indent helpers — `Ctrl-t` / `Ctrl-d` (increase /
//!   decrease indent on current line) and `Ctrl-r <reg>` (paste from a
//!   register). `Ctrl-r` needs the `RegisterBank` from P3 to be useful.
//! - TODO: `/` and `?` search prompts still live in `the host/src/lib.rs`.
//!   The plan calls for moving them into the editor (so the editor owns
//!   `last_search_pattern` rather than the TUI loop). Safe to defer.

use crate::VimMode;
use crate::input::{Input, Key};

use crate::buf_helpers::{
    buf_cursor_pos, buf_line, buf_line_bytes, buf_line_chars, buf_row_count, buf_set_cursor_pos,
    buf_set_cursor_rc,
};
use crate::editor::Editor;

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

/// ROT13 a string: rotate ASCII letters by 13, leave everything else.
pub(crate) fn rot13_str(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a' + 13) % 26) + b'a') as char,
            'A'..='Z' => (((c as u8 - b'A' + 13) % 26) + b'A') as char,
            _ => c,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
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

// ─── VimState ──────────────────────────────────────────────────────────────

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

#[derive(Default)]
pub struct VimState {
    /// Internal FSM mode. Kept in sync with `current_mode` after every
    /// `step`. Phase 6.6b: promoted from private to `pub` so the FSM
    /// body (moving to hjkl-vim in 6.6c–6.6g) can read/write it directly
    /// until the migration is complete.
    pub mode: Mode,
    /// Two-key chord in progress. `Pending::None` when idle.
    pub pending: Pending,
    /// Digit prefix accumulated before an operator or motion. `0` means
    /// no prefix was typed (treated as 1 by most commands).
    pub count: usize,
    /// Last `f`/`F`/`t`/`T` target, for `;` / `,` repeat.
    pub last_find: Option<(char, bool, bool)>,
    /// Most-recent mutating command for `.` dot-repeat.
    pub last_change: Option<LastChange>,
    /// Captured on insert-mode entry: count, buffer snapshot, entry kind.
    pub insert_session: Option<InsertSession>,
    /// (row, col) anchor for char-wise Visual mode. Set on entry, used
    /// to compute the highlight range and the operator range without
    /// relying on tui-textarea's live selection.
    pub visual_anchor: (usize, usize),
    /// Row anchor for VisualLine mode.
    pub visual_line_anchor: usize,
    /// (row, col) anchor for VisualBlock mode. The live cursor is the
    /// opposite corner.
    pub block_anchor: (usize, usize),
    /// Intended "virtual" column for the block's active corner. j/k
    /// clamp cursor.col to shorter rows, which would collapse the
    /// block across ragged content — so we remember the desired column
    /// separately and use it for block bounds / insert-column
    /// computations. Updated by h/l only.
    pub block_vcol: usize,
    /// Track whether the last yank/cut was linewise (drives `p`/`P` layout).
    pub yank_linewise: bool,
    /// Active register selector — set by `"reg` prefix, consumed by
    /// the next y / d / c / p. `None` falls back to the unnamed `"`.
    pub pending_register: Option<char>,
    /// Recording target — set by `q{reg}`, cleared by a bare `q`.
    /// While `Some`, every consumed `Input` is appended to
    /// `recording_keys`.
    pub recording_macro: Option<char>,
    /// Keys recorded into the in-progress macro. On `q` finish, these
    /// are encoded via [`crate::input::encode_macro`] and written to
    /// the matching named register slot, so macros and yanks share a
    /// single store.
    pub recording_keys: Vec<crate::input::Input>,
    /// Set during `@reg` replay so the recorder doesn't capture the
    /// replayed keystrokes a second time.
    pub replaying_macro: bool,
    /// Last register played via `@reg`. `@@` re-plays this one.
    pub last_macro: Option<char>,
    /// Position of the most recent buffer mutation. Surfaced via
    /// the `'.` / `` `. `` marks for quick "back to last edit".
    pub last_edit_pos: Option<(usize, usize)>,
    /// Position where the cursor was when insert mode last exited (Esc).
    /// Used by `gi` to return to the exact (row, col) where the user
    /// last typed, matching vim's `:h gi`.
    pub last_insert_pos: Option<(usize, usize)>,
    /// Bounded ring of recent edit positions (newest at the back).
    /// `g;` walks toward older entries, `g,` toward newer ones. Capped
    /// at [`CHANGE_LIST_MAX`].
    pub change_list: Vec<(usize, usize)>,
    /// Index into `change_list` while walking. `None` outside a walk —
    /// any new edit clears it (and trims forward entries past it).
    pub change_list_cursor: Option<usize>,
    /// Snapshot of the last visual selection for `gv` re-entry.
    /// Stored on every Visual / VisualLine / VisualBlock exit.
    pub last_visual: Option<LastVisual>,
    /// `zz` / `zt` / `zb` set this so the end-of-step scrolloff
    /// pass doesn't override the user's explicit viewport pinning.
    /// Cleared every step.
    pub viewport_pinned: bool,
    /// Set while replaying `.` / last-change so we don't re-record it.
    pub replaying: bool,
    /// Entered Normal from Insert via `Ctrl-o`; after the next complete
    /// normal-mode command we return to Insert.
    pub one_shot_normal: bool,
    /// Live `/` or `?` prompt. `None` outside search-prompt mode.
    pub search_prompt: Option<SearchPrompt>,
    /// Most recent committed search pattern. Surfaced to host apps via
    /// [`Editor::last_search`] so their status line can render a hint
    /// and so `n` / `N` have something to repeat.
    pub last_search: Option<String>,
    /// Direction of the last committed search. `n` repeats this; `N`
    /// inverts it. Defaults to forward so a never-searched buffer's
    /// `n` still walks downward.
    pub last_search_forward: bool,
    /// Text of the most recent insert session — vim's `".` register, pasted
    /// via `<C-r>.` in insert mode (and `".p` in normal mode).
    pub last_insert_text: Option<String>,
    /// Back half of the jumplist — `Ctrl-o` pops from here. Populated
    /// with the pre-motion cursor when a "big jump" motion fires
    /// (`gg`/`G`, `%`, `*`/`#`, `n`/`N`, `H`/`M`/`L`, committed `/` or
    /// `?`). Capped at 100 entries.
    pub jump_back: Vec<(usize, usize)>,
    /// Forward half — `Ctrl-i` pops from here. Cleared by any new big
    /// jump, matching vim's "branch off trims forward history" rule.
    pub jump_fwd: Vec<(usize, usize)>,
    /// Set by `Ctrl-R` in insert mode while waiting for the register
    /// selector. The next typed char names the register; its contents
    /// are inserted inline at the cursor and the flag clears.
    pub insert_pending_register: bool,
    /// Stashed start position for the `[` mark on a Change operation.
    /// Set to `top` before the cut in `run_operator_over_range` (Change
    /// arm); consumed by `finish_insert_session` on Esc-from-insert
    /// when the reason is `AfterChange`. Mirrors vim's `:h '[` / `:h ']`
    /// rule that `[` = start of change, `]` = last typed char on exit.
    pub change_mark_start: Option<(usize, usize)>,
    /// Bounded history of committed `/` / `?` search patterns. Newest
    /// entries are at the back; capped at [`SEARCH_HISTORY_MAX`] to
    /// avoid unbounded growth on long sessions.
    pub search_history: Vec<String>,
    /// Index into `search_history` while the user walks past patterns
    /// in the prompt via `Ctrl-P` / `Ctrl-N`. `None` outside that walk
    /// — typing or backspacing in the prompt resets it so the next
    /// `Ctrl-P` starts from the most recent entry again.
    pub search_history_cursor: Option<usize>,
    /// Wall-clock instant of the last keystroke. Drives the
    /// `:set timeoutlen` multi-key timeout — if `now() - last_input_at`
    /// exceeds the configured budget, any pending prefix is cleared
    /// before the new key dispatches. `None` before the first key.
    /// 0.0.29 (Patch B): `:set timeoutlen` math now reads
    /// [`crate::types::Host::now`] via `last_input_host_at`. This
    /// `Instant`-flavoured field stays for snapshot tests that still
    /// observe it directly.
    pub last_input_at: Option<std::time::Instant>,
    /// `Host::now()` reading at the last keystroke. Drives
    /// `:set timeoutlen` so macro replay / headless drivers stay
    /// deterministic regardless of wall-clock skew.
    pub last_input_host_at: Option<core::time::Duration>,
    /// Canonical current mode. Mirrors `mode` (the FSM-internal field)
    /// AND is written by every Phase 6.3 primitive (`set_mode`,
    /// `enter_visual_char_bridge`, …). Once the FSM is gone this is the
    /// sole source of truth; until then both fields are kept in sync.
    /// Initialized to `Normal` via `#[derive(Default)]`.
    pub(crate) current_mode: crate::VimMode,
    /// Read-only view overlay layered over `current_mode` (git blame, …).
    /// Orthogonal to the input mode: while `Blame`, input is still
    /// interpreted as Normal but mutations are blocked and the host renders
    /// the overlay. Auto-reset to `Normal` whenever the input mode leaves
    /// `Normal` (see `drop_blame_if_left_normal`). Initialized to `Normal`.
    pub(crate) view: crate::ViewMode,
    /// Most recent successful :s invocation. Stored so :& / :&& can repeat it.
    pub last_substitute: Option<crate::substitute::SubstituteCmd>,
    /// Stack of auto-inserted closing characters awaiting skip-over.
    ///
    /// Each entry `(row, col, ch)` records where autopair placed a close
    /// character. When the next typed char matches `ch` AND the cursor is
    /// immediately before that position, the engine advances past it
    /// ("skip-over") instead of inserting. The stack is cleared on any
    /// cursor motion, mode change, or out-of-pair edit.
    pub pending_closes: Vec<(usize, usize, char)>,
    /// Last sneak digraph and direction: `Some(((c1, c2), forward))`.
    /// Used by `;` / `,` sneak-repeat when `last_horizontal_motion == Sneak`.
    pub last_sneak: Option<((char, char), bool)>,
    /// Tracks which kind of horizontal motion was last performed, so `;` / `,`
    /// can dispatch to sneak-repeat vs. find-char-repeat as appropriate.
    pub last_horizontal_motion: LastHorizontalMotion,
    /// Insert-mode (and cmdline-mode) abbreviations. Populated by `:abbreviate`,
    /// `:iabbrev`, `:cabbrev`, `:noreabbrev`, etc. Empty by default.
    pub abbrevs: Vec<Abbrev>,
}

pub(crate) const SEARCH_HISTORY_MAX: usize = 100;
pub(crate) const CHANGE_LIST_MAX: usize = 100;

/// Active `/` or `?` search prompt. Text mutations drive the textarea's
/// live search pattern so matches highlight as the user types.
#[derive(Debug, Clone)]
pub struct SearchPrompt {
    pub text: String,
    pub cursor: usize,
    pub forward: bool,
    /// Operator-pending search (`d/pat`, `c/pat`, `y/pat`): the operator, its
    /// count, and the cursor position where the operator started. `None` for a
    /// plain `/` / `?` search. On commit the operator runs over the (exclusive,
    /// charwise) range from `origin` to the match.
    pub operator: Option<(Operator, usize, (usize, usize))>,
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
    /// every row in `top..=bot`. `col` is the start column for `I`, the
    /// one-past-block-end column for `A`.
    BlockEdge { top: usize, bot: usize, col: usize },
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

impl VimState {
    pub fn public_mode(&self) -> VimMode {
        match self.mode {
            Mode::Normal => VimMode::Normal,
            Mode::Insert => VimMode::Insert,
            Mode::Visual => VimMode::Visual,
            Mode::VisualLine => VimMode::VisualLine,
            Mode::VisualBlock => VimMode::VisualBlock,
        }
    }

    pub fn force_normal(&mut self) {
        self.mode = Mode::Normal;
        self.pending = Pending::None;
        self.count = 0;
        self.insert_session = None;
        // Phase 6.3: keep current_mode in sync for callers that bypass step().
        self.current_mode = crate::VimMode::Normal;
    }

    /// Reset every prefix-tracking field so the next keystroke starts
    /// a fresh sequence. Drives `:set timeoutlen` — when the user
    /// pauses past the configured budget, `hjkl_vim::dispatch_input` calls
    /// this before dispatching the new key.
    ///
    /// Resets: `pending`, `count`, `pending_register`,
    /// `insert_pending_register`. Does NOT touch `mode`,
    /// `insert_session`, marks, jump list, or visual anchors —
    /// those aren't part of the in-flight chord.
    pub(crate) fn clear_pending_prefix(&mut self) {
        self.pending = Pending::None;
        self.count = 0;
        self.pending_register = None;
        self.insert_pending_register = false;
    }

    /// Widen the active insert session's row window to include `row`. Called
    /// by the Phase 6.1 public `Editor::insert_*` methods after each
    /// mutation so `finish_insert_session` diffs the right range on Esc.
    /// No-op when no insert session is active (e.g. calling from Normal mode).
    pub(crate) fn widen_insert_row(&mut self, row: usize) {
        if let Some(ref mut session) = self.insert_session {
            session.row_min = session.row_min.min(row);
            session.row_max = session.row_max.max(row);
        }
    }

    pub fn is_visual(&self) -> bool {
        matches!(
            self.mode,
            Mode::Visual | Mode::VisualLine | Mode::VisualBlock
        )
    }

    pub fn is_visual_char(&self) -> bool {
        self.mode == Mode::Visual
    }

    /// The pending repeat count (typed digits before a motion/operator),
    /// or `None` when no digits are pending. Zero is treated as absent.
    pub(crate) fn pending_count_val(&self) -> Option<u32> {
        if self.count == 0 {
            None
        } else {
            Some(self.count as u32)
        }
    }

    /// `true` when an in-flight chord is awaiting more keys. Inverse of
    /// `matches!(self.pending, Pending::None)`.
    pub(crate) fn is_chord_pending(&self) -> bool {
        !matches!(self.pending, Pending::None)
    }

    /// Return a single char representing the pending operator, if any.
    /// Used by host apps (status line "showcmd" area) to display e.g.
    /// `d`, `y`, `c` while waiting for a motion.
    pub(crate) fn pending_op_char(&self) -> Option<char> {
        let op = match &self.pending {
            Pending::Op { op, .. }
            | Pending::OpTextObj { op, .. }
            | Pending::OpG { op, .. }
            | Pending::OpFind { op, .. }
            | Pending::OpSquareBracketOpen { op, .. }
            | Pending::OpSquareBracketClose { op, .. } => Some(*op),
            _ => None,
        };
        op.map(|o| match o {
            Operator::Delete => 'd',
            Operator::Change => 'c',
            Operator::Yank => 'y',
            Operator::Uppercase => 'U',
            Operator::Lowercase => 'u',
            Operator::ToggleCase => '~',
            Operator::Indent => '>',
            Operator::Outdent => '<',
            Operator::Fold => 'z',
            Operator::Reflow => 'q',
            Operator::ReflowKeepCursor => 'w',
            Operator::AutoIndent => '=',
            Operator::Filter => '!',
            // `gc` prefix — doubled as `gcc`.
            Operator::Comment => 'c',
            // `g?` prefix — doubled as `g??`.
            Operator::Rot13 => '?',
        })
    }
}

// ─── Entry point ───────────────────────────────────────────────────────────

/// Open the `/` (forward) or `?` (backward) search prompt. Clears any
/// live search highlight until the user commits a query. `last_search`
/// is preserved so an empty `<CR>` can re-run the previous pattern.
pub(crate) fn enter_search<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    forward: bool,
) {
    ed.vim.search_prompt = Some(SearchPrompt {
        text: String::new(),
        cursor: 0,
        forward,
        operator: None,
    });
    ed.vim.search_history_cursor = None;
    // 0.0.37: clear via the engine search state (the buffer-side
    // bridge from 0.0.35 was removed in this patch — the `BufferView`
    // renderer reads the pattern from `Editor::search_state()`).
    ed.set_search_pattern(None);
}

/// `d/pat` / `c/pat` / `y/pat` (and `?` forms) — open the search prompt in
/// operator-pending mode. On commit the operator runs over the exclusive
/// charwise range from the current cursor to the match.
pub(crate) fn enter_search_op<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    forward: bool,
    op: Operator,
    count: usize,
) {
    let origin = ed.cursor();
    ed.vim.search_prompt = Some(SearchPrompt {
        text: String::new(),
        cursor: 0,
        forward,
        operator: Some((op, count.max(1), origin)),
    });
    ed.vim.search_history_cursor = None;
    ed.set_search_pattern(None);
}

/// Apply a pending operator-search over the exclusive charwise range from
/// `origin` to the current (post-search) cursor. Used by the search-prompt
/// commit path for `d/` / `c/` / `y/`.
pub(crate) fn apply_op_search_range<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    origin: (usize, usize),
) {
    let target = ed.cursor();
    run_operator_over_range(ed, op, origin, target, RangeKind::Exclusive);
}

/// `g;` / `g,` body. `dir = -1` walks toward older entries (g;),
/// `dir = 1` toward newer (g,). `count` repeats the step. Stops at
/// the ends of the ring; off-ring positions are silently ignored.
fn walk_change_list<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    dir: isize,
    count: usize,
) {
    if ed.vim.change_list.is_empty() {
        return;
    }
    let len = ed.vim.change_list.len();
    let mut idx: isize = match (ed.vim.change_list_cursor, dir) {
        (None, -1) => len as isize - 1,
        (None, 1) => return, // already past the newest entry
        (Some(i), -1) => i as isize - 1,
        (Some(i), 1) => i as isize + 1,
        _ => return,
    };
    for _ in 1..count {
        let next = idx + dir;
        if next < 0 || next >= len as isize {
            break;
        }
        idx = next;
    }
    if idx < 0 || idx >= len as isize {
        return;
    }
    let idx = idx as usize;
    ed.vim.change_list_cursor = Some(idx);
    let (row, col) = ed.vim.change_list[idx];
    ed.jump_cursor(row, col);
}

/// `Ctrl-R {reg}` body — insert the named register's contents at the
/// cursor as charwise text. Embedded newlines split lines naturally via
/// `Edit::InsertStr`. Unknown selectors and empty slots are no-ops so
/// stray keystrokes don't mutate the buffer.
fn insert_register_text<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    selector: char,
) {
    use hjkl_buffer::Edit;
    // Special read-only registers: `/` = last search pattern, `.` = last
    // inserted text. Fall back to the register store for everything else.
    let text = match selector {
        '/' => match &ed.vim.last_search {
            Some(s) if !s.is_empty() => s.clone(),
            _ => return,
        },
        '.' => match &ed.vim.last_insert_text {
            Some(s) if !s.is_empty() => s.clone(),
            _ => return,
        },
        _ => match ed.registers().read(selector) {
            Some(slot) if !slot.text.is_empty() => slot.text.clone(),
            _ => return,
        },
    };
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(&ed.buffer);
    ed.mutate_edit(Edit::InsertStr {
        at: cursor,
        text: text.clone(),
    });
    // Advance cursor to the end of the inserted payload — multi-line
    // pastes land on the last inserted row at the post-text column.
    let mut row = cursor.row;
    let mut col = cursor.col;
    for ch in text.chars() {
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    buf_set_cursor_rc(&mut ed.buffer, row, col);
    ed.push_buffer_cursor_to_textarea();
    ed.mark_content_dirty();
    if let Some(ref mut session) = ed.vim.insert_session {
        session.row_min = session.row_min.min(row);
        session.row_max = session.row_max.max(row);
    }
}

/// Compute the indent string to insert at the start of a new line
/// after Enter is pressed at `cursor`. Walks the smartindent rules:
///
/// - autoindent off → empty string
/// - autoindent on  → copy prev line's leading whitespace
/// - smartindent on → bump one `shiftwidth` if prev line's last
///   non-whitespace char is `{` / `(` / `[`
///
/// Indent unit (used for the smartindent bump):
///
/// - `expandtab && softtabstop > 0` → `softtabstop` spaces
/// - `expandtab` → `shiftwidth` spaces
/// - `!expandtab` → one literal `\t`
///
/// This is the placeholder for a future tree-sitter indent provider:
/// when a language has an `indents.scm` query, the engine will route
/// the same call through that provider and only fall back to this
/// heuristic when no query matches.
pub(super) fn compute_enter_indent(settings: &crate::editor::Settings, prev_line: &str) -> String {
    if !settings.autoindent {
        return String::new();
    }
    // Copy the prev line's leading whitespace (autoindent base).
    let base: String = prev_line
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();

    if settings.smartindent {
        let unit = if settings.expandtab {
            if settings.softtabstop > 0 {
                " ".repeat(settings.softtabstop)
            } else {
                " ".repeat(settings.shiftwidth)
            }
        } else {
            "\t".to_string()
        };

        // Open-bracket bump — language-agnostic.
        let last_non_ws = prev_line.chars().rev().find(|c| !c.is_whitespace());
        if matches!(last_non_ws, Some('{' | '(' | '[')) {
            return format!("{base}{unit}");
        }

        // HTML-family opening-tag bump: `<head>` / `<div class="...">`.
        // Gated on filetype so Rust generics like `Vec<T>` don't trigger.
        // Reuses scan_tag_opener which already filters self-closing and
        // void elements.
        if is_html_filetype(&settings.filetype) {
            let trimmed_end_len = prev_line
                .trim_end_matches(|c: char| c.is_whitespace())
                .len();
            let trimmed = &prev_line[..trimmed_end_len];
            if let Some(stripped) = trimmed.strip_suffix('>')
                && scan_tag_opener(trimmed, stripped.len()).is_some()
            {
                return format!("{base}{unit}");
            }
        }
    }

    base
}

// ── Comment-continuation helpers ──────────────────────────────────────────

/// Return the ordered (longest-first) list of line-comment prefixes for
/// `lang`. Each prefix includes one trailing space (e.g. `"// "`).
/// The same table lives in `hjkl-lang::comment` for the `gc` toggle (#187).
fn comment_prefixes_for_lang(lang: &str) -> &'static [&'static str] {
    match lang {
        "rust" => &["/// ", "//! ", "// "],
        "c" | "cpp" => &["// "],
        "python" | "sh" | "bash" | "zsh" | "fish" | "toml" | "yaml" => &["# "],
        "lua" => &["-- "],
        "sql" => &["-- "],
        "vim" | "viml" => &["\" "],
        _ => &[],
    }
}

/// Detect whether `line` starts with a known comment prefix for `lang`.
///
/// Returns `Some((indent, prefix))` where `indent` is the leading whitespace
/// of the line and `prefix` is the canonical (with trailing space) comment
/// marker. Returns `None` when the line is not a recognised comment.
pub(crate) fn detect_comment_on_line(lang: &str, line: &str) -> Option<(String, &'static str)> {
    let indent_end = line
        .char_indices()
        .find(|(_, c)| *c != ' ' && *c != '\t')
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    let indent = line[..indent_end].to_string();
    let rest = &line[indent_end..];
    for &prefix in comment_prefixes_for_lang(lang) {
        if rest.starts_with(prefix) {
            return Some((indent, prefix));
        }
        // Also match the bare prefix (line that is exactly `//` with no
        // trailing content).
        let bare = prefix.trim_end_matches(' ');
        if rest == bare || rest.starts_with(&format!("{bare} ")) {
            return Some((indent, prefix));
        }
    }
    None
}

/// Given the current `row` in `buffer` and the active `settings`, return the
/// string to prepend on the new line when comment-continuation fires.
///
/// Returns `Some("<indent><prefix>")` when the row is a comment line and
/// continuation is appropriate, `None` otherwise. The caller appends the
/// string after the `\n` they are about to insert.
pub(crate) fn continue_comment(
    buffer: &hjkl_buffer::Buffer,
    settings: &crate::editor::Settings,
    row: usize,
) -> Option<String> {
    if settings.filetype.is_empty() {
        return None;
    }
    let line = crate::buf_helpers::buf_line(buffer, row)?;
    let (indent, prefix) = detect_comment_on_line(&settings.filetype, &line)?;
    Some(format!("{indent}{prefix}"))
}

/// Strip one indent unit from the beginning of `line` and insert `ch`
/// instead. Returns `true` when it consumed the keystroke (dedent +
/// insert), `false` when the caller should insert normally.
///
/// Dedent fires when:
///   - `smartindent` is on
///   - `ch` is `}` / `)` / `]`
///   - all bytes BEFORE the cursor on the current line are whitespace
///   - there is at least one full indent unit of leading whitespace
fn try_dedent_close_bracket<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    cursor: hjkl_buffer::Position,
    ch: char,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};

    if !ed.settings.smartindent {
        return false;
    }
    if !matches!(ch, '}' | ')' | ']') {
        return false;
    }

    let line = match buf_line(&ed.buffer, cursor.row) {
        Some(l) => l.to_string(),
        None => return false,
    };

    // All chars before cursor must be whitespace.
    let before: String = line.chars().take(cursor.col).collect();
    if !before.chars().all(|c| c == ' ' || c == '\t') {
        return false;
    }
    if before.is_empty() {
        // Nothing to strip — just insert normally (cursor at col 0).
        return false;
    }

    // Compute indent unit.
    let unit_len: usize = if ed.settings.expandtab {
        if ed.settings.softtabstop > 0 {
            ed.settings.softtabstop
        } else {
            ed.settings.shiftwidth
        }
    } else {
        // Tab: one literal tab character.
        1
    };

    // Check there's at least one full unit to strip.
    let strip_len = if ed.settings.expandtab {
        // Count leading spaces; need at least `unit_len`.
        let spaces = before.chars().filter(|c| *c == ' ').count();
        if spaces < unit_len {
            return false;
        }
        unit_len
    } else {
        // noexpandtab: strip one leading tab.
        if !before.starts_with('\t') {
            return false;
        }
        1
    };

    // Delete the leading `strip_len` chars of the current line.
    ed.mutate_edit(Edit::DeleteRange {
        start: Position::new(cursor.row, 0),
        end: Position::new(cursor.row, strip_len),
        kind: MotionKind::Char,
    });
    // Insert the close bracket at column 0 (after the delete the cursor
    // is still positioned at the end of the remaining whitespace; the
    // delete moved the text so the cursor is now at col = before.len() -
    // strip_len).
    let new_col = cursor.col.saturating_sub(strip_len);
    ed.mutate_edit(Edit::InsertChar {
        at: Position::new(cursor.row, new_col),
        ch,
    });
    true
}

fn finish_insert_session<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    let Some(session) = ed.vim.insert_session.take() else {
        return;
    };
    let after_rope = crate::types::Query::rope(&ed.buffer);
    // Clamp both slices to their respective bounds — the buffer may have
    // grown (Enter splits rows) or shrunk (Backspace joins rows) during
    // the session, so row_max can overshoot either side.
    let before_n = session.before_rope.len_lines();
    let after_n = after_rope.len_lines();
    let after_end = session.row_max.min(after_n.saturating_sub(1));
    let before_end = session.row_max.min(before_n.saturating_sub(1));
    let before = if before_end >= session.row_min && session.row_min < before_n {
        rope_row_range_str(&session.before_rope, session.row_min, before_end)
    } else {
        String::new()
    };
    let after = if after_end >= session.row_min && session.row_min < after_n {
        rope_row_range_str(&after_rope, session.row_min, after_end)
    } else {
        String::new()
    };
    // `R` overstrike keeps the line length the same, so `extract_inserted`
    // (which only reports net growth) misses the typed text. Use the changed
    // run instead so dot-repeat retypes it.
    let inserted = if matches!(session.reason, InsertReason::Replace) {
        changed_run(&before, &after)
    } else {
        extract_inserted(&before, &after)
    };
    // vim `".` register — text of the most recent insert.
    if !ed.vim.replaying && !inserted.is_empty() {
        ed.vim.last_insert_text = Some(inserted.clone());
    }
    let open_line = matches!(session.reason, InsertReason::Open { .. });
    if session.count > 1 && !ed.vim.replaying {
        use hjkl_buffer::{Edit, Position};
        if open_line {
            // `[count]o` / `[count]O` open `count` SEPARATE lines, each with the
            // typed text. Read the just-opened line's content directly (the
            // row-range extract above is unreliable across the open boundary)
            // and stack `count - 1` further lines below it.
            let (start_row, _) = ed.cursor();
            let typed = buf_line(&ed.buffer, start_row).unwrap_or_default();
            for at_row in start_row..start_row + (session.count - 1) {
                let end = buf_line_chars(&ed.buffer, at_row);
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(at_row, end),
                    text: format!("\n{typed}"),
                });
            }
        } else if !inserted.is_empty() {
            // `[count]i` / `[count]A` repeat the typed text inline.
            for _ in 0..session.count - 1 {
                let (row, col) = ed.cursor();
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(row, col),
                    text: inserted.clone(),
                });
            }
        }
    }
    // Helper: replicate `inserted` text across block rows top+1..=bot at `col`,
    // padding short rows to reach `col` first. Returns without touching the
    // cursor — callers position the cursor afterward according to their needs.
    fn replicate_block_text<H: crate::types::Host>(
        ed: &mut Editor<hjkl_buffer::Buffer, H>,
        inserted: &str,
        top: usize,
        bot: usize,
        col: usize,
    ) {
        use hjkl_buffer::{Edit, Position};
        for r in (top + 1)..=bot {
            let line_len = buf_line_chars(&ed.buffer, r);
            if col > line_len {
                let pad: String = std::iter::repeat_n(' ', col - line_len).collect();
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(r, line_len),
                    text: pad,
                });
            }
            ed.mutate_edit(Edit::InsertStr {
                at: Position::new(r, col),
                text: inserted.to_string(),
            });
        }
    }

    if let InsertReason::BlockEdge { top, bot, col } = session.reason {
        // `I` / `A` from VisualBlock: replicate text across rows; cursor
        // stays at the block-start column (vim leaves cursor there).
        if !inserted.is_empty() && top < bot && !ed.vim.replaying {
            replicate_block_text(ed, &inserted, top, bot, col);
            buf_set_cursor_rc(&mut ed.buffer, top, col);
            ed.push_buffer_cursor_to_textarea();
        }
        return;
    }
    if let InsertReason::BlockChange { top, bot, col } = session.reason {
        // `c` from VisualBlock: replicate text across rows; cursor advances
        // to `col + ins_chars` (pre-step-back) so the Esc step-back lands
        // on the last typed char (col + ins_chars - 1), matching nvim.
        if !inserted.is_empty() && top < bot && !ed.vim.replaying {
            replicate_block_text(ed, &inserted, top, bot, col);
            let ins_chars = inserted.chars().count();
            let line_len = buf_line_chars(&ed.buffer, top);
            let target_col = (col + ins_chars).min(line_len);
            buf_set_cursor_rc(&mut ed.buffer, top, target_col);
            ed.push_buffer_cursor_to_textarea();
        }
        return;
    }
    if ed.vim.replaying {
        return;
    }
    match session.reason {
        InsertReason::Enter(entry) => {
            ed.vim.last_change = Some(LastChange::InsertAt {
                entry,
                inserted,
                count: session.count,
            });
        }
        InsertReason::Open { above } => {
            ed.vim.last_change = Some(LastChange::OpenLine { above, inserted });
        }
        InsertReason::AfterChange => {
            if let Some(
                LastChange::OpMotion { inserted: ins, .. }
                | LastChange::OpTextObj { inserted: ins, .. }
                | LastChange::LineOp { inserted: ins, .. }
                | LastChange::GnOp { inserted: ins, .. },
            ) = ed.vim.last_change.as_mut()
            {
                *ins = Some(inserted);
            }
            // Vim `:h '[` / `:h ']`: on change, `[` = start of the
            // changed range (stashed before the cut), `]` = the cursor
            // at Esc time (last inserted char, before the step-back).
            // When nothing was typed cursor still sits at the change
            // start, satisfying vim's "both at start" parity for `c<m><Esc>`.
            if let Some(start) = ed.vim.change_mark_start.take() {
                let end = ed.cursor();
                ed.set_mark('[', start);
                ed.set_mark(']', end);
            }
        }
        InsertReason::DeleteToEol => {
            ed.vim.last_change = Some(LastChange::DeleteToEol {
                inserted: Some(inserted),
            });
        }
        InsertReason::ReplayOnly => {}
        InsertReason::BlockEdge { .. } => unreachable!("handled above"),
        InsertReason::BlockChange { .. } => unreachable!("handled above"),
        InsertReason::Replace => {
            // `R` overstrike: dot-repeat re-overtypes the same text at the
            // cursor (vim parity — not a delete-to-EOL).
            ed.vim.last_change = Some(LastChange::ReplaceMode { text: inserted });
        }
    }
}

pub(crate) fn begin_insert<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
    reason: InsertReason,
) {
    // `nomodifiable`: silently refuse to enter insert/replace; stay in current mode.
    if !ed.settings.modifiable {
        return;
    }
    // BLAME view: pressing `i` exits blame (drops the overlay) but stays Normal.
    if ed.vim.view == crate::ViewMode::Blame {
        ed.vim.view = crate::ViewMode::Normal;
        return;
    }
    let record = !matches!(reason, InsertReason::ReplayOnly);
    if record {
        ed.push_undo();
    }
    let reason = if ed.vim.replaying {
        InsertReason::ReplayOnly
    } else {
        reason
    };
    let (row, col) = ed.cursor();
    ed.vim.insert_session = Some(InsertSession {
        count,
        row_min: row,
        row_max: row,
        before_rope: crate::types::Query::rope(&ed.buffer),
        reason,
        start_row: row,
        start_col: col,
    });
    ed.vim.mode = Mode::Insert;
    // Phase 6.3: keep current_mode in sync for callers that bypass step().
    ed.vim.current_mode = crate::VimMode::Insert;
    drop_blame_if_left_normal(ed);
}

/// `:set undobreak` semantics for insert-mode motions. When the
/// toggle is on, a non-character keystroke that moves the cursor
/// (arrow keys, Home/End, mouse click) ends the current undo group
/// and starts a new one mid-session. After this, a subsequent `u`
/// in normal mode reverts only the post-break run, leaving the
/// pre-break edits in place — matching vim's behaviour.
///
/// Implementation: snapshot the current buffer onto the undo stack
/// (the new break point) and reset the active `InsertSession`'s
/// `before_lines` so `finish_insert_session`'s diff window only
/// captures the post-break run for `last_change` / dot-repeat.
///
/// During replay we skip the break — replay shouldn't pollute the
/// undo stack with intra-replay snapshots.
pub(crate) fn break_undo_group_in_insert<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    if !ed.settings.undo_break_on_motion {
        return;
    }
    if ed.vim.replaying {
        return;
    }
    if ed.vim.insert_session.is_none() {
        return;
    }
    ed.push_undo();
    let before_rope = crate::types::Query::rope(&ed.buffer);
    let row = crate::types::Cursor::cursor(&ed.buffer).line as usize;
    if let Some(ref mut session) = ed.vim.insert_session {
        session.before_rope = before_rope;
        session.row_min = row;
        session.row_max = row;
    }
}

// ─── Phase 6.1: public insert-mode primitives ──────────────────────────────
//
// Each `pub(crate)` free function below implements one insert-mode action.
// hjkl-vim's insert dispatcher calls them through `Editor::insert_*` methods.
// External callers can also invoke the public Editor methods directly.
//
// Invariants every function upholds:
//   - Opens with `ed.sync_buffer_content_from_textarea()` (no-op, kept for
//     forward compatibility once textarea is gone).
//   - All buffer mutations go through `ed.mutate_edit(...)` so dirty flag,
//     undo, change-list, content-edit fan-out all fire uniformly.
//   - Navigation-only functions call `break_undo_group_in_insert` when the
//     FSM did so, then return `false` (no mutation).
//   - After mutations, `ed.push_buffer_cursor_to_textarea()` is called
//     (currently a no-op but kept for migration hygiene).
//   - Returns `true` when the buffer was mutated, `false` otherwise.

/// Return the filetype-gated autopair close character for `open`, or `None`
/// when no pairing applies.
///
/// Rules:
/// - `(` → `)`, `[` → `]`, `{` → `}` always.
/// - `"` → `"` and `` ` `` → `` ` `` always, EXCEPT when the previous two
///   characters are the same quote — typing the third `` ` `` of a markdown
///   code-fence or the third `"` of a Python triple-quoted string must
///   emit a bare quote (no close) so the result is `` ``` `` / `"""` and
///   not `` ```` `` / `""""`.
/// - `<` → `>` only for HTML/XML family filetypes.
/// - `'` → `'` unless the character immediately before the cursor is
///   `[A-Za-z]` (prose apostrophe guard — "don't" stays "don't"), AND the
///   same triple-quote guard as `"` / `` ` ``.
fn autopair_close_for(
    ch: char,
    filetype: &str,
    prev_char: Option<char>,
    prev2_char: Option<char>,
) -> Option<char> {
    // Triple-quote guard — applies to ", `, and ' (the three quote chars
    // that get same-char pairing). When the previous two characters are
    // both this same quote, treat the third keystroke as a bare insert so
    // the user lands on `` ``` `` / `"""` / `'''` without a stray fourth
    // quote dangling after the cursor.
    let is_triple_quote_third =
        matches!(ch, '"' | '`' | '\'') && prev_char == Some(ch) && prev2_char == Some(ch);

    match ch {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => {
            if is_triple_quote_third {
                None
            } else {
                Some('"')
            }
        }
        '`' => {
            if is_triple_quote_third {
                None
            } else {
                Some('`')
            }
        }
        '<' => {
            if is_html_filetype(filetype) {
                Some('>')
            } else {
                None
            }
        }
        '\'' => {
            if is_triple_quote_third {
                return None;
            }
            // Prose guard: skip pairing when the previous char is a letter
            // (covers "don't", "it's", etc.).
            if prev_char.map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
                None
            } else {
                Some('\'')
            }
        }
        _ => None,
    }
}

/// Detect a markdown / doc-comment code-fence opener on the current line.
///
/// Returns `Some(fence)` (the backtick run that should be used as the
/// closing fence) when:
/// - The cursor is at the end of the visible line (`cursor_col` equals the
///   line's char count).
/// - The line, after leading whitespace, begins with 3+ backticks followed
///   by a non-empty language tag matching `[A-Za-z0-9_+-]+` and nothing
///   else (no trailing space, no extra text).
///
/// The language tag requirement is deliberate: a bare ` ``` ` could be
/// either an opener OR a closer, and we don't track fence parity here.
/// Requiring a tag means we only fire when the user is clearly opening a
/// fence (` ```rust `, ` ```ts `, etc.).
fn detect_code_fence_opener(line: &str, cursor_col: usize) -> Option<String> {
    if cursor_col != line.chars().count() {
        return None;
    }
    let trimmed = line.trim_start();
    let backtick_run = trimmed.chars().take_while(|c| *c == '`').count();
    if backtick_run < 3 {
        return None;
    }
    let rest = &trimmed[backtick_run..];
    if rest.is_empty() {
        return None;
    }
    let all_lang_chars = rest
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '+' || c == '-');
    if !all_lang_chars {
        return None;
    }
    Some("`".repeat(backtick_run))
}

/// Filetypes that get HTML/XML-family treatment (`<` pairing + tag autoclose).
fn is_html_filetype(ft: &str) -> bool {
    matches!(
        ft,
        "html" | "xml" | "svg" | "jsx" | "tsx" | "vue" | "svelte"
    )
}

// ── Paired-tag auto-rename (issue #182) ────────────────────────────────────
//
// When the user edits the name of an HTML/XML opening tag (e.g. `ci<` to
// change-inner the tag name, type a new name, then `<Esc>`), the matching
// closing tag should rename automatically so the pair stays in sync.
// Same on the close side: edit `</X>` → its opener gets renamed.
//
// Trigger: leave_insert_to_normal_bridge calls sync_paired_tag_on_exit, which
// inspects the cursor's current position. If the cursor sits inside a tag
// name and the paired tag has a different name, rewrite the paired tag.
//
// Pairing uses a stack-based scan so nested same-name tags
// (`<div><div></div></div>`) pair correctly.

/// Tag kind detected at a cursor position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagKind {
    Open,
    Close,
}

/// A single tag instance located in the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TagSpan {
    kind: TagKind,
    name: String,
    /// Row index in the buffer.
    row: usize,
    /// Char-column range of the tag NAME (excluding `<`, `</`, attributes, `>`).
    name_start_col: usize,
    name_end_col: usize,
}

/// Detect the tag containing `(row, col)` in `line`. Returns the tag kind
/// (Open / Close), its name, and the char-column range of that name.
/// Returns `None` when the cursor is not inside a tag-name region.
fn detect_tag_at_cursor(line: &str, row: usize, col: usize) -> Option<TagSpan> {
    let chars: Vec<char> = line.chars().collect();
    // Find the nearest `<` at or before the cursor column.
    let mut lt = None;
    let mut i = col.min(chars.len());
    while i > 0 {
        i -= 1;
        let c = chars[i];
        if c == '<' {
            lt = Some(i);
            break;
        }
        // Bail if we cross a `>` (we're outside any open tag).
        if c == '>' {
            return None;
        }
    }
    let lt = lt?;
    // Detect close tag (`</`) vs open (`<`).
    let (kind, name_start) = if chars.get(lt + 1) == Some(&'/') {
        (TagKind::Close, lt + 2)
    } else {
        (TagKind::Open, lt + 1)
    };
    // First char of the name must be a letter.
    let first = chars.get(name_start)?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    // Tag name = [A-Za-z][A-Za-z0-9-]*
    let mut name_end = name_start;
    while name_end < chars.len()
        && (chars[name_end].is_ascii_alphanumeric() || chars[name_end] == '-')
    {
        name_end += 1;
    }
    // Cursor must be inside the name range (inclusive of both ends so that
    // landing right after the name still resolves — vim Insert leaves the
    // cursor one past the last typed char).
    if col < name_start || col > name_end {
        return None;
    }
    let name: String = chars[name_start..name_end].iter().collect();
    Some(TagSpan {
        kind,
        name,
        row,
        name_start_col: name_start,
        name_end_col: name_end,
    })
}

/// Scan the buffer to find the structural partner of `anchor` using a
/// depth counter. Names are intentionally NOT compared during the scan —
/// the anchor is the source of truth and the partner inherits its name.
/// Otherwise an in-flight rename (the whole point of this feature) would
/// look like a malformed pair and bail.
///
/// Forward scan from an opener: opens increment depth, closes decrement
/// depth. The close that brings depth back to zero is the partner.
/// Backward scan from a closer is symmetric (closes increment, opens
/// decrement).
///
/// Returns `None` when the buffer end is reached before depth hits zero
/// (orphan tag or malformed input).
fn find_matching_tag(buffer: &hjkl_buffer::Buffer, anchor: &TagSpan) -> Option<TagSpan> {
    let row_count = buffer.row_count();
    let scan_forward = anchor.kind == TagKind::Open;
    let row_iter: Box<dyn Iterator<Item = usize>> = if scan_forward {
        Box::new(anchor.row..row_count)
    } else {
        Box::new((0..=anchor.row).rev())
    };
    let push_kind = if scan_forward {
        TagKind::Open
    } else {
        TagKind::Close
    };
    let mut depth: usize = 1;

    for r in row_iter {
        let line = buf_line(buffer, r)?;
        let chars: Vec<char> = line.chars().collect();
        let tags = scan_line_tags(&chars, r);
        let tags_iter: Box<dyn Iterator<Item = TagSpan>> = if scan_forward {
            Box::new(tags.into_iter())
        } else {
            Box::new(tags.into_iter().rev())
        };
        for tag in tags_iter {
            // Skip the anchor itself when we walk over its line.
            if r == anchor.row
                && tag.name_start_col == anchor.name_start_col
                && tag.kind == anchor.kind
            {
                continue;
            }
            // On the anchor's own row, gate by direction relative to anchor
            // so the scan only inspects tags AFTER the anchor (forward) or
            // BEFORE the anchor (backward).
            if r == anchor.row {
                if scan_forward && tag.name_start_col < anchor.name_start_col {
                    continue;
                }
                if !scan_forward && tag.name_start_col > anchor.name_start_col {
                    continue;
                }
            }
            if tag.kind == push_kind {
                depth += 1;
            } else {
                depth -= 1;
                if depth == 0 {
                    return Some(tag);
                }
            }
        }
    }
    None
}

/// Collect all tag opens / closes on a single line in left-to-right order.
/// Skips comments (`<!-- ... -->`) and self-closing tags (`<br />`), and
/// excludes void HTML elements that don't form a pair.
fn scan_line_tags(chars: &[char], row: usize) -> Vec<TagSpan> {
    let mut out = Vec::new();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        if chars[i] != '<' {
            i += 1;
            continue;
        }
        // `<!--` comment — skip to `-->`.
        if chars[i..].starts_with(&['<', '!', '-', '-']) {
            let mut j = i + 4;
            while j + 2 < n && !(chars[j] == '-' && chars[j + 1] == '-' && chars[j + 2] == '>') {
                j += 1;
            }
            i = (j + 3).min(n);
            continue;
        }
        let (kind, name_start) = if chars.get(i + 1) == Some(&'/') {
            (TagKind::Close, i + 2)
        } else {
            (TagKind::Open, i + 1)
        };
        // Validate name start.
        if chars
            .get(name_start)
            .is_none_or(|c| !c.is_ascii_alphabetic())
        {
            i += 1;
            continue;
        }
        let mut name_end = name_start;
        while name_end < n && (chars[name_end].is_ascii_alphanumeric() || chars[name_end] == '-') {
            name_end += 1;
        }
        // Find the closing `>` to know whether this tag is self-closing.
        let mut k = name_end;
        let mut self_closing = false;
        while k < n {
            if chars[k] == '>' {
                if k > name_end && chars[k - 1] == '/' {
                    self_closing = true;
                }
                break;
            }
            k += 1;
        }
        if k >= n {
            // Unterminated tag on this line — bail.
            break;
        }
        let name: String = chars[name_start..name_end].iter().collect();
        // Skip self-closing and void elements (no pair).
        if !(self_closing || kind == TagKind::Open && is_void_element(&name)) {
            out.push(TagSpan {
                kind,
                name,
                row,
                name_start_col: name_start,
                name_end_col: name_end,
            });
        }
        i = k + 1;
    }
    out
}

/// If the cursor sits inside an HTML/XML tag name AND the paired tag's name
/// differs, rewrite the paired tag's name to match. Called from
/// `leave_insert_to_normal_bridge` so the magical sync fires exactly when
/// the user finishes editing.
pub(crate) fn sync_paired_tag_on_exit<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    if !is_html_filetype(&ed.settings.filetype) {
        return;
    }
    let (row, col) = ed.cursor();
    let line = match buf_line(&ed.buffer, row) {
        Some(l) => l,
        None => return,
    };
    let anchor = match detect_tag_at_cursor(&line, row, col) {
        Some(t) => t,
        None => return,
    };
    let partner = match find_matching_tag(&ed.buffer, &anchor) {
        Some(t) => t,
        None => return,
    };
    if partner.name == anchor.name {
        return;
    }
    // Rewrite the partner's name range with the anchor's name.
    use hjkl_buffer::{Edit, MotionKind, Position};
    let start = Position::new(partner.row, partner.name_start_col);
    let end = Position::new(partner.row, partner.name_end_col);
    ed.mutate_edit(Edit::DeleteRange {
        start,
        end,
        kind: MotionKind::Char,
    });
    ed.mutate_edit(Edit::InsertStr {
        at: start,
        text: anchor.name.clone(),
    });
    // Restore the user's cursor — mutate_edit may have moved it during the
    // partner-side rewrite when the partner is on a row before the cursor.
    buf_set_cursor_rc(&mut ed.buffer, row, col);
    ed.push_buffer_cursor_to_textarea();
}

/// Resolve the HTML/XML tag-name pair under the cursor for matchparen-style
/// highlight (#243). Returns `[(row, name_start_col, name_end_col); 2]` for
/// the tag under the cursor and its structural partner, or `None` when the
/// cursor is not on a tag name or the tag is unpaired. Char-column ranges
/// (display), consistent with `motions::matching_bracket_pos`.
pub fn matching_tag_pair(
    buffer: &hjkl_buffer::Buffer,
    row: usize,
    col: usize,
) -> Option<[(usize, usize, usize); 2]> {
    let line = buf_line(buffer, row)?;
    let anchor = detect_tag_at_cursor(&line, row, col)?;
    let partner = find_matching_tag(buffer, &anchor)?;
    Some([
        (anchor.row, anchor.name_start_col, anchor.name_end_col),
        (partner.row, partner.name_start_col, partner.name_end_col),
    ])
}

/// Void HTML elements that must never get an auto-close tag.
fn is_void_element(tag: &str) -> bool {
    matches!(
        tag.to_ascii_lowercase().as_str(),
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Scan backward from `col` (exclusive) in `line` for a `<tagname…` opener.
///
/// Returns `Some(tag_name)` when:
/// - An opening `<` is found
/// - The tag name matches `[A-Za-z][A-Za-z0-9-]*`
/// - The tag is not self-closing (does not end with `/` before `>`)
/// - The tag is not a void element
///
/// Returns `None` otherwise (no opener, self-closing, void, or malformed).
fn scan_tag_opener(line: &str, col: usize) -> Option<String> {
    // col is where `>` was just inserted (the char is already in the line).
    // We look at the slice BEFORE the `>`.
    let before = if col > 0 { &line[..col] } else { return None };

    // Walk backward to find the matching `<`.
    let lt_pos = before.rfind('<')?;
    let inner = &before[lt_pos + 1..]; // e.g. "div class=\"foo\""

    // A `!` opener is a comment/doctype — skip.
    if inner.starts_with('!') {
        return None;
    }
    // Self-closing if the last non-space char before `>` was `/`.
    if inner.trim_end().ends_with('/') {
        return None;
    }

    // Extract tag name: first token of `inner`.
    let tag: String = inner
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if tag.is_empty() {
        return None;
    }
    // First char must be a letter.
    if !tag
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic())
        .unwrap_or(false)
    {
        return None;
    }
    if is_void_element(&tag) {
        return None;
    }
    Some(tag)
}

/// Insert a single character at the cursor. Handles replace-mode overstrike
/// (when `InsertSession::reason` is `Replace`) and smart-indent dedent of
/// closing brackets (}/)]/). Also handles autopair insertion and skip-over.
/// Returns `true`.
pub(crate) fn insert_char_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    ch: char,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let in_replace = matches!(
        ed.vim.insert_session.as_ref().map(|s| &s.reason),
        Some(InsertReason::Replace)
    );

    // ── Abbreviation expansion (insert mode, non-replace) ────────────────────
    // A non-keyword char typed in insert mode can trigger expansion.
    // We check BEFORE inserting the character; if an abbrev matches, we delete
    // the lhs and insert the rhs, then continue to insert `ch` as normal.
    // `<C-v>` (literal-insert) must bypass this — callers that want literal
    // insertion should NOT call this bridge; they use insert_char_literal.
    if !in_replace && !ed.vim.abbrevs.is_empty() {
        let iskeyword = ed.settings.iskeyword.clone();
        if !is_keyword_char(ch, &iskeyword) {
            // Only non-keyword trigger chars fire abbreviation expansion.
            check_and_apply_abbrev(ed, AbbrevTrigger::NonKeyword(ch));
            // (we do NOT return early; continue to insert `ch` below)
        }
    }
    // Read cursor (after any abbreviation expansion that may have changed the buffer).
    let cursor = buf_cursor_pos(&ed.buffer);
    let line_chars = buf_line_chars(&ed.buffer, cursor.row);

    // ── Skip-over: if the typed char matches the top of the pending-closes
    // stack AND the char currently under the cursor IS that close char,
    // pop the stack and advance the cursor instead of inserting.
    //
    // We check the actual char in the buffer (not a stored col) so that
    // characters typed between the pair don't invalidate the skip — the
    // close char shifts right as the user types inside, but the buffer
    // char check always finds it correctly.
    if !in_replace
        && !ed.vim.pending_closes.is_empty()
        && let Some(&(pr, _pc, pch)) = ed.vim.pending_closes.last()
        && ch == pch
        && cursor.row == pr
    {
        let char_at_cursor =
            buf_line(&ed.buffer, cursor.row).and_then(|l| l.chars().nth(cursor.col));
        if char_at_cursor == Some(ch) {
            ed.vim.pending_closes.pop();
            // For `>` skip-over in HTML/XML: also run tag autoclose.
            let filetype = ed.settings.filetype.clone();
            let autoclose_tag = ed.settings.autoclose_tag;
            if ch == '>' && autoclose_tag && is_html_filetype(&filetype) {
                // Skip past the `>` that was auto-inserted.
                let new_col = cursor.col + 1;
                buf_set_cursor_rc(&mut ed.buffer, cursor.row, new_col);
                // Now check for tag autoclose on the line up to new_col.
                if let Some(line) = buf_line(&ed.buffer, cursor.row)
                    && let Some(tag) = scan_tag_opener(&line, new_col.saturating_sub(1))
                {
                    let close_tag = format!("</{tag}>");
                    let insert_pos = Position::new(cursor.row, new_col);
                    ed.mutate_edit(Edit::InsertStr {
                        at: insert_pos,
                        text: close_tag,
                    });
                    // Cursor stays at new_col (between > and </tag>).
                    buf_set_cursor_rc(&mut ed.buffer, cursor.row, new_col);
                }
            } else {
                buf_set_cursor_rc(&mut ed.buffer, cursor.row, cursor.col + 1);
            }
            ed.push_buffer_cursor_to_textarea();
            return true;
        }
    }

    if in_replace && cursor.col < line_chars {
        // Replace mode: clear pending closes (edit outside the pair).
        ed.vim.pending_closes.clear();
        ed.mutate_edit(Edit::DeleteRange {
            start: cursor,
            end: Position::new(cursor.row, cursor.col + 1),
            kind: MotionKind::Char,
        });
        ed.mutate_edit(Edit::InsertChar { at: cursor, ch });
    } else if !try_dedent_close_bracket(ed, cursor, ch) {
        // Normal insert. Check autopair first.
        let autopair = ed.settings.autopair;
        let filetype = ed.settings.filetype.clone();
        let autoclose_tag = ed.settings.autoclose_tag;

        let (prev_char, prev2_char) = {
            let line = buf_line(&ed.buffer, cursor.row).unwrap_or_default();
            let chars: Vec<char> = line.chars().collect();
            let p1 = if cursor.col > 0 {
                chars.get(cursor.col - 1).copied()
            } else {
                None
            };
            let p2 = if cursor.col > 1 {
                chars.get(cursor.col - 2).copied()
            } else {
                None
            };
            (p1, p2)
        };

        if autopair {
            if let Some(close) = autopair_close_for(ch, &filetype, prev_char, prev2_char) {
                // Insert open char.
                ed.mutate_edit(Edit::InsertChar { at: cursor, ch });
                // Insert close char immediately after the open char.
                // After inserting open at cursor, buffer cursor is at cursor.col+1.
                let after = Position::new(cursor.row, cursor.col + 1);
                ed.mutate_edit(Edit::InsertChar {
                    at: after,
                    ch: close,
                });
                // After inserting close, buffer cursor is at cursor.col+2.
                // We want cursor between open and close: cursor.col+1.
                let between_col = cursor.col + 1;
                buf_set_cursor_rc(&mut ed.buffer, cursor.row, between_col);
                // Record the close char for skip-over. We store the row and
                // the close char; col is not tracked precisely because chars
                // typed inside the pair shift the close right. The skip-over
                // logic checks the actual buffer char at cursor instead.
                ed.vim.pending_closes.push((cursor.row, between_col, close));
                ed.push_buffer_cursor_to_textarea();
                return true;
            }

            // Tag autoclose: `>` in HTML/XML family (no prior `<` pair).
            // This fires when autopair did NOT match `>` (e.g. `>` was
            // typed directly, not via a skip-over of an auto-inserted `>`).
            if ch == '>' && autoclose_tag && is_html_filetype(&filetype) {
                ed.mutate_edit(Edit::InsertChar { at: cursor, ch });
                let new_col = cursor.col + 1;
                // scan_tag_opener looks at the line up to (new_col-1), i.e.
                // the char just inserted is at index new_col-1.
                if let Some(line) = buf_line(&ed.buffer, cursor.row)
                    && let Some(tag) = scan_tag_opener(&line, new_col.saturating_sub(1))
                {
                    let close_tag = format!("</{tag}>");
                    let insert_pos = Position::new(cursor.row, new_col);
                    ed.mutate_edit(Edit::InsertStr {
                        at: insert_pos,
                        text: close_tag,
                    });
                    // Cursor stays at new_col (between `>` and `</tag>`).
                    buf_set_cursor_rc(&mut ed.buffer, cursor.row, new_col);
                }
                ed.push_buffer_cursor_to_textarea();
                return true;
            }
        }

        // Plain insert — do not clear the pending-closes stack here.
        // The stack is cleared on cursor motion or mode change (Esc).
        // Clearing here would prevent skip-over from firing after the
        // user types content inside an auto-paired bracket.
        ed.mutate_edit(Edit::InsertChar { at: cursor, ch });
    }
    ed.push_buffer_cursor_to_textarea();
    true
}

/// Insert a newline at the cursor, applying autoindent / smartindent and
/// optionally continuing a line comment when `formatoptions` has `r`.
/// Also handles open-pair-newline: Enter between `{|}` / `(|)` / `[|]`
/// produces an indented block with the close on its own line.
/// Returns `true`.
pub(crate) fn insert_newline_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    use hjkl_buffer::Edit;
    ed.sync_buffer_content_from_textarea();

    // ── Abbreviation expansion on CR ─────────────────────────────────────────
    // CR triggers expansion for full-id / end-id / non-id abbreviations.
    // We expand BEFORE the newline is inserted; CR is then inserted as normal.
    if !ed.vim.abbrevs.is_empty() {
        check_and_apply_abbrev(ed, AbbrevTrigger::Cr);
    }

    let cursor = buf_cursor_pos(&ed.buffer);
    let prev_line = buf_line(&ed.buffer, cursor.row)
        .unwrap_or_default()
        .to_string();

    // Open-pair-newline: if autopair is on and the cursor is between a
    // matching open/close bracket pair, split into two newlines so the
    // close ends up on its own dedented line.
    if ed.settings.autopair && !ed.vim.pending_closes.is_empty() {
        // Check: char before cursor is an open bracket AND char at cursor
        // is the matching close bracket (from our pending-closes stack).
        let prev_char = if cursor.col > 0 {
            prev_line.chars().nth(cursor.col - 1)
        } else {
            None
        };
        let next_char = prev_line.chars().nth(cursor.col);
        let is_open_pair = matches!(
            (prev_char, next_char),
            (Some('{'), Some('}')) | (Some('('), Some(')')) | (Some('['), Some(']'))
        );
        if is_open_pair {
            // The pending-closes stack refers to the close char at cursor.col.
            // We clear it because the newline expansion moves the close.
            ed.vim.pending_closes.clear();
            // Compute indents: inner gets one extra unit, close gets base.
            let base_indent: String = prev_line
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            let inner_indent = if ed.settings.expandtab {
                let unit = if ed.settings.softtabstop > 0 {
                    ed.settings.softtabstop
                } else {
                    ed.settings.shiftwidth
                };
                format!("{base_indent}{}", " ".repeat(unit))
            } else {
                format!("{base_indent}\t")
            };
            // Insert: \n<inner_indent>\n<base_indent>
            // Then cursor lands after the first \n (inside the block).
            let text = format!("\n{inner_indent}\n{base_indent}");
            ed.mutate_edit(Edit::InsertStr { at: cursor, text });
            // Move cursor to end of first new line (inner_indent line).
            let new_row = cursor.row + 1;
            let new_col = inner_indent.len();
            buf_set_cursor_rc(&mut ed.buffer, new_row, new_col);
            ed.push_buffer_cursor_to_textarea();
            return true;
        }
    }

    // Code-fence expansion: line content is ` ``` ` (3+ backticks) followed
    // by a non-empty language tag, cursor sits at end of line → insert the
    // matching closing fence on the line below and park the cursor on a
    // blank middle line. Matches the open-pair-newline shape but for
    // markdown / doc-comment code blocks. Gated on a language tag because
    // a bare ` ``` ` could just as easily be a closing fence — we'd need
    // full document parity tracking to handle that safely, which v1
    // doesn't have.
    if ed.settings.autopair
        && let Some(fence) = detect_code_fence_opener(&prev_line, cursor.col)
    {
        ed.vim.pending_closes.clear();
        let base_indent: String = prev_line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let text = format!("\n{base_indent}\n{base_indent}{fence}");
        ed.mutate_edit(Edit::InsertStr { at: cursor, text });
        let new_row = cursor.row + 1;
        let new_col = base_indent.chars().count();
        buf_set_cursor_rc(&mut ed.buffer, new_row, new_col);
        ed.push_buffer_cursor_to_textarea();
        return true;
    }

    // formatoptions `r`: continue comment on Enter in insert mode.
    let comment_cont = if ed.settings.formatoptions.contains('r') {
        continue_comment(&ed.buffer, &ed.settings, cursor.row)
    } else {
        None
    };

    // Any Enter clears the pending-closes stack (cursor moved off the pair).
    ed.vim.pending_closes.clear();

    let text = if let Some(cont) = comment_cont {
        // Comment continuation overrides autoindent: the indent is already
        // baked into the continuation prefix.
        format!("\n{cont}")
    } else {
        let indent = compute_enter_indent(&ed.settings, &prev_line);
        format!("\n{indent}")
    };
    ed.mutate_edit(Edit::InsertStr { at: cursor, text });
    ed.push_buffer_cursor_to_textarea();
    true
}

/// Insert a tab character (or spaces up to the next softtabstop boundary when
/// `expandtab` is set). Returns `true`.
pub(crate) fn insert_tab_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    use hjkl_buffer::Edit;
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(&ed.buffer);
    if ed.settings.expandtab {
        let sts = ed.settings.softtabstop;
        let n = if sts > 0 {
            sts - (cursor.col % sts)
        } else {
            ed.settings.tabstop.max(1)
        };
        ed.mutate_edit(Edit::InsertStr {
            at: cursor,
            text: " ".repeat(n),
        });
    } else {
        ed.mutate_edit(Edit::InsertChar {
            at: cursor,
            ch: '\t',
        });
    }
    ed.push_buffer_cursor_to_textarea();
    true
}

/// Delete the character before the cursor (vim Backspace / `^H`). With
/// `softtabstop` active, deletes the entire soft-tab run at an aligned
/// boundary. Joins with the previous line when at column 0.
///
/// **Comment-continuation backspace**: when the current line's entire content
/// is the auto-inserted comment prefix (e.g. `// ` with nothing after it),
/// a single Backspace removes the whole prefix in one stroke — vim parity.
///
/// Returns `true` when something was deleted, `false` at the very start of the
/// buffer.
pub(crate) fn insert_backspace_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(&ed.buffer);

    // Comment-continuation backspace: if the line is just the prefix (with no
    // user content after it), delete the whole prefix in one stroke.
    if cursor.col > 0 {
        let line = buf_line(&ed.buffer, cursor.row).unwrap_or_default();
        if let Some((indent, prefix)) = detect_comment_on_line(&ed.settings.filetype, &line) {
            let full_prefix = format!("{indent}{prefix}");
            // The cursor must be at the end of (or within) the prefix with no
            // additional content after — i.e. the line equals the prefix exactly.
            let line_trimmed = line.trim_end_matches(' ');
            let prefix_trimmed = full_prefix.trim_end_matches(' ');
            if line_trimmed == prefix_trimmed && cursor.col == full_prefix.chars().count() {
                // Delete everything from col 0 to cursor.
                ed.mutate_edit(Edit::DeleteRange {
                    start: Position::new(cursor.row, 0),
                    end: cursor,
                    kind: MotionKind::Char,
                });
                ed.push_buffer_cursor_to_textarea();
                return true;
            }
        }
    }

    let sts = ed.settings.softtabstop;
    if sts > 0 && cursor.col >= sts && cursor.col.is_multiple_of(sts) {
        let line = buf_line(&ed.buffer, cursor.row).unwrap_or_default();
        let chars: Vec<char> = line.chars().collect();
        let run_start = cursor.col - sts;
        if (run_start..cursor.col).all(|i| chars.get(i).copied() == Some(' ')) {
            ed.mutate_edit(Edit::DeleteRange {
                start: Position::new(cursor.row, run_start),
                end: cursor,
                kind: MotionKind::Char,
            });
            ed.push_buffer_cursor_to_textarea();
            return true;
        }
    }
    let result = if cursor.col > 0 {
        ed.mutate_edit(Edit::DeleteRange {
            start: Position::new(cursor.row, cursor.col - 1),
            end: cursor,
            kind: MotionKind::Char,
        });
        true
    } else if cursor.row > 0 {
        let prev_row = cursor.row - 1;
        let prev_chars = buf_line_chars(&ed.buffer, prev_row);
        ed.mutate_edit(Edit::JoinLines {
            row: prev_row,
            count: 1,
            with_space: false,
        });
        buf_set_cursor_rc(&mut ed.buffer, prev_row, prev_chars);
        true
    } else {
        false
    };
    ed.push_buffer_cursor_to_textarea();
    result
}

/// Delete the character under the cursor (vim `Delete`). Joins with the
/// next line when at end-of-line. Returns `true` when something was deleted.
pub(crate) fn insert_delete_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(&ed.buffer);
    let line_chars = buf_line_chars(&ed.buffer, cursor.row);
    let result = if cursor.col < line_chars {
        ed.mutate_edit(Edit::DeleteRange {
            start: cursor,
            end: Position::new(cursor.row, cursor.col + 1),
            kind: MotionKind::Char,
        });
        buf_set_cursor_pos(&mut ed.buffer, cursor);
        true
    } else if cursor.row + 1 < buf_row_count(&ed.buffer) {
        ed.mutate_edit(Edit::JoinLines {
            row: cursor.row,
            count: 1,
            with_space: false,
        });
        buf_set_cursor_pos(&mut ed.buffer, cursor);
        true
    } else {
        false
    };
    ed.push_buffer_cursor_to_textarea();
    result
}

/// Direction for insert-mode arrow movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertDir {
    Left,
    Right,
    Up,
    Down,
}

/// Move the cursor one step in `dir`, breaking the undo group per
/// `undo_break_on_motion`. Clears the autopair pending-closes stack (cursor
/// moved off the pair). Returns `false` (no mutation).
pub(crate) fn insert_arrow_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    dir: InsertDir,
) -> bool {
    ed.sync_buffer_content_from_textarea();
    ed.vim.pending_closes.clear();
    match dir {
        InsertDir::Left => {
            crate::motions::move_left(&mut ed.buffer, 1);
        }
        InsertDir::Right => {
            crate::motions::move_right_to_end(&mut ed.buffer, 1);
        }
        InsertDir::Up => {
            let folds = crate::buffer_impl::SnapshotFoldProvider::from_buffer(&ed.buffer);
            crate::motions::move_up(&mut ed.buffer, &folds, 1, &mut ed.sticky_col);
        }
        InsertDir::Down => {
            let folds = crate::buffer_impl::SnapshotFoldProvider::from_buffer(&ed.buffer);
            crate::motions::move_down(&mut ed.buffer, &folds, 1, &mut ed.sticky_col);
        }
    }
    break_undo_group_in_insert(ed);
    ed.push_buffer_cursor_to_textarea();
    false
}

/// Move the cursor to the start of the current line, breaking the undo group.
/// Clears the autopair pending-closes stack. Returns `false` (no mutation).
pub(crate) fn insert_home_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    ed.sync_buffer_content_from_textarea();
    ed.vim.pending_closes.clear();
    crate::motions::move_line_start(&mut ed.buffer);
    break_undo_group_in_insert(ed);
    ed.push_buffer_cursor_to_textarea();
    false
}

/// Move the cursor to the end of the current line, breaking the undo group.
/// Clears the autopair pending-closes stack. Returns `false` (no mutation).
pub(crate) fn insert_end_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    ed.sync_buffer_content_from_textarea();
    ed.vim.pending_closes.clear();
    crate::motions::move_line_end(&mut ed.buffer);
    break_undo_group_in_insert(ed);
    ed.push_buffer_cursor_to_textarea();
    false
}

/// Scroll up one full viewport height, moving the cursor with it.
/// Breaks the undo group. Returns `false` (no mutation).
pub(crate) fn insert_pageup_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    viewport_h: u16,
) -> bool {
    let rows = viewport_h.saturating_sub(2).max(1) as isize;
    scroll_cursor_rows(ed, -rows);
    false
}

/// Scroll down one full viewport height, moving the cursor with it.
/// Breaks the undo group. Returns `false` (no mutation).
pub(crate) fn insert_pagedown_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    viewport_h: u16,
) -> bool {
    let rows = viewport_h.saturating_sub(2).max(1) as isize;
    scroll_cursor_rows(ed, rows);
    false
}

/// Delete from the cursor back to the start of the previous word (`Ctrl-W`).
/// At col 0, joins with the previous line (vim semantics). Returns `true`
/// when something was deleted.
pub(crate) fn insert_ctrl_w_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(&ed.buffer);
    if cursor.row == 0 && cursor.col == 0 {
        return true;
    }
    crate::motions::move_word_back(&mut ed.buffer, false, 1, &ed.settings.iskeyword);
    let word_start = buf_cursor_pos(&ed.buffer);
    if word_start == cursor {
        return true;
    }
    buf_set_cursor_pos(&mut ed.buffer, cursor);
    ed.mutate_edit(Edit::DeleteRange {
        start: word_start,
        end: cursor,
        kind: MotionKind::Char,
    });
    ed.push_buffer_cursor_to_textarea();
    true
}

/// Delete from the cursor back to the start of the current line (`Ctrl-U`).
/// No-op when already at column 0. Returns `true` when something was deleted.
pub(crate) fn insert_ctrl_u_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(&ed.buffer);
    if cursor.col > 0 {
        ed.mutate_edit(Edit::DeleteRange {
            start: Position::new(cursor.row, 0),
            end: cursor,
            kind: MotionKind::Char,
        });
        ed.push_buffer_cursor_to_textarea();
    }
    true
}

/// Delete one character backwards (`Ctrl-H`) — alias for Backspace in insert
/// mode. Joins with the previous line when at col 0. Returns `true` when
/// something was deleted.
pub(crate) fn insert_ctrl_h_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(&ed.buffer);
    if cursor.col > 0 {
        ed.mutate_edit(Edit::DeleteRange {
            start: Position::new(cursor.row, cursor.col - 1),
            end: cursor,
            kind: MotionKind::Char,
        });
    } else if cursor.row > 0 {
        let prev_row = cursor.row - 1;
        let prev_chars = buf_line_chars(&ed.buffer, prev_row);
        ed.mutate_edit(Edit::JoinLines {
            row: prev_row,
            count: 1,
            with_space: false,
        });
        buf_set_cursor_rc(&mut ed.buffer, prev_row, prev_chars);
    }
    ed.push_buffer_cursor_to_textarea();
    true
}

/// Indent the current line by one `shiftwidth` and shift the cursor right by
/// the same amount (`Ctrl-T`). Returns `true`.
pub(crate) fn insert_ctrl_t_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    let (row, col) = ed.cursor();
    let sw = ed.settings().shiftwidth;
    indent_rows(ed, row, row, 1);
    ed.jump_cursor(row, col + sw);
    true
}

/// Outdent the current line by up to one `shiftwidth` and shift the cursor
/// left by the amount stripped (`Ctrl-D`). Returns `true`.
pub(crate) fn insert_ctrl_d_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    let (row, col) = ed.cursor();
    let before_len = buf_line_bytes(&ed.buffer, row);
    outdent_rows(ed, row, row, 1);
    let after_len = buf_line_bytes(&ed.buffer, row);
    let stripped = before_len.saturating_sub(after_len);
    let new_col = col.saturating_sub(stripped);
    ed.jump_cursor(row, new_col);
    true
}

/// Enter "one-shot normal" mode (`Ctrl-O`): suspend insert for the next
/// complete normal-mode command, then return to insert. Returns `false`
/// (no buffer mutation — only mode state changes).
pub(crate) fn insert_ctrl_o_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    ed.vim.one_shot_normal = true;
    ed.vim.mode = Mode::Normal;
    // Phase 6.3: keep current_mode in sync for callers that bypass step().
    ed.vim.current_mode = crate::VimMode::Normal;
    false
}

/// Arm the register-paste selector (`Ctrl-R`): the next typed character
/// names the register whose text will be inserted inline. Returns `false`
/// (no buffer mutation yet — mutation happens when the register char arrives).
pub(crate) fn insert_ctrl_r_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    ed.vim.insert_pending_register = true;
    false
}

/// Paste the contents of `reg` at the cursor (the body of `Ctrl-R {reg}`).
/// Unknown or empty registers are a no-op. Returns `true` when text was
/// inserted.
pub(crate) fn insert_paste_register_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    reg: char,
) -> bool {
    insert_register_text(ed, reg);
    // insert_register_text already calls mark_content_dirty internally;
    // return true to signal that the session row window should be widened.
    true
}

/// Exit insert mode to Normal: finish the insert session, step the cursor one
/// cell left (vim convention), record the `gi` target, and update the sticky
/// column. Clears the autopair pending-closes stack. Returns `true` (always
/// consumed — even if no buffer mutation, the mode change itself is a
/// meaningful step).
pub(crate) fn leave_insert_to_normal_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) -> bool {
    ed.vim.pending_closes.clear();

    // ── Abbreviation expansion on Esc ────────────────────────────────────────
    // Esc triggers expansion for all abbreviation types.
    if !ed.vim.abbrevs.is_empty() {
        check_and_apply_abbrev(ed, AbbrevTrigger::Esc);
    }

    finish_insert_session(ed);
    // Paired-tag auto-rename (issue #182). Must run BEFORE the cursor moves
    // left (the move-left is vim's "leave-insert cursor adjustment"; the
    // sync needs the post-insert cursor position to detect the tag name).
    sync_paired_tag_on_exit(ed);
    ed.vim.mode = Mode::Normal;
    // Phase 6.3: keep current_mode in sync for callers that bypass step().
    ed.vim.current_mode = crate::VimMode::Normal;
    let col = ed.cursor().1;
    ed.vim.last_insert_pos = Some(ed.cursor());
    if col > 0 {
        crate::motions::move_left(&mut ed.buffer, 1);
        ed.push_buffer_cursor_to_textarea();
    }
    ed.sticky_col = Some(ed.cursor().1);
    true
}

// ─── Phase 6.2: normal-mode primitive bridges ──────────────────────────────

/// Scroll direction for `scroll_full_page`, `scroll_half_page`, and
/// `scroll_line` controller methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDir {
    /// Move forward / downward (toward end of buffer).
    Down,
    /// Move backward / upward (toward start of buffer).
    Up,
}

// ── Insert-mode entry bridges ──────────────────────────────────────────────

/// `i` — begin Insert at the cursor. `count` is stored in the session for
/// insert-exit replay. Returns `true`.
pub(crate) fn enter_insert_i_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    begin_insert(ed, count.max(1), InsertReason::Enter(InsertEntry::I));
}

/// `I` — move to first non-blank then begin Insert. `count` stored for replay.
pub(crate) fn enter_insert_shift_i_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    move_first_non_whitespace(ed);
    begin_insert(ed, count.max(1), InsertReason::Enter(InsertEntry::ShiftI));
}

/// `a` — advance past the cursor char then begin Insert. `count` for replay.
pub(crate) fn enter_insert_a_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    crate::motions::move_right_to_end(&mut ed.buffer, 1);
    ed.push_buffer_cursor_to_textarea();
    begin_insert(ed, count.max(1), InsertReason::Enter(InsertEntry::A));
}

/// `A` — move to end-of-line then begin Insert. `count` for replay.
pub(crate) fn enter_insert_shift_a_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    crate::motions::move_line_end(&mut ed.buffer);
    crate::motions::move_right_to_end(&mut ed.buffer, 1);
    ed.push_buffer_cursor_to_textarea();
    begin_insert(ed, count.max(1), InsertReason::Enter(InsertEntry::ShiftA));
}

/// `o` — open a new line below the cursor and begin Insert.
/// When `formatoptions` has `o` and the current line is a comment, the
/// continuation prefix is inserted automatically.
pub(crate) fn open_line_below_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    use hjkl_buffer::{Edit, Position};
    ed.push_undo();
    begin_insert_noundo(ed, count.max(1), InsertReason::Open { above: false });
    ed.sync_buffer_content_from_textarea();
    let row = buf_cursor_pos(&ed.buffer).row;
    let line_chars = buf_line_chars(&ed.buffer, row);
    let prev_line = buf_line(&ed.buffer, row).unwrap_or_default();

    // formatoptions `o`: continue comment on open-below.
    let comment_cont = if ed.settings.formatoptions.contains('o') {
        continue_comment(&ed.buffer, &ed.settings, row)
    } else {
        None
    };

    let suffix = if let Some(cont) = comment_cont {
        format!("\n{cont}")
    } else {
        let indent = compute_enter_indent(&ed.settings, &prev_line);
        format!("\n{indent}")
    };
    ed.mutate_edit(Edit::InsertStr {
        at: Position::new(row, line_chars),
        text: suffix,
    });
    ed.push_buffer_cursor_to_textarea();
}

/// `O` — open a new line above the cursor and begin Insert.
/// When `formatoptions` has `o` and the current line is a comment, the
/// continuation prefix is inserted automatically on the new line above.
pub(crate) fn open_line_above_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    use hjkl_buffer::{Edit, Position};
    ed.push_undo();
    begin_insert_noundo(ed, count.max(1), InsertReason::Open { above: true });
    ed.sync_buffer_content_from_textarea();
    let row = buf_cursor_pos(&ed.buffer).row;

    // formatoptions `o`: continue comment on open-above (current line drives).
    let comment_cont = if ed.settings.formatoptions.contains('o') {
        continue_comment(&ed.buffer, &ed.settings, row)
    } else {
        None
    };

    // `new_line_content` is the text of the new line (without the trailing `\n`).
    // Used to position the cursor at the end of that content after the move.
    let (insert_text, new_line_content) = if let Some(cont) = comment_cont {
        let content = cont.clone();
        (format!("{cont}\n"), content)
    } else {
        // vim `O` autoindent copies the CURRENT line's indent (the line the
        // cursor sits on, which becomes the line *below* the new one), NOT the
        // line above. Using the line above wrongly inherits a deeper child's
        // indent when the cursor is on a shallower line (e.g. explorer tree:
        // `O` on a dir whose preceding row is its own nested child).
        let cur = buf_line(&ed.buffer, row).unwrap_or_default();
        let indent = compute_enter_indent(&ed.settings, &cur);
        let content = indent.clone();
        (format!("{indent}\n"), content)
    };
    ed.mutate_edit(Edit::InsertStr {
        at: Position::new(row, 0),
        text: insert_text,
    });
    let folds = crate::buffer_impl::SnapshotFoldProvider::from_buffer(&ed.buffer);
    crate::motions::move_up(&mut ed.buffer, &folds, 1, &mut ed.sticky_col);
    let new_row = buf_cursor_pos(&ed.buffer).row;
    buf_set_cursor_rc(&mut ed.buffer, new_row, new_line_content.chars().count());
    ed.push_buffer_cursor_to_textarea();
}

/// `R` — enter Replace mode (overstrike). `count` stored for replay.
pub(crate) fn enter_replace_mode_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    // Guard delegated to begin_insert which already checks modifiable/Blame.
    begin_insert(ed, count.max(1), InsertReason::Replace);
}

// ── Char / line ops ────────────────────────────────────────────────────────

/// `x` — delete `count` chars forward from the cursor, writing to the unnamed
/// register. Records `LastChange::CharDel` for dot-repeat.
pub(crate) fn delete_char_forward_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    do_char_delete(ed, true, count.max(1));
    if !ed.vim.replaying {
        ed.vim.last_change = Some(LastChange::CharDel {
            forward: true,
            count: count.max(1),
        });
    }
}

/// `X` — delete `count` chars backward from the cursor, writing to the unnamed
/// register. Records `LastChange::CharDel` for dot-repeat.
pub(crate) fn delete_char_backward_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    do_char_delete(ed, false, count.max(1));
    if !ed.vim.replaying {
        ed.vim.last_change = Some(LastChange::CharDel {
            forward: false,
            count: count.max(1),
        });
    }
}

/// `s` — substitute `count` chars (delete then enter Insert). Equivalent to
/// `cl`. Records `LastChange::OpMotion` for dot-repeat.
pub(crate) fn substitute_char_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.push_undo();
    ed.sync_buffer_content_from_textarea();
    for _ in 0..count.max(1) {
        let cursor = buf_cursor_pos(&ed.buffer);
        let line_chars = buf_line_chars(&ed.buffer, cursor.row);
        if cursor.col >= line_chars {
            break;
        }
        ed.mutate_edit(Edit::DeleteRange {
            start: cursor,
            end: Position::new(cursor.row, cursor.col + 1),
            kind: MotionKind::Char,
        });
    }
    ed.push_buffer_cursor_to_textarea();
    begin_insert_noundo(ed, 1, InsertReason::AfterChange);
    if !ed.vim.replaying {
        ed.vim.last_change = Some(LastChange::OpMotion {
            op: Operator::Change,
            motion: Motion::Right,
            count: count.max(1),
            inserted: None,
        });
    }
}

/// `S` — substitute the whole line (delete line contents then enter Insert).
/// Equivalent to `cc`. Records `LastChange::LineOp` for dot-repeat.
pub(crate) fn substitute_line_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    execute_line_op(ed, Operator::Change, count.max(1));
    if !ed.vim.replaying {
        ed.vim.last_change = Some(LastChange::LineOp {
            op: Operator::Change,
            count: count.max(1),
            inserted: None,
        });
    }
}

/// `D` — delete from the cursor to end-of-line, writing to the unnamed
/// register. Cursor parks on the new last char. Records for dot-repeat.
pub(crate) fn delete_to_eol_bridge<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    ed.push_undo();
    delete_to_eol(ed);
    crate::motions::move_left(&mut ed.buffer, 1);
    ed.push_buffer_cursor_to_textarea();
    if !ed.vim.replaying {
        ed.vim.last_change = Some(LastChange::DeleteToEol { inserted: None });
    }
}

/// `C` — change from the cursor to end-of-line (delete then enter Insert).
/// Equivalent to `c$`. Shares the delete path with `D`.
pub(crate) fn change_to_eol_bridge<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    ed.push_undo();
    delete_to_eol(ed);
    begin_insert_noundo(ed, 1, InsertReason::DeleteToEol);
}

/// `Y` — yank from the cursor to end-of-line (same as `y$` in Vim 8 default).
pub(crate) fn yank_to_eol_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    apply_op_with_motion(ed, Operator::Yank, &Motion::LineEnd, count.max(1));
}

/// `J` — join `count` lines (default 2) onto the current one, inserting a
/// single space between each pair (vim semantics). Records for dot-repeat.
pub(crate) fn join_line_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    // vim `[count]J` joins `count` lines together — i.e. `count - 1` joins.
    // Bare `J` (and `1J`) join the current line with the one below (1 join).
    let joins = count.max(2) - 1;
    for _ in 0..joins {
        ed.push_undo();
        join_line(ed);
    }
    if !ed.vim.replaying {
        ed.vim.last_change = Some(LastChange::JoinLine { count: joins });
    }
}

/// `~` — toggle the case of `count` chars from the cursor, advancing right.
/// Records `LastChange::ToggleCase` for dot-repeat.
pub(crate) fn toggle_case_at_cursor_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    for _ in 0..count.max(1) {
        ed.push_undo();
        toggle_case_at_cursor(ed);
    }
    if !ed.vim.replaying {
        ed.vim.last_change = Some(LastChange::ToggleCase {
            count: count.max(1),
        });
    }
}

/// `p` — paste the unnamed register (or `"reg` register) after the cursor.
/// Linewise yanks open a new line below; charwise pastes inline.
/// Records `LastChange::Paste` for dot-repeat.
pub(crate) fn paste_after_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    paste_bridge(ed, false, count, false, false);
}

/// `P` — paste the unnamed register (or `"reg` register) before the cursor.
/// Linewise yanks open a new line above; charwise pastes inline.
/// Records `LastChange::Paste` for dot-repeat.
pub(crate) fn paste_before_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    paste_bridge(ed, true, count, false, false);
}

/// Shared paste entry for `p`/`P`, `gp`/`gP` (`cursor_after`), and
/// `]p`/`[p` (`reindent`). Records `LastChange::Paste` for dot-repeat.
pub(crate) fn paste_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    before: bool,
    count: usize,
    cursor_after: bool,
    reindent: bool,
) {
    do_paste(ed, before, count.max(1), cursor_after, reindent);
    if !ed.vim.replaying {
        ed.vim.last_change = Some(LastChange::Paste {
            before,
            count: count.max(1),
            cursor_after,
            reindent,
        });
    }
}

// ── Jump bridges ───────────────────────────────────────────────────────────

/// `<C-o>` — jump back `count` entries in the jumplist, saving the current
/// position on the forward stack so `<C-i>` can return.
pub(crate) fn jump_back_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    for _ in 0..count.max(1) {
        jump_back(ed);
    }
}

/// `<C-i>` / `Tab` — redo `count` jumps on the forward stack, saving the
/// current position on the backward stack.
pub(crate) fn jump_forward_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    for _ in 0..count.max(1) {
        jump_forward(ed);
    }
}

// ── Scroll bridges ─────────────────────────────────────────────────────────

/// `<C-f>` / `<C-b>` — scroll the cursor by one full viewport height
/// (`h - 2` rows to preserve two-line overlap). `count` multiplies.
pub(crate) fn scroll_full_page_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    dir: ScrollDir,
    count: usize,
) {
    let rows = viewport_full_rows(ed, count) as isize;
    match dir {
        ScrollDir::Down => scroll_cursor_rows(ed, rows),
        ScrollDir::Up => scroll_cursor_rows(ed, -rows),
    }
}

/// `<C-d>` / `<C-u>` — scroll the cursor by half the viewport height.
/// `count` multiplies.
pub(crate) fn scroll_half_page_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    dir: ScrollDir,
    count: usize,
) {
    let rows = viewport_half_rows(ed, count) as isize;
    match dir {
        ScrollDir::Down => scroll_cursor_rows(ed, rows),
        ScrollDir::Up => scroll_cursor_rows(ed, -rows),
    }
}

/// `<C-e>` / `<C-y>` — scroll the viewport `count` lines without moving the
/// cursor (cursor is clamped to the new visible region if it would go
/// off-screen). `<C-e>` scrolls down; `<C-y>` scrolls up.
pub(crate) fn scroll_line_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    dir: ScrollDir,
    count: usize,
) {
    let n = count.max(1);
    let total = buf_row_count(&ed.buffer);
    let last = total.saturating_sub(1);
    let h = ed.viewport_height_value() as usize;
    let vp = ed.host().viewport();
    let cur_top = vp.top_row;
    let new_top = match dir {
        ScrollDir::Down => (cur_top + n).min(last),
        ScrollDir::Up => cur_top.saturating_sub(n),
    };
    ed.set_viewport_top(new_top);
    // Clamp cursor to stay within the new visible region.
    let (row, col) = ed.cursor();
    let bot = (new_top + h).saturating_sub(1).min(last);
    let clamped = row.max(new_top).min(bot);
    if clamped != row {
        buf_set_cursor_rc(&mut ed.buffer, clamped, col);
        ed.push_buffer_cursor_to_textarea();
    }
}

// ── Search bridges ─────────────────────────────────────────────────────────

/// `n` / `N` — repeat the last search `count` times. `forward = true` means
/// repeat in the original search direction; `false` inverts it (like `N`).
pub(crate) fn search_repeat_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    forward: bool,
    count: usize,
) {
    if let Some(pattern) = ed.vim.last_search.clone() {
        ed.push_search_pattern(&pattern);
    }
    if ed.search_state().pattern.is_none() {
        return;
    }
    let go_forward = ed.vim.last_search_forward == forward;
    for _ in 0..count.max(1) {
        if go_forward {
            ed.search_advance_forward(true);
        } else {
            ed.search_advance_backward(true);
        }
    }
    ed.push_buffer_cursor_to_textarea();
}

/// `*` / `#` / `g*` / `g#` — search for the word under the cursor.
/// `forward` picks search direction; `whole_word` wraps in `\b...\b`.
/// `count` repeats the advance.
pub(crate) fn word_search_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    forward: bool,
    whole_word: bool,
    count: usize,
) {
    word_at_cursor_search(ed, forward, whole_word, count.max(1));
}

// ── Undo / redo confirmation wrappers (already public on Editor) ───────────

/// `u` bridge — identical to `do_undo`; retained for Phase 6.6b audit.
/// The FSM now calls `ed.undo()` directly (Phase 6.6a).
#[allow(dead_code)]
#[inline]
pub(crate) fn do_undo_bridge<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    do_undo(ed);
}

// ─── Phase 6.3: visual-mode primitive bridges ──────────────────────────────
//
// Each `pub(crate)` free function is the extractable body of one visual-mode
// transition. These bridges set `vim.mode` directly AND write `current_mode`
// so that `Editor::vim_mode()` can read from the stable field without going
// through `public_mode()`.
//
// Pattern identical to Phase 6.1 / 6.2:
//   - Bridge fn is `pub(crate) fn *_bridge<H: Host>(ed, …)` in this file.
//   - Public wrapper is `pub fn *(&mut self, …)` in `editor.rs` with rustdoc.

/// Drop the `Blame` view overlay whenever the input mode is no longer
/// `Normal`. BLAME is a Normal-only read-only view; entering Insert/Visual/etc.
/// (by keyboard, mouse drag, or programmatic transition) implicitly leaves it.
/// Called from every mode-transition funnel so the FSM is the single source of
/// truth — the host never has to police this.
#[inline]
pub(crate) fn drop_blame_if_left_normal<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    if ed.vim.current_mode != crate::VimMode::Normal {
        ed.vim.view = crate::ViewMode::Normal;
    }
}

/// Helper — set both the FSM-internal `mode` and the stable `current_mode`
/// field in one call. Every Phase 6.3 bridge that changes mode calls this so
/// `vim_mode()` stays correct without going through the FSM's `step()` loop.
#[inline]
pub(crate) fn set_vim_mode_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    mode: Mode,
) {
    ed.vim.mode = mode;
    ed.vim.current_mode = ed.vim.public_mode();
    drop_blame_if_left_normal(ed);
}

/// `v` from Normal — enter charwise Visual mode. Anchors at the current
/// cursor position; the cursor IS the live end of the selection.
pub(crate) fn enter_visual_char_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    let cur = ed.cursor();
    ed.vim.visual_anchor = cur;
    set_vim_mode_bridge(ed, Mode::Visual);
}

/// `V` from Normal — enter linewise Visual mode. Anchors the whole line
/// containing the current cursor; `o` still swaps the anchor row.
pub(crate) fn enter_visual_line_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    let (row, _) = ed.cursor();
    ed.vim.visual_line_anchor = row;
    set_vim_mode_bridge(ed, Mode::VisualLine);
}

/// `<C-v>` from Normal — enter Visual-block mode. Anchors at the current
/// cursor; `block_vcol` is seeded from the cursor column so h/l navigation
/// preserves the desired virtual column.
pub(crate) fn enter_visual_block_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    let cur = ed.cursor();
    ed.vim.block_anchor = cur;
    ed.vim.block_vcol = cur.1;
    set_vim_mode_bridge(ed, Mode::VisualBlock);
}

/// Esc from any visual mode — set `<` / `>` marks (per `:h v_:`), stash the
/// selection for `gv` re-entry, and return to Normal. Replicates the
/// `pre_visual_snapshot` logic in `step()` so callers outside the FSM get
/// identical behaviour.
pub(crate) fn exit_visual_to_normal_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    // Build the same snapshot that `step()` captures at pre-step time.
    let snap: Option<LastVisual> = match ed.vim.mode {
        Mode::Visual => Some(LastVisual {
            mode: Mode::Visual,
            anchor: ed.vim.visual_anchor,
            cursor: ed.cursor(),
            block_vcol: 0,
        }),
        Mode::VisualLine => Some(LastVisual {
            mode: Mode::VisualLine,
            anchor: (ed.vim.visual_line_anchor, 0),
            cursor: ed.cursor(),
            block_vcol: 0,
        }),
        Mode::VisualBlock => Some(LastVisual {
            mode: Mode::VisualBlock,
            anchor: ed.vim.block_anchor,
            cursor: ed.cursor(),
            block_vcol: ed.vim.block_vcol,
        }),
        _ => None,
    };
    // Transition to Normal first (matches FSM order).
    ed.vim.pending = Pending::None;
    ed.vim.count = 0;
    ed.vim.insert_session = None;
    set_vim_mode_bridge(ed, Mode::Normal);
    // Set `<` / `>` marks and stash `last_visual` — mirrors the post-step
    // logic in `step()` that fires when a visual → non-visual transition
    // is detected.
    if let Some(snap) = snap {
        let (lo, hi) = match snap.mode {
            Mode::Visual => {
                if snap.anchor <= snap.cursor {
                    (snap.anchor, snap.cursor)
                } else {
                    (snap.cursor, snap.anchor)
                }
            }
            Mode::VisualLine => {
                let r_lo = snap.anchor.0.min(snap.cursor.0);
                let r_hi = snap.anchor.0.max(snap.cursor.0);
                let vl_rope = ed.buffer().rope();
                let r_hi_clamped = r_hi.min(vl_rope.len_lines().saturating_sub(1));
                let last_col = hjkl_buffer::rope_line_str(&vl_rope, r_hi_clamped)
                    .chars()
                    .count()
                    .saturating_sub(1);
                ((r_lo, 0), (r_hi, last_col))
            }
            Mode::VisualBlock => {
                let (r1, c1) = snap.anchor;
                let (r2, c2) = snap.cursor;
                ((r1.min(r2), c1.min(c2)), (r1.max(r2), c1.max(c2)))
            }
            _ => {
                if snap.anchor <= snap.cursor {
                    (snap.anchor, snap.cursor)
                } else {
                    (snap.cursor, snap.anchor)
                }
            }
        };
        ed.set_mark('<', lo);
        ed.set_mark('>', hi);
        ed.vim.last_visual = Some(snap);
    }
}

/// `o` in Visual / VisualLine / VisualBlock — swap the cursor and anchor
/// without mutating the selection range. In charwise mode the cursor jumps
/// to the old anchor and the anchor takes the old cursor. In linewise mode
/// the anchor *row* swaps with the current cursor row. In block mode the
/// block corners swap.
pub(crate) fn visual_o_toggle_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    match ed.vim.mode {
        Mode::Visual => {
            let cur = ed.cursor();
            let anchor = ed.vim.visual_anchor;
            ed.vim.visual_anchor = cur;
            ed.jump_cursor(anchor.0, anchor.1);
        }
        Mode::VisualLine => {
            let cur_row = ed.cursor().0;
            let anchor_row = ed.vim.visual_line_anchor;
            ed.vim.visual_line_anchor = cur_row;
            ed.jump_cursor(anchor_row, 0);
        }
        Mode::VisualBlock => {
            let cur = ed.cursor();
            let anchor = ed.vim.block_anchor;
            ed.vim.block_anchor = cur;
            ed.vim.block_vcol = anchor.1;
            ed.jump_cursor(anchor.0, anchor.1);
        }
        _ => {}
    }
}

/// `gv` — restore the last visual selection (mode + anchor + cursor).
/// No-op if no selection was ever stored. Mirrors the `gv` arm in
/// `handle_normal_g`.
pub(crate) fn reenter_last_visual_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    if let Some(snap) = ed.vim.last_visual {
        match snap.mode {
            Mode::Visual => {
                ed.vim.visual_anchor = snap.anchor;
                set_vim_mode_bridge(ed, Mode::Visual);
            }
            Mode::VisualLine => {
                ed.vim.visual_line_anchor = snap.anchor.0;
                set_vim_mode_bridge(ed, Mode::VisualLine);
            }
            Mode::VisualBlock => {
                ed.vim.block_anchor = snap.anchor;
                ed.vim.block_vcol = snap.block_vcol;
                set_vim_mode_bridge(ed, Mode::VisualBlock);
            }
            _ => {}
        }
        ed.jump_cursor(snap.cursor.0, snap.cursor.1);
    }
}

/// Direct mode-transition entry point for external controllers (e.g.
/// hjkl-vim). Sets both the FSM-internal `mode` and the stable
/// `current_mode`. Use sparingly — prefer the semantic primitives
/// (`enter_visual_char_bridge`, `enter_insert_i_bridge`, …) which also
/// set up the required bookkeeping (anchors, sessions, …).
pub(crate) fn set_mode_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    mode: crate::VimMode,
) {
    let internal = match mode {
        crate::VimMode::Normal => Mode::Normal,
        crate::VimMode::Insert => Mode::Insert,
        crate::VimMode::Visual => Mode::Visual,
        crate::VimMode::VisualLine => Mode::VisualLine,
        crate::VimMode::VisualBlock => Mode::VisualBlock,
    };
    ed.vim.mode = internal;
    ed.vim.current_mode = mode;
    drop_blame_if_left_normal(ed);
}

// ─── Normal / Visual / Operator-pending dispatcher removed in Phase 6.6g.3 ──
//
// `step_normal` and all private dispatch helpers (handle_after_op,
// handle_after_g, handle_after_z, handle_normal_only, etc.) were deleted.
// The canonical FSM body lives in `hjkl-vim::normal`. Use
// `hjkl_vim::dispatch_input` as the entry point.
//
// DELETED FUNCTION SIGNATURE (for archaeology):
// pub(crate) fn step_normal<H: crate::types::Host>(ed: ..., input: Input) -> bool {

/// `m{ch}` — public controller entry point. Validates `ch` (must be
/// alphanumeric to match vim's mark-name rules) and records the current
/// cursor position under that name. Promoted to the public surface in 0.6.7
/// so the hjkl-vim `PendingState::SetMark` reducer can dispatch
/// `EngineCmd::SetMark` without re-entering the engine FSM.
/// `handle_set_mark` delegates here to avoid logic duplication.
pub(crate) fn set_mark_at_cursor<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    ch: char,
) {
    if ch.is_ascii_lowercase() {
        let pos = ed.cursor();
        ed.set_mark(ch, pos);
    } else if ch.is_ascii_uppercase() {
        let pos = ed.cursor();
        let bid = ed.current_buffer_id();
        ed.set_global_mark(ch, bid, pos);
        tracing::debug!(
            mark = ch as u32,
            buffer_id = bid,
            row = pos.0,
            col = pos.1,
            "global mark set"
        );
    }
    // Invalid chars silently no-op (mirrors handle_set_mark behaviour).
}

/// `'<ch>` / `` `<ch> `` — public controller entry point for lowercase and
/// special marks. Validates `ch` against the set of legal mark names
/// (lowercase, special: `'`/`` ` ``/`.`/`[`/`]`/`<`/`>`), resolves the
/// target position, and jumps the cursor. `linewise = true` → row only, col
/// snaps to first non-blank; `linewise = false` → exact (row, col).
///
/// Uppercase marks are handled by [`try_goto_mark`] which can return a
/// `MarkJump::CrossBuffer` for cross-buffer jumps.
pub(crate) fn goto_mark<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    ch: char,
    linewise: bool,
) {
    let target = match ch {
        'a'..='z' => ed.mark(ch),
        '\'' | '`' => ed.vim.jump_back.last().copied(),
        '.' => ed.vim.last_edit_pos,
        '[' | ']' | '<' | '>' => ed.mark(ch),
        _ => None,
    };
    let Some((row, col)) = target else {
        return;
    };
    let pre = ed.cursor();
    let (r, c_clamped) = clamp_pos(ed, (row, col));
    if linewise {
        buf_set_cursor_rc(&mut ed.buffer, r, 0);
        ed.push_buffer_cursor_to_textarea();
        move_first_non_whitespace(ed);
    } else {
        buf_set_cursor_rc(&mut ed.buffer, r, c_clamped);
        ed.push_buffer_cursor_to_textarea();
    }
    if ed.cursor() != pre {
        ed.push_jump(pre);
    }
    ed.sticky_col = Some(ed.cursor().1);
}

/// Unified mark-jump entry point that returns a [`crate::editor::MarkJump`]
/// so the app layer can decide whether to switch buffers.
///
/// - Uppercase marks (`'A'`–`'Z'`) look in `global_marks`. If the stored
///   `buffer_id` differs from `ed.current_buffer_id()`, returns
///   `CrossBuffer`. Same-buffer uppercase marks execute the jump normally.
/// - All other legal mark chars delegate to [`goto_mark`] and return
///   `SameBuffer`.
pub(crate) fn try_goto_mark<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    ch: char,
    linewise: bool,
) -> crate::editor::MarkJump {
    use crate::editor::MarkJump;
    match ch {
        'A'..='Z' => {
            let Some((bid, row, col)) = ed.global_mark(ch) else {
                return MarkJump::Unset;
            };
            if bid != ed.current_buffer_id() {
                tracing::debug!(
                    mark = ch as u32,
                    buffer_id = bid,
                    row,
                    col,
                    "global mark cross-buffer jump"
                );
                return MarkJump::CrossBuffer {
                    buffer_id: bid,
                    row,
                    col,
                };
            }
            // Same buffer — execute the jump normally.
            let pre = ed.cursor();
            let (r, c_clamped) = clamp_pos(ed, (row, col));
            if linewise {
                buf_set_cursor_rc(&mut ed.buffer, r, 0);
                ed.push_buffer_cursor_to_textarea();
                move_first_non_whitespace(ed);
            } else {
                buf_set_cursor_rc(&mut ed.buffer, r, c_clamped);
                ed.push_buffer_cursor_to_textarea();
            }
            if ed.cursor() != pre {
                ed.push_jump(pre);
            }
            ed.sticky_col = Some(ed.cursor().1);
            MarkJump::SameBuffer
        }
        'a'..='z' | '\'' | '`' | '.' | '[' | ']' | '<' | '>' => {
            goto_mark(ed, ch, linewise);
            MarkJump::SameBuffer
        }
        _ => MarkJump::Unset,
    }
}

/// `true` when `op` records a `last_change` entry for dot-repeat purposes.
/// Promoted to `pub` in Phase 6.6e so `hjkl-vim::normal` can use it without
/// duplicating the logic.
pub fn op_is_change(op: Operator) -> bool {
    matches!(op, Operator::Delete | Operator::Change)
}

// ─── Jumplist (Ctrl-o / Ctrl-i) ────────────────────────────────────────────

/// Max jumplist depth. Matches vim default.
pub(crate) const JUMPLIST_MAX: usize = 100;

/// `Ctrl-o` — jump back to the most recent pre-jump position. Saves
/// the current cursor onto the forward stack so `Ctrl-i` can return.
fn jump_back<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    let Some(target) = ed.vim.jump_back.pop() else {
        return;
    };
    let cur = ed.cursor();
    ed.vim.jump_fwd.push(cur);
    let (r, c) = clamp_pos(ed, target);
    ed.jump_cursor(r, c);
    ed.sticky_col = Some(c);
}

/// `Ctrl-i` / `Tab` — redo the last `Ctrl-o`. Saves the current cursor
/// onto the back stack.
fn jump_forward<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    let Some(target) = ed.vim.jump_fwd.pop() else {
        return;
    };
    let cur = ed.cursor();
    ed.vim.jump_back.push(cur);
    if ed.vim.jump_back.len() > JUMPLIST_MAX {
        ed.vim.jump_back.remove(0);
    }
    let (r, c) = clamp_pos(ed, target);
    ed.jump_cursor(r, c);
    ed.sticky_col = Some(c);
}

/// Clamp a stored `(row, col)` to the live buffer in case edits
/// shrunk the document between push and pop.
fn clamp_pos<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    pos: (usize, usize),
) -> (usize, usize) {
    let last_row = buf_row_count(&ed.buffer).saturating_sub(1);
    let r = pos.0.min(last_row);
    let line_len = buf_line_chars(&ed.buffer, r);
    let c = pos.1.min(line_len.saturating_sub(1));
    (r, c)
}

/// True for motions that vim treats as jumps (pushed onto the jumplist).
fn is_big_jump(motion: &Motion) -> bool {
    matches!(
        motion,
        Motion::FileTop
            | Motion::FileBottom
            | Motion::MatchBracket
            | Motion::WordAtCursor { .. }
            | Motion::SearchNext { .. }
            | Motion::ViewportTop
            | Motion::ViewportMiddle
            | Motion::ViewportBottom
    )
}

// ─── Scroll helpers (Ctrl-d / Ctrl-u / Ctrl-f / Ctrl-b) ────────────────────

/// Half-viewport row count, with a floor of 1 so tiny / un-rendered
/// viewports still step by a single row. `count` multiplies.
fn viewport_half_rows<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) -> usize {
    let h = ed.viewport_height_value() as usize;
    (h / 2).max(1).saturating_mul(count.max(1))
}

/// Full-viewport row count. Vim conventionally keeps 2 lines of overlap
/// between successive `Ctrl-f` pages; we approximate with `h - 2`.
fn viewport_full_rows<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) -> usize {
    let h = ed.viewport_height_value() as usize;
    h.saturating_sub(2).max(1).saturating_mul(count.max(1))
}

/// Move the cursor by `delta` rows (positive = down, negative = up),
/// clamp to the document, then land at the first non-blank on the new
/// row. The textarea viewport auto-scrolls to keep the cursor visible
/// when the cursor pushes off-screen.
fn scroll_cursor_rows<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    delta: isize,
) {
    if delta == 0 {
        return;
    }
    ed.sync_buffer_content_from_textarea();
    let (row, _) = ed.cursor();
    let last_row = buf_row_count(&ed.buffer).saturating_sub(1);
    let target = (row as isize + delta).max(0).min(last_row as isize) as usize;
    buf_set_cursor_rc(&mut ed.buffer, target, 0);
    crate::motions::move_first_non_blank(&mut ed.buffer);
    ed.push_buffer_cursor_to_textarea();
    ed.sticky_col = Some(buf_cursor_pos(&ed.buffer).col);
}

// ─── Motion parsing ────────────────────────────────────────────────────────

/// Parse the first key of a normal/visual-mode motion. Returns `None` for
/// keys that don't start a motion (operator keys, command keys, etc.).
/// Promoted to `pub` in Phase 6.6e so `hjkl-vim::normal` can call it.
pub fn parse_motion(input: &Input) -> Option<Motion> {
    if input.ctrl {
        return None;
    }
    match input.key {
        Key::Char('h') | Key::Backspace | Key::Left => Some(Motion::Left),
        Key::Char('l') | Key::Right => Some(Motion::Right),
        Key::Char('j') | Key::Down => Some(Motion::Down),
        // `+` / `<CR>` — first non-blank of next line (linewise, count-aware).
        Key::Char('+') | Key::Enter => Some(Motion::FirstNonBlankNextLine),
        // `-` — first non-blank of previous line (linewise, count-aware).
        Key::Char('-') => Some(Motion::FirstNonBlankPrevLine),
        // `_` — first non-blank of current line, or count-1 lines down (linewise).
        Key::Char('_') => Some(Motion::FirstNonBlankLine),
        Key::Char('k') | Key::Up => Some(Motion::Up),
        Key::Char('w') => Some(Motion::WordFwd),
        Key::Char('W') => Some(Motion::BigWordFwd),
        Key::Char('b') => Some(Motion::WordBack),
        Key::Char('B') => Some(Motion::BigWordBack),
        Key::Char('e') => Some(Motion::WordEnd),
        Key::Char('E') => Some(Motion::BigWordEnd),
        Key::Char('0') | Key::Home => Some(Motion::LineStart),
        Key::Char('^') => Some(Motion::FirstNonBlank),
        Key::Char('$') | Key::End => Some(Motion::LineEnd),
        Key::Char('G') => Some(Motion::FileBottom),
        Key::Char('%') => Some(Motion::MatchBracket),
        Key::Char(';') => Some(Motion::FindRepeat { reverse: false }),
        Key::Char(',') => Some(Motion::FindRepeat { reverse: true }),
        Key::Char('*') => Some(Motion::WordAtCursor {
            forward: true,
            whole_word: true,
        }),
        Key::Char('#') => Some(Motion::WordAtCursor {
            forward: false,
            whole_word: true,
        }),
        Key::Char('n') => Some(Motion::SearchNext { reverse: false }),
        Key::Char('N') => Some(Motion::SearchNext { reverse: true }),
        Key::Char('H') => Some(Motion::ViewportTop),
        Key::Char('M') => Some(Motion::ViewportMiddle),
        Key::Char('L') => Some(Motion::ViewportBottom),
        Key::Char('{') => Some(Motion::ParagraphPrev),
        Key::Char('}') => Some(Motion::ParagraphNext),
        Key::Char('(') => Some(Motion::SentencePrev),
        Key::Char(')') => Some(Motion::SentenceNext),
        Key::Char('|') => Some(Motion::GotoColumn),
        _ => None,
    }
}

// ─── Motion execution ──────────────────────────────────────────────────────

pub(crate) fn execute_motion<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    motion: Motion,
    count: usize,
) {
    let count = count.max(1);
    // `;`/`,` smart fallback: if the last horizontal motion was a sneak
    // digraph, repeat via apply_sneak instead of find-char.
    if let Motion::FindRepeat { reverse } = motion
        && ed.vim.last_horizontal_motion == LastHorizontalMotion::Sneak
    {
        if let Some(((c1, c2), fwd)) = ed.vim.last_sneak {
            let effective_fwd = if reverse { !fwd } else { fwd };
            apply_sneak(ed, c1, c2, effective_fwd, count);
        }
        return;
    }
    // FindRepeat needs the stored direction.
    let motion = match motion {
        Motion::FindRepeat { reverse } => match ed.vim.last_find {
            Some((ch, forward, till)) => Motion::Find {
                ch,
                forward: if reverse { !forward } else { forward },
                till,
            },
            None => return,
        },
        other => other,
    };
    let pre_pos = ed.cursor();
    let pre_col = pre_pos.1;
    apply_motion_cursor(ed, &motion, count);
    let post_pos = ed.cursor();
    if is_big_jump(&motion) && pre_pos != post_pos {
        ed.push_jump(pre_pos);
    }
    apply_sticky_col(ed, &motion, pre_col);
    // Phase 7b: keep the migration buffer's cursor + viewport in
    // lockstep with the textarea after every motion. Once 7c lands
    // (motions ported onto the buffer's API), this flips: the
    // buffer becomes authoritative and the textarea mirrors it.
    ed.sync_buffer_from_textarea();
}

// ─── Keymap-layer motion controller ────────────────────────────────────────

/// Wrapper around `execute_motion` that also syncs `block_vcol` when in
/// VisualBlock mode. The engine FSM's `step()` already does this (line ~2001);
/// the keymap path (`apply_motion_kind`) must do the same so VisualBlock h/l
/// extend the highlighted region correctly.
///
/// `update_block_vcol` is only a no-op for vertical / non-horizontal motions
/// (Up, Down, FileTop, FileBottom, Search), so passing every motion through is
/// safe — the function's own match arm handles the no-op case.
fn execute_motion_with_block_vcol<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    motion: Motion,
    count: usize,
) {
    let motion_copy = motion.clone();
    execute_motion(ed, motion, count);
    if ed.vim.mode == Mode::VisualBlock {
        update_block_vcol(ed, &motion_copy);
    }
}

/// Execute a `crate::MotionKind` cursor motion. Called by the host's
/// `Editor::apply_motion` controller method — the keymap dispatch path for
/// Phase 3a of kryptic-sh/hjkl#69.
///
/// Maps each variant to the same internal primitives used by the engine FSM
/// so cursor, sticky column, scroll, and sync semantics are identical.
///
/// # Visual-mode post-motion sync audit (2026-05-13)
///
/// After `execute_motion`, two things are conditional on visual mode:
///
/// 1. **VisualBlock `block_vcol` sync** — `update_block_vcol(ed, &motion)` is
///    called when `mode == Mode::VisualBlock`.  This is replicated here via
///    `execute_motion_with_block_vcol` for every motion variant below.
///
/// 2. **`last_find` update** — `Motion::Find` is dispatched through
///    `Pending::Find → apply_find_char` (in hjkl-vim), which writes `last_find`
///    itself.  A post-motion `last_find` write here would be dead code.  The keymap
///    path writes `last_find` in `apply_find_char` (called from
///    `Editor::find_char`), so no gap exists here.
///
/// No VisualLine-specific or Visual-specific post-motion work exists in the
/// FSM: anchors (`visual_anchor`, `visual_line_anchor`, `block_anchor`) are
/// only written on mode-entry or `o`-swap, never on motion.  The `<`/`>`
/// mark update in `step()` fires only on visual→normal transition, not after
/// each motion.  There are **no further sync gaps** beyond the `block_vcol`
/// fix already applied above.
pub(crate) fn apply_motion_kind<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    kind: crate::MotionKind,
    count: usize,
) {
    let count = count.max(1);
    match kind {
        crate::MotionKind::CharLeft => {
            execute_motion_with_block_vcol(ed, Motion::Left, count);
        }
        crate::MotionKind::CharRight => {
            execute_motion_with_block_vcol(ed, Motion::Right, count);
        }
        crate::MotionKind::LineDown => {
            execute_motion_with_block_vcol(ed, Motion::Down, count);
        }
        crate::MotionKind::LineUp => {
            execute_motion_with_block_vcol(ed, Motion::Up, count);
        }
        crate::MotionKind::FirstNonBlankDown => {
            // `+`: move down `count` lines then land on first non-blank.
            // Not a big-jump (no jump-list entry), sticky col set to the
            // landed column (first non-blank). Mirrors scroll_cursor_rows
            // semantics but goes through the fold-aware buffer motion path.
            let folds = crate::buffer_impl::SnapshotFoldProvider::from_buffer(&ed.buffer);
            crate::motions::move_down(&mut ed.buffer, &folds, count, &mut ed.sticky_col);
            crate::motions::move_first_non_blank(&mut ed.buffer);
            ed.push_buffer_cursor_to_textarea();
            ed.sticky_col = Some(buf_cursor_pos(&ed.buffer).col);
            ed.sync_buffer_from_textarea();
        }
        crate::MotionKind::FirstNonBlankUp => {
            // `-`: move up `count` lines then land on first non-blank.
            // Same pattern as FirstNonBlankDown, direction reversed.
            let folds = crate::buffer_impl::SnapshotFoldProvider::from_buffer(&ed.buffer);
            crate::motions::move_up(&mut ed.buffer, &folds, count, &mut ed.sticky_col);
            crate::motions::move_first_non_blank(&mut ed.buffer);
            ed.push_buffer_cursor_to_textarea();
            ed.sticky_col = Some(buf_cursor_pos(&ed.buffer).col);
            ed.sync_buffer_from_textarea();
        }
        crate::MotionKind::WordForward => {
            execute_motion_with_block_vcol(ed, Motion::WordFwd, count);
        }
        crate::MotionKind::BigWordForward => {
            execute_motion_with_block_vcol(ed, Motion::BigWordFwd, count);
        }
        crate::MotionKind::WordBackward => {
            execute_motion_with_block_vcol(ed, Motion::WordBack, count);
        }
        crate::MotionKind::BigWordBackward => {
            execute_motion_with_block_vcol(ed, Motion::BigWordBack, count);
        }
        crate::MotionKind::WordEnd => {
            execute_motion_with_block_vcol(ed, Motion::WordEnd, count);
        }
        crate::MotionKind::BigWordEnd => {
            execute_motion_with_block_vcol(ed, Motion::BigWordEnd, count);
        }
        crate::MotionKind::LineStart => {
            // `0` / `<Home>`: first column of the current line.
            // count is ignored — matches vim `0` semantics.
            execute_motion_with_block_vcol(ed, Motion::LineStart, 1);
        }
        crate::MotionKind::FirstNonBlank => {
            // `^`: first non-blank column on the current line.
            // count is ignored — matches vim `^` semantics.
            execute_motion_with_block_vcol(ed, Motion::FirstNonBlank, 1);
        }
        crate::MotionKind::GotoLine => {
            // `G`: bare `G` → last line; `count G` → jump to line `count`.
            // apply_motion_kind normalises the raw count to count.max(1)
            // above, so count == 1 means "bare G" (last line) and count > 1
            // means "go to line N". execute_motion's FileBottom arm applies
            // the same `count > 1` check before calling move_bottom, so the
            // convention aligns: pass count straight through.
            // FileBottom is vertical — update_block_vcol is a no-op here
            // (preserves vcol), so the helper is safe to use.
            execute_motion_with_block_vcol(ed, Motion::FileBottom, count);
        }
        crate::MotionKind::LineEnd => {
            // `$` / `<End>`: last character on the current line.
            // count is ignored at the keymap-path level (vim `N$` moves
            // down N-1 lines then lands at line-end; not yet wired).
            execute_motion_with_block_vcol(ed, Motion::LineEnd, 1);
        }
        crate::MotionKind::FindRepeat => {
            // `;` — repeat last f/F/t/T in the same direction.
            // execute_motion resolves FindRepeat via ed.vim.last_find;
            // no-op if no prior find exists (None arm returns early).
            execute_motion_with_block_vcol(ed, Motion::FindRepeat { reverse: false }, count);
        }
        crate::MotionKind::FindRepeatReverse => {
            // `,` — repeat last f/F/t/T in the reverse direction.
            // execute_motion resolves FindRepeat via ed.vim.last_find;
            // no-op if no prior find exists (None arm returns early).
            execute_motion_with_block_vcol(ed, Motion::FindRepeat { reverse: true }, count);
        }
        crate::MotionKind::BracketMatch => {
            // `%` — jump to the matching bracket.
            // count is passed through; engine-side matching_bracket handles
            // the no-match case as a no-op (cursor stays). Engine FSM arm
            // for `%` in parse_motion is kept intact for macro-replay.
            execute_motion_with_block_vcol(ed, Motion::MatchBracket, count);
        }
        crate::MotionKind::ViewportTop => {
            // `H` — cursor to top of visible viewport, then count-1 rows down.
            // Engine FSM arm for `H` in parse_motion is kept intact for macro-replay.
            execute_motion_with_block_vcol(ed, Motion::ViewportTop, count);
        }
        crate::MotionKind::ViewportMiddle => {
            // `M` — cursor to middle of visible viewport; count ignored.
            // Engine FSM arm for `M` in parse_motion is kept intact for macro-replay.
            execute_motion_with_block_vcol(ed, Motion::ViewportMiddle, count);
        }
        crate::MotionKind::ViewportBottom => {
            // `L` — cursor to bottom of visible viewport, then count-1 rows up.
            // Engine FSM arm for `L` in parse_motion is kept intact for macro-replay.
            execute_motion_with_block_vcol(ed, Motion::ViewportBottom, count);
        }
        crate::MotionKind::HalfPageDown => {
            // `<C-d>` — half page down, count multiplies the distance.
            // Calls scroll_cursor_rows directly rather than adding a Motion enum
            // variant, keeping engine Motion churn minimal.
            scroll_cursor_rows(ed, viewport_half_rows(ed, count) as isize);
        }
        crate::MotionKind::HalfPageUp => {
            // `<C-u>` — half page up, count multiplies the distance.
            // Direct call mirrors the FSM Ctrl-u arm. No new Motion variant.
            scroll_cursor_rows(ed, -(viewport_half_rows(ed, count) as isize));
        }
        crate::MotionKind::FullPageDown => {
            // `<C-f>` — full page down (2-line overlap), count multiplies.
            // Direct call mirrors the FSM Ctrl-f arm. No new Motion variant.
            scroll_cursor_rows(ed, viewport_full_rows(ed, count) as isize);
        }
        crate::MotionKind::FullPageUp => {
            // `<C-b>` — full page up (2-line overlap), count multiplies.
            // Direct call mirrors the FSM Ctrl-b arm. No new Motion variant.
            scroll_cursor_rows(ed, -(viewport_full_rows(ed, count) as isize));
        }
        crate::MotionKind::FirstNonBlankLine => {
            execute_motion_with_block_vcol(ed, Motion::FirstNonBlankLine, count);
        }
        crate::MotionKind::SectionBackward => {
            execute_motion_with_block_vcol(ed, Motion::SectionBackward, count);
        }
        crate::MotionKind::SectionForward => {
            execute_motion_with_block_vcol(ed, Motion::SectionForward, count);
        }
        crate::MotionKind::SectionEndBackward => {
            execute_motion_with_block_vcol(ed, Motion::SectionEndBackward, count);
        }
        crate::MotionKind::SectionEndForward => {
            execute_motion_with_block_vcol(ed, Motion::SectionEndForward, count);
        }
    }
}

/// Restore the cursor to the sticky column after vertical motions and
/// sync the sticky column to the current column after horizontal ones.
/// `pre_col` is the cursor column captured *before* the motion — used
/// to bootstrap the sticky value on the very first motion.
fn apply_sticky_col<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    motion: &Motion,
    pre_col: usize,
) {
    if is_vertical_motion(motion) {
        let want = ed.sticky_col.unwrap_or(pre_col);
        // Record the desired column so the next vertical motion sees
        // it even if we currently clamped to a shorter row.
        ed.sticky_col = Some(want);
        let (row, _) = ed.cursor();
        let line_len = buf_line_chars(&ed.buffer, row);
        // Clamp to the last char on non-empty lines (vim normal-mode
        // never parks the cursor one past end of line). Empty lines
        // collapse to col 0.
        let max_col = line_len.saturating_sub(1);
        let target = want.min(max_col);
        // raw primitive: this function MUST preserve the un-clamped `want`
        // already stored in `ed.sticky_col`; `jump_cursor` would overwrite
        // it with the clamped `target`.
        buf_set_cursor_rc(&mut ed.buffer, row, target);
    } else {
        // Horizontal motion or non-motion: sticky column tracks the
        // new cursor column so the *next* vertical motion aims there.
        ed.sticky_col = Some(ed.cursor().1);
    }
}

fn is_vertical_motion(motion: &Motion) -> bool {
    // Only j / k preserve the sticky column. Everything else (search,
    // gg / G, word jumps, etc.) lands at the match's own column so the
    // sticky value should sync to the new cursor column.
    matches!(
        motion,
        Motion::Up | Motion::Down | Motion::ScreenUp | Motion::ScreenDown
    )
}

fn apply_motion_cursor<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    motion: &Motion,
    count: usize,
) {
    apply_motion_cursor_ctx(ed, motion, count, false)
}

pub(crate) fn apply_motion_cursor_ctx<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    motion: &Motion,
    count: usize,
    as_operator: bool,
) {
    match motion {
        Motion::Left => {
            // `h` — Buffer clamps at col 0 (no wrap), matching vim.
            crate::motions::move_left(&mut ed.buffer, count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::Right => {
            // `l` — operator-motion context (`dl`/`cl`/`yl`) is allowed
            // one past the last char so the range includes it; cursor
            // context clamps at the last char.
            if as_operator {
                crate::motions::move_right_to_end(&mut ed.buffer, count);
            } else {
                crate::motions::move_right_in_line(&mut ed.buffer, count);
            }
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::Up => {
            // Final col is set by `apply_sticky_col` below — push the
            // post-move row to the textarea and let sticky tracking
            // finish the work.
            let folds = crate::buffer_impl::SnapshotFoldProvider::from_buffer(&ed.buffer);
            crate::motions::move_up(&mut ed.buffer, &folds, count, &mut ed.sticky_col);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::Down => {
            let folds = crate::buffer_impl::SnapshotFoldProvider::from_buffer(&ed.buffer);
            crate::motions::move_down(&mut ed.buffer, &folds, count, &mut ed.sticky_col);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::ScreenUp => {
            let v = *ed.host.viewport();
            let folds = crate::buffer_impl::SnapshotFoldProvider::from_buffer(&ed.buffer);
            crate::motions::move_screen_up(&mut ed.buffer, &folds, &v, count, &mut ed.sticky_col);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::ScreenDown => {
            let v = *ed.host.viewport();
            let folds = crate::buffer_impl::SnapshotFoldProvider::from_buffer(&ed.buffer);
            crate::motions::move_screen_down(&mut ed.buffer, &folds, &v, count, &mut ed.sticky_col);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::WordFwd => {
            crate::motions::move_word_fwd(&mut ed.buffer, false, count, &ed.settings.iskeyword);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::WordBack => {
            crate::motions::move_word_back(&mut ed.buffer, false, count, &ed.settings.iskeyword);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::WordEnd => {
            crate::motions::move_word_end(&mut ed.buffer, false, count, &ed.settings.iskeyword);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::BigWordFwd => {
            crate::motions::move_word_fwd(&mut ed.buffer, true, count, &ed.settings.iskeyword);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::BigWordBack => {
            crate::motions::move_word_back(&mut ed.buffer, true, count, &ed.settings.iskeyword);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::BigWordEnd => {
            crate::motions::move_word_end(&mut ed.buffer, true, count, &ed.settings.iskeyword);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::WordEndBack => {
            crate::motions::move_word_end_back(
                &mut ed.buffer,
                false,
                count,
                &ed.settings.iskeyword,
            );
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::BigWordEndBack => {
            crate::motions::move_word_end_back(&mut ed.buffer, true, count, &ed.settings.iskeyword);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::LineStart => {
            crate::motions::move_line_start(&mut ed.buffer);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::FirstNonBlank => {
            crate::motions::move_first_non_blank(&mut ed.buffer);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::LineEnd => {
            // Vim normal-mode `$` lands on the last char, not one past it.
            crate::motions::move_line_end(&mut ed.buffer);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::FileTop => {
            // `count gg` jumps to line `count` (first non-blank);
            // bare `gg` lands at the top.
            if count > 1 {
                crate::motions::move_bottom(&mut ed.buffer, count);
            } else {
                crate::motions::move_top(&mut ed.buffer);
            }
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::FileBottom => {
            // `count G` jumps to line `count`; bare `G` lands at
            // the buffer bottom (`Buffer::move_bottom(0)`).
            if count > 1 {
                crate::motions::move_bottom(&mut ed.buffer, count);
            } else {
                crate::motions::move_bottom(&mut ed.buffer, 0);
            }
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::Find { ch, forward, till } => {
            for _ in 0..count {
                if !find_char_on_line(ed, *ch, *forward, *till) {
                    break;
                }
            }
        }
        Motion::FindRepeat { .. } => {} // already resolved upstream
        Motion::MatchBracket => {
            let _ = matching_bracket(ed);
        }
        Motion::UnmatchedBracket { forward, open } => {
            goto_unmatched_bracket(ed, *forward, *open, count);
        }
        Motion::WordAtCursor {
            forward,
            whole_word,
        } => {
            word_at_cursor_search(ed, *forward, *whole_word, count);
        }
        Motion::SearchNext { reverse } => {
            // Re-push the last query so the buffer's search state is
            // correct even if the host happened to clear it (e.g. while
            // a Visual mode draw was in progress).
            if let Some(pattern) = ed.vim.last_search.clone() {
                ed.push_search_pattern(&pattern);
            }
            if ed.search_state().pattern.is_none() {
                return;
            }
            // `n` repeats the last search in its committed direction;
            // `N` inverts. So a `?` search makes `n` walk backward and
            // `N` walk forward.
            let forward = ed.vim.last_search_forward != *reverse;
            for _ in 0..count.max(1) {
                if forward {
                    ed.search_advance_forward(true);
                } else {
                    ed.search_advance_backward(true);
                }
            }
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::ViewportTop => {
            let v = *ed.host().viewport();
            crate::motions::move_viewport_top(&mut ed.buffer, &v, count.saturating_sub(1));
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::ViewportMiddle => {
            let v = *ed.host().viewport();
            crate::motions::move_viewport_middle(&mut ed.buffer, &v);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::ViewportBottom => {
            let v = *ed.host().viewport();
            crate::motions::move_viewport_bottom(&mut ed.buffer, &v, count.saturating_sub(1));
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::LastNonBlank => {
            crate::motions::move_last_non_blank(&mut ed.buffer);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::LineMiddle => {
            let row = ed.cursor().0;
            let line_chars = buf_line_chars(&ed.buffer, row);
            // Vim's `gM`: column = floor(chars / 2). Empty / single-char
            // lines stay at col 0.
            let target = line_chars / 2;
            ed.jump_cursor(row, target);
        }
        Motion::ParagraphPrev => {
            crate::motions::move_paragraph_prev(&mut ed.buffer, count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::ParagraphNext => {
            crate::motions::move_paragraph_next(&mut ed.buffer, count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::SentencePrev => {
            for _ in 0..count.max(1) {
                if let Some((row, col)) = sentence_boundary(ed, false) {
                    ed.jump_cursor(row, col);
                }
            }
        }
        Motion::SentenceNext => {
            for _ in 0..count.max(1) {
                if let Some((row, col)) = sentence_boundary(ed, true) {
                    ed.jump_cursor(row, col);
                }
            }
        }
        Motion::SectionBackward => {
            crate::motions::move_section_backward(&mut ed.buffer, count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::SectionForward => {
            crate::motions::move_section_forward(&mut ed.buffer, count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::SectionEndBackward => {
            crate::motions::move_section_end_backward(&mut ed.buffer, count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::SectionEndForward => {
            crate::motions::move_section_end_forward(&mut ed.buffer, count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::FirstNonBlankNextLine => {
            crate::motions::move_first_non_blank_next_line(&mut ed.buffer, count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::FirstNonBlankPrevLine => {
            crate::motions::move_first_non_blank_prev_line(&mut ed.buffer, count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::FirstNonBlankLine => {
            crate::motions::move_first_non_blank_line(&mut ed.buffer, count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::GotoColumn => {
            crate::motions::move_goto_column(&mut ed.buffer, count);
            ed.push_buffer_cursor_to_textarea();
        }
    }
}

fn move_first_non_whitespace<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    // Some call sites invoke this right after `dd` / `<<` / `>>` etc
    // mutates the textarea content, so the migration buffer hasn't
    // seen the new lines OR new cursor yet. Mirror the full content
    // across before delegating, then push the result back so the
    // textarea reflects the resolved column too.
    ed.sync_buffer_content_from_textarea();
    crate::motions::move_first_non_blank(&mut ed.buffer);
    ed.push_buffer_cursor_to_textarea();
}

fn find_char_on_line<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    ch: char,
    forward: bool,
    till: bool,
) -> bool {
    let moved = crate::motions::find_char_on_line(&mut ed.buffer, ch, forward, till);
    if moved {
        ed.push_buffer_cursor_to_textarea();
    }
    moved
}

fn matching_bracket<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) -> bool {
    let moved = crate::motions::match_bracket(&mut ed.buffer);
    if moved {
        ed.push_buffer_cursor_to_textarea();
    }
    moved
}

/// `[(` / `])` / `[{` / `]}` — move to the `count`-th previous (`forward =
/// false`) / next (`forward = true`) unmatched bracket of the kind given by
/// `open` (`(` or `{`). Balanced inner pairs are skipped via a depth counter.
fn goto_unmatched_bracket<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    forward: bool,
    open: char,
    count: usize,
) {
    let close = match open {
        '(' => ')',
        '{' => '}',
        _ => return,
    };
    let cursor = buf_cursor_pos(&ed.buffer);
    let rows = buf_row_count(&ed.buffer);
    let target = count.max(1);
    let mut found = 0usize;
    let mut depth = 0i32;

    if forward {
        let mut r = cursor.row;
        let mut from_col = cursor.col + 1;
        while r < rows {
            let line: Vec<char> = buf_line(&ed.buffer, r)
                .unwrap_or_default()
                .chars()
                .collect();
            let mut ci = from_col;
            while ci < line.len() {
                let ch = line[ci];
                if ch == open {
                    depth += 1;
                } else if ch == close {
                    if depth == 0 {
                        found += 1;
                        if found == target {
                            buf_set_cursor_rc(&mut ed.buffer, r, ci);
                            ed.push_buffer_cursor_to_textarea();
                            return;
                        }
                    } else {
                        depth -= 1;
                    }
                }
                ci += 1;
            }
            r += 1;
            from_col = 0;
        }
    } else {
        let mut r = cursor.row as isize;
        // First row scans from the column left of the cursor; earlier rows from
        // their last column (`isize::MAX` clamps to `len - 1`).
        let mut from_col = cursor.col as isize - 1;
        while r >= 0 {
            let line: Vec<char> = buf_line(&ed.buffer, r as usize)
                .unwrap_or_default()
                .chars()
                .collect();
            let mut ci = from_col.min(line.len() as isize - 1);
            while ci >= 0 {
                let ch = line[ci as usize];
                if ch == close {
                    depth += 1;
                } else if ch == open {
                    if depth == 0 {
                        found += 1;
                        if found == target {
                            buf_set_cursor_rc(&mut ed.buffer, r as usize, ci as usize);
                            ed.push_buffer_cursor_to_textarea();
                            return;
                        }
                    } else {
                        depth -= 1;
                    }
                }
                ci -= 1;
            }
            r -= 1;
            from_col = isize::MAX;
        }
    }
}

fn word_at_cursor_search<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    forward: bool,
    whole_word: bool,
    count: usize,
) {
    let (row, col) = ed.cursor();
    let line: String = buf_line(&ed.buffer, row).unwrap_or_default();
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return;
    }
    // Expand around cursor to a word boundary.
    let spec = ed.settings().iskeyword.clone();
    let is_word = |c: char| is_keyword_char(c, &spec);
    let mut start = col.min(chars.len().saturating_sub(1));
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = start;
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    if end <= start {
        return;
    }
    let word: String = chars[start..end].iter().collect();
    let escaped = regex_escape(&word);
    let pattern = if whole_word {
        format!(r"\b{escaped}\b")
    } else {
        escaped
    };
    ed.push_search_pattern(&pattern);
    if ed.search_state().pattern.is_none() {
        return;
    }
    // Remember the query so `n` / `N` keep working after the jump.
    ed.vim.last_search = Some(pattern);
    ed.vim.last_search_forward = forward;
    for _ in 0..count.max(1) {
        if forward {
            ed.search_advance_forward(true);
        } else {
            ed.search_advance_backward(true);
        }
    }
    ed.push_buffer_cursor_to_textarea();
}

fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ─── Operator application ──────────────────────────────────────────────────

/// Public(crate) entry: apply operator over the motion identified by a raw
/// char key. Called by `Editor::apply_op_motion` (the public controller API)
/// so the hjkl-vim pending-state reducer can dispatch `ApplyOpMotion` without
/// re-entering the FSM.
///
/// Applies standard vim quirks:
/// - `cw` / `cW` → `ce` / `cE`
/// - `FindRepeat` → resolves against `last_find`
/// - Updates `last_find` and `last_change` per existing conventions.
///
/// No-op when `motion_key` does not produce a known motion.
pub(crate) fn apply_op_motion_key<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    motion_key: char,
    total_count: usize,
) {
    let input = Input {
        key: Key::Char(motion_key),
        ctrl: false,
        alt: false,
        shift: false,
    };
    let Some(motion) = parse_motion(&input) else {
        return;
    };
    let motion = match motion {
        Motion::FindRepeat { reverse } => match ed.vim.last_find {
            Some((ch, forward, till)) => Motion::Find {
                ch,
                forward: if reverse { !forward } else { forward },
                till,
            },
            None => return,
        },
        // Vim quirk: `cw` / `cW` → `ce` / `cE`.
        Motion::WordFwd if op == Operator::Change => Motion::WordEnd,
        Motion::BigWordFwd if op == Operator::Change => Motion::BigWordEnd,
        m => m,
    };
    apply_op_with_motion(ed, op, &motion, total_count);
    if let Motion::Find { ch, forward, till } = &motion {
        ed.vim.last_find = Some((*ch, *forward, *till));
    }
    if !ed.vim.replaying && op_is_change(op) {
        ed.vim.last_change = Some(LastChange::OpMotion {
            op,
            motion,
            count: total_count,
            inserted: None,
        });
    }
}

/// Public(crate) entry: apply doubled-letter line op (`dd`/`yy`/`cc`/`>>`/`<<`/`gcc`).
/// Called by `Editor::apply_op_double` (the public controller API).
pub(crate) fn apply_op_double<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    total_count: usize,
) {
    if op == Operator::Comment {
        // `gcc` / `{N}gcc` — toggle comment on `total_count` lines starting at cursor.
        let row = buf_cursor_pos(&ed.buffer).row;
        let end_row = (row + total_count.max(1) - 1).min(ed.buffer.row_count().saturating_sub(1));
        ed.toggle_comment_range(row, end_row);
        ed.vim.mode = Mode::Normal;
        if !ed.vim.replaying {
            ed.vim.last_change = Some(LastChange::LineOp {
                op,
                count: total_count,
                inserted: None,
            });
        }
        return;
    }
    execute_line_op(ed, op, total_count);
    if !ed.vim.replaying {
        ed.vim.last_change = Some(LastChange::LineOp {
            op,
            count: total_count,
            inserted: None,
        });
    }
}

/// Compute the `gn` / `gN` target match as a `(start, end_inclusive)` pair.
/// When the cursor sits inside a match, that match is the target; otherwise the
/// next match (forward) or previous match (backward) is used. Returns `None`
/// when there is no pattern or no match remains.
fn gn_find_range<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    re: &regex::Regex,
    forward: bool,
) -> Option<(crate::types::Pos, crate::types::Pos)> {
    use crate::types::{Cursor, Pos, Search};
    let cursor = Cursor::cursor(&ed.buffer);
    let contains =
        Search::find_prev(&ed.buffer, cursor, re).filter(|m| m.start <= cursor && cursor < m.end);
    let range = if let Some(m) = contains {
        m
    } else if forward {
        Search::find_next(&ed.buffer, cursor, re)?
    } else {
        Search::find_prev(&ed.buffer, cursor, re)?
    };
    let end_incl = if range.end.col > 0 {
        Pos::new(range.end.line, range.end.col - 1)
    } else {
        range.end
    };
    Some((range.start, end_incl))
}

/// `gn` / `gN` — operate on (or select) the search match. `op = None` enters
/// Visual mode with the match selected; `Some(op)` applies the operator to the
/// match as a charwise inclusive range. Records `LastChange::GnOp` so `cgn` /
/// `dgn` are `.`-repeatable.
pub(crate) fn gn_operate<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Option<Operator>,
    forward: bool,
    count: usize,
) {
    use crate::types::{Cursor, Pos};
    // Make sure the compiled pattern reflects the last `/` or `*` search.
    if let Some(p) = ed.vim.last_search.clone() {
        ed.push_search_pattern(&p);
    }
    let Some(re) = ed.search_state().pattern.clone() else {
        return;
    };
    ed.sync_buffer_content_from_textarea();

    let Some(mut range) = gn_find_range(ed, &re, forward) else {
        return;
    };
    // `[count]gn` walks to the count-th match.
    for _ in 1..count.max(1) {
        let past = Pos::new(range.1.line, range.1.col + 1);
        Cursor::set_cursor(&mut ed.buffer, past);
        match gn_find_range(ed, &re, forward) {
            Some(r) => range = r,
            None => break,
        }
    }
    let start_t = (range.0.line as usize, range.0.col as usize);
    let end_t = (range.1.line as usize, range.1.col as usize);

    match op {
        None => {
            // Bare `gn` — select the match in Visual mode.
            ed.vim.visual_anchor = start_t;
            buf_set_cursor_rc(&mut ed.buffer, end_t.0, end_t.1);
            ed.vim.mode = Mode::Visual;
            ed.vim.current_mode = crate::VimMode::Visual;
            ed.push_buffer_cursor_to_textarea();
        }
        Some(Operator::Delete) => {
            ed.push_undo();
            cut_vim_range(ed, start_t, end_t, RangeKind::Inclusive);
            // Deleting at the line end can leave the cursor one past the last
            // char; vim clamps it back onto the line.
            clamp_cursor_to_normal_mode(ed);
            ed.push_buffer_cursor_to_textarea();
            if !ed.vim.replaying {
                ed.vim.last_change = Some(LastChange::GnOp {
                    op: Operator::Delete,
                    forward,
                    inserted: None,
                });
            }
        }
        Some(Operator::Change) => {
            ed.push_undo();
            ed.vim.change_mark_start = Some(start_t);
            cut_vim_range(ed, start_t, end_t, RangeKind::Inclusive);
            if !ed.vim.replaying {
                ed.vim.last_change = Some(LastChange::GnOp {
                    op: Operator::Change,
                    forward,
                    inserted: None,
                });
            }
            begin_insert_noundo(ed, 1, InsertReason::AfterChange);
        }
        Some(Operator::Yank) => {
            let text = read_vim_range(ed, start_t, end_t, RangeKind::Inclusive);
            if !text.is_empty() {
                ed.record_yank_to_host(text.clone());
                ed.record_yank(text, false);
            }
            buf_set_cursor_rc(&mut ed.buffer, start_t.0, start_t.1);
            ed.push_buffer_cursor_to_textarea();
        }
        Some(other @ (Operator::Uppercase | Operator::Lowercase | Operator::ToggleCase)) => {
            // Case op over a gn match: apply as a charwise op over the
            // inclusive range.
            ed.push_undo();
            apply_case_op_to_selection(ed, other, start_t, end_t, RangeKind::Inclusive);
        }
        Some(_) => {}
    }
}

/// Shared implementation: apply operator over a g-chord motion or case-op
/// linewise form. Called by `Editor::apply_op_g` (the public controller API)
/// so the hjkl-vim reducer can dispatch `ApplyOpG` without re-entering the FSM.
///
/// - If `op` is Uppercase/Lowercase/ToggleCase and `ch` matches the op's char
///   (`U`/`u`/`~`): executes the line op and updates `last_change`.
/// - `n` / `N` operate on the search match (`dgn` / `cgn`).
/// - Otherwise, maps `ch` to a motion (`g`→FileTop, `e`→WordEndBack,
///   `E`→BigWordEndBack, `j`→ScreenDown, `k`→ScreenUp) and applies. Unknown
///   chars are silently ignored (no-op), matching the engine FSM's behaviour.
pub(crate) fn apply_op_g_inner<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    ch: char,
    total_count: usize,
) {
    // Case-op linewise form: `gUgU`, `gugu`, `g~g~`, `g?g?` — same effect as
    // `gUU` / `guu` / `g~~` / `g??`.
    if matches!(
        op,
        Operator::Uppercase | Operator::Lowercase | Operator::ToggleCase | Operator::Rot13
    ) {
        let op_char = match op {
            Operator::Uppercase => 'U',
            Operator::Lowercase => 'u',
            Operator::ToggleCase => '~',
            Operator::Rot13 => '?',
            _ => unreachable!(),
        };
        if ch == op_char {
            execute_line_op(ed, op, total_count);
            if !ed.vim.replaying {
                ed.vim.last_change = Some(LastChange::LineOp {
                    op,
                    count: total_count,
                    inserted: None,
                });
            }
            return;
        }
    }
    // `dgn` / `cgn` / `ygn` (and `gN` forms) — operate on the search match.
    if ch == 'n' || ch == 'N' {
        gn_operate(ed, Some(op), ch == 'n', total_count);
        return;
    }
    let motion = match ch {
        'g' => Motion::FileTop,
        'e' => Motion::WordEndBack,
        'E' => Motion::BigWordEndBack,
        'j' => Motion::ScreenDown,
        'k' => Motion::ScreenUp,
        _ => return, // Unknown char — no-op.
    };
    apply_op_with_motion(ed, op, &motion, total_count);
    if !ed.vim.replaying && op_is_change(op) {
        ed.vim.last_change = Some(LastChange::OpMotion {
            op,
            motion,
            count: total_count,
            inserted: None,
        });
    }
}

/// Public(crate) entry point for bare `g<x>`. Applies the g-chord effect
/// given the char `ch` and pre-captured `count`. Called by `Editor::after_g`
/// (the public controller API) so the hjkl-vim pending-state reducer can
/// dispatch `AfterGChord` without re-entering the FSM.
pub(crate) fn apply_after_g<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    ch: char,
    count: usize,
) {
    match ch {
        'g' => {
            // gg — top / jump to line count.
            let pre = ed.cursor();
            if count > 1 {
                ed.jump_cursor(count - 1, 0);
            } else {
                ed.jump_cursor(0, 0);
            }
            move_first_non_whitespace(ed);
            // Update sticky_col to the first-non-blank column so j/k after
            // gg aim for the correct column per vim semantics.
            ed.sticky_col = Some(ed.cursor().1);
            if ed.cursor() != pre {
                ed.push_jump(pre);
            }
        }
        'e' => execute_motion(ed, Motion::WordEndBack, count),
        'E' => execute_motion(ed, Motion::BigWordEndBack, count),
        // `g_` — last non-blank on the line.
        '_' => execute_motion(ed, Motion::LastNonBlank, count),
        // `gM` — middle char column of the current line.
        'M' => execute_motion(ed, Motion::LineMiddle, count),
        // `gv` — re-enter the last visual selection.
        // Phase 6.6a: drive through the public Editor API.
        'v' => ed.reenter_last_visual(),
        // `gj` / `gk` — display-line down / up. Walks one screen
        // segment at a time under `:set wrap`; falls back to `j`/`k`
        // when wrap is off (Buffer::move_screen_* handles the branch).
        'j' => execute_motion(ed, Motion::ScreenDown, count),
        'k' => execute_motion(ed, Motion::ScreenUp, count),
        // Case operators: `gU` / `gu` / `g~`. Enter operator-pending
        // so the next input is treated as the motion / text object /
        // shorthand double (`gUU`, `guu`, `g~~`).
        'U' => {
            ed.vim.pending = Pending::Op {
                op: Operator::Uppercase,
                count1: count,
            };
        }
        'u' => {
            ed.vim.pending = Pending::Op {
                op: Operator::Lowercase,
                count1: count,
            };
        }
        '~' => {
            ed.vim.pending = Pending::Op {
                op: Operator::ToggleCase,
                count1: count,
            };
        }
        '?' => {
            // `g?{motion}` — ROT13 operator (`g??` / `g?g?` doubled).
            ed.vim.pending = Pending::Op {
                op: Operator::Rot13,
                count1: count,
            };
        }
        'q' => {
            // `gq{motion}` — text reflow operator. Subsequent motion
            // / textobj rides the same operator pipeline.
            ed.vim.pending = Pending::Op {
                op: Operator::Reflow,
                count1: count,
            };
        }
        'w' => {
            // `gw{motion}` — same reflow as `gq` but cursor stays at
            // its pre-reflow position (clamped to new EOL if shorter).
            ed.vim.pending = Pending::Op {
                op: Operator::ReflowKeepCursor,
                count1: count,
            };
        }
        'J' => {
            // `gJ` — join line below without inserting a space. `[count]gJ`
            // joins `count` lines (`count - 1` joins), like `J`.
            let joins = count.max(2) - 1;
            for _ in 0..joins {
                ed.push_undo();
                join_line_raw(ed);
            }
            if !ed.vim.replaying {
                ed.vim.last_change = Some(LastChange::JoinLine { count: joins });
            }
        }
        'd' => {
            // `gd` — goto definition. hjkl-engine doesn't run an LSP
            // itself; raise an intent the host drains and routes to
            // `sqls`. The cursor stays put here — the host moves it
            // once it has the target location.
            ed.pending_lsp = Some(crate::editor::LspIntent::GotoDefinition);
        }
        // `gi` — go to last-insert position and re-enter insert mode.
        // Matches vim's `:h gi`: moves to the `'^` mark position (the
        // cursor where insert mode was last active, before Esc step-back)
        // and enters insert mode there.
        'i' => {
            if let Some((row, col)) = ed.vim.last_insert_pos {
                ed.jump_cursor(row, col);
            }
            begin_insert(ed, count.max(1), InsertReason::Enter(InsertEntry::I));
        }
        // `gc` — enter operator-pending for the comment-toggle operator.
        // `gcc` (doubled 'c') is the line-wise form; `gc{motion}` is the
        // motion form. The operator is Comment — the app layer (or the
        // doubled-char path in handle_after_op) calls toggle_comment_range.
        'c' => {
            ed.vim.pending = Pending::Op {
                op: Operator::Comment,
                count1: count,
            };
        }
        // `gp` / `gP` — paste like `p`/`P` but leave the cursor just after
        // the pasted text.
        'p' => paste_bridge(ed, false, count.max(1), true, false),
        'P' => paste_bridge(ed, true, count.max(1), true, false),
        // `gn` / `gN` — select the next / previous search match in Visual mode.
        'n' => gn_operate(ed, None, true, count.max(1)),
        'N' => gn_operate(ed, None, false, count.max(1)),
        // `g;` / `g,` — walk the change list. `g;` toward older
        // entries, `g,` toward newer.
        ';' => walk_change_list(ed, -1, count.max(1)),
        ',' => walk_change_list(ed, 1, count.max(1)),
        // `g*` / `g#` — like `*` / `#` but match substrings (no `\b`
        // boundary anchors), so the cursor on `foo` finds it inside
        // `foobar` too.
        '*' => execute_motion(
            ed,
            Motion::WordAtCursor {
                forward: true,
                whole_word: false,
            },
            count,
        ),
        '#' => execute_motion(
            ed,
            Motion::WordAtCursor {
                forward: false,
                whole_word: false,
            },
            count,
        ),
        // `g&` — repeat last `:s` over the whole buffer (1,$), keeping all
        // original flags. Equivalent to `:%s//~/&` in vim.
        '&' => {
            let cmd = match ed.vim.last_substitute.clone() {
                Some(c) => c,
                None => {
                    // No prior substitute — mirror the `:&` error path; do
                    // nothing to the buffer (the host's status line will show
                    // the pending error if wired; for headless / test hosts
                    // we simply return silently).
                    return;
                }
            };
            let last_row = buf_row_count(&ed.buffer).saturating_sub(1) as u32;
            let r = 0u32..=last_row;
            // apply_substitute moves cursor to last changed line and pushes
            // one undo snapshot — same semantics as `:&&` / `:%s//~/&`.
            let _ = crate::substitute::apply_substitute(ed, &cmd, r);
            // Update stored substitute so subsequent `g&` sees the same cmd.
            // (apply_substitute doesn't call set_last_substitute itself.)
            ed.vim.last_substitute = Some(cmd);
        }
        _ => {}
    }
}

/// Normal-mode `&` — repeat the last `:s` on the current line, dropping the
/// previous flags (vim: `&` ≡ `:s` with no flags). `g&` keeps flags + whole
/// buffer; this is the single-line, flag-less form.
pub(crate) fn ampersand_repeat<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    let Some(mut cmd) = ed.vim.last_substitute.clone() else {
        return;
    };
    cmd.flags = crate::substitute::SubstFlags::default();
    let row = buf_cursor_pos(&ed.buffer).row as u32;
    let _ = crate::substitute::apply_substitute(ed, &cmd, row..=row);
}

/// Public(crate) entry point for bare `z<x>`. Applies the z-chord effect
/// given the char `ch` and pre-captured `count`. Called by `Editor::after_z`
/// (the public controller API) so the hjkl-vim pending-state reducer can
/// dispatch `AfterZChord` without re-entering the engine FSM.
pub(crate) fn apply_after_z<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    ch: char,
    count: usize,
) {
    use crate::editor::CursorScrollTarget;
    let row = ed.cursor().0;
    match ch {
        'z' => {
            ed.scroll_cursor_to(CursorScrollTarget::Center);
            ed.vim.viewport_pinned = true;
        }
        't' => {
            ed.scroll_cursor_to(CursorScrollTarget::Top);
            ed.vim.viewport_pinned = true;
        }
        'b' => {
            ed.scroll_cursor_to(CursorScrollTarget::Bottom);
            ed.vim.viewport_pinned = true;
        }
        // Folds — operate on the fold under the cursor (or the
        // whole buffer for `R` / `M`). Routed through
        // [`Editor::apply_fold_op`] (0.0.38 Patch C-δ.4) so the host
        // can observe / veto each op via [`Editor::take_fold_ops`].
        'o' => {
            ed.apply_fold_op(crate::types::FoldOp::OpenAt(row));
        }
        'c' => {
            ed.apply_fold_op(crate::types::FoldOp::CloseAt(row));
        }
        'a' => {
            ed.apply_fold_op(crate::types::FoldOp::ToggleAt(row));
        }
        'R' => {
            ed.apply_fold_op(crate::types::FoldOp::OpenAll);
        }
        'M' => {
            ed.apply_fold_op(crate::types::FoldOp::CloseAll);
        }
        'E' => {
            ed.apply_fold_op(crate::types::FoldOp::ClearAll);
        }
        'd' => {
            ed.apply_fold_op(crate::types::FoldOp::RemoveAt(row));
        }
        'f' => {
            if matches!(
                ed.vim.mode,
                Mode::Visual | Mode::VisualLine | Mode::VisualBlock
            ) {
                // `zf` over a Visual selection creates a fold spanning
                // anchor → cursor.
                let anchor_row = match ed.vim.mode {
                    Mode::VisualLine => ed.vim.visual_line_anchor,
                    Mode::VisualBlock => ed.vim.block_anchor.0,
                    _ => ed.vim.visual_anchor.0,
                };
                let cur = ed.cursor().0;
                let top = anchor_row.min(cur);
                let bot = anchor_row.max(cur);
                ed.apply_fold_op(crate::types::FoldOp::Add {
                    start_row: top,
                    end_row: bot,
                    closed: true,
                });
                ed.vim.mode = Mode::Normal;
            } else {
                // `zf{motion}` / `zf{textobj}` — route through the
                // operator pipeline. `Operator::Fold` reuses every
                // motion / text-object / `g`-prefix branch the other
                // operators get.
                ed.vim.pending = Pending::Op {
                    op: Operator::Fold,
                    count1: count,
                };
            }
        }
        _ => {}
    }
}

/// Public(crate) entry point for bare `f<x>` / `F<x>` / `t<x>` / `T<x>`.
/// Applies the motion and records `last_find` for `;` / `,` repeat.
/// Called by `Editor::find_char` (the public controller API) so the
/// hjkl-vim pending-state reducer can dispatch `FindChar` without
/// re-entering the FSM.
pub(crate) fn apply_find_char<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    ch: char,
    forward: bool,
    till: bool,
    count: usize,
) {
    execute_motion(ed, Motion::Find { ch, forward, till }, count.max(1));
    ed.vim.last_find = Some((ch, forward, till));
    ed.vim.last_horizontal_motion = LastHorizontalMotion::FindChar;
}

// ─── Sneak motion ──────────────────────────────────────────────────────────

/// Scan the buffer from the current cursor position for the `count`-th
/// occurrence of the two-char digraph `(c1, c2)`.
///
/// - `forward=true` → scan downward (rows) and rightward (cols) past cursor.
/// - `forward=false` → scan upward and leftward.
///
/// When a match is found the cursor jumps to the first char of the digraph.
/// `last_sneak` and `last_horizontal_motion` are updated so `;`/`,` repeat.
/// No-op (cursor unchanged) when no match exists.
pub(crate) fn apply_sneak<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    c1: char,
    c2: char,
    forward: bool,
    count: usize,
) {
    let count = count.max(1);
    let (start_row, start_col) = ed.cursor();
    let row_count = buf_row_count(&ed.buffer);

    let result = if forward {
        sneak_scan_forward(ed, start_row, start_col, c1, c2, count)
    } else {
        sneak_scan_backward(ed, start_row, start_col, c1, c2, count)
    };

    if let Some((row, col)) = result {
        buf_set_cursor_rc(&mut ed.buffer, row, col);
        ed.push_buffer_cursor_to_textarea();
        let _ = row_count; // suppress unused-variable warning
    }

    ed.vim.last_sneak = Some(((c1, c2), forward));
    ed.vim.last_horizontal_motion = LastHorizontalMotion::Sneak;
}

/// Scan forward from `(start_row, start_col)` (exclusive — start right after
/// cursor) for the `count`-th occurrence of `c1+c2`.
fn sneak_scan_forward<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    start_row: usize,
    start_col: usize,
    c1: char,
    c2: char,
    count: usize,
) -> Option<(usize, usize)> {
    let row_count = buf_row_count(&ed.buffer);
    let mut hits = 0usize;
    for row in start_row..row_count {
        let line = buf_line(&ed.buffer, row).unwrap_or_default();
        let chars: Vec<char> = line.chars().collect();
        // On the start row begin scanning one past the current column.
        let col_start = if row == start_row { start_col + 1 } else { 0 };
        if col_start + 1 > chars.len() {
            continue;
        }
        for col in col_start..chars.len().saturating_sub(1) {
            if chars[col] == c1 && chars[col + 1] == c2 {
                hits += 1;
                if hits == count {
                    return Some((row, col));
                }
            }
        }
    }
    None
}

/// Scan backward from `(start_row, start_col)` (exclusive — start left of
/// cursor) for the `count`-th occurrence of `c1+c2`.
fn sneak_scan_backward<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    start_row: usize,
    start_col: usize,
    c1: char,
    c2: char,
    count: usize,
) -> Option<(usize, usize)> {
    let row_count = buf_row_count(&ed.buffer);
    let mut hits = 0usize;
    // Iterate rows from start_row down to 0.
    let rows_to_scan = (0..row_count).rev().skip(row_count - start_row - 1);
    for row in rows_to_scan {
        let line = buf_line(&ed.buffer, row).unwrap_or_default();
        let chars: Vec<char> = line.chars().collect();
        // On the start row end scanning one before the current column.
        let col_end = if row == start_row {
            start_col.saturating_sub(1)
        } else if chars.is_empty() {
            continue;
        } else {
            chars.len().saturating_sub(1)
        };
        if col_end == 0 {
            continue;
        }
        // Scan cols right-to-left from col_end-1 so we match c1 at col, c2 at col+1.
        for col in (0..col_end).rev() {
            if col + 1 < chars.len() && chars[col] == c1 && chars[col + 1] == c2 {
                hits += 1;
                if hits == count {
                    return Some((row, col));
                }
            }
        }
    }
    None
}

/// Apply `op` over the sneak digraph range. Charwise exclusive from cursor up
/// to (but not including) the first char of the first match. This matches
/// vim-sneak's default `<Plug>Sneak_s` operator-pending behavior.
///
/// Example: buffer `"foo ab bar\n"`, cursor col 0, `dsab` → deletes `"foo "`
/// leaving `"ab bar\n"`.
pub(crate) fn apply_op_sneak<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    c1: char,
    c2: char,
    forward: bool,
    total_count: usize,
) {
    let start = ed.cursor();
    let result = if forward {
        sneak_scan_forward(ed, start.0, start.1, c1, c2, total_count)
    } else {
        sneak_scan_backward(ed, start.0, start.1, c1, c2, total_count)
    };
    let Some(end) = result else {
        return;
    };
    // Charwise exclusive — land the virtual cursor at end, then use
    // Exclusive range kind (end position not included).
    ed.jump_cursor(end.0, end.1);
    let end_cur = ed.cursor();
    ed.jump_cursor(start.0, start.1);
    run_operator_over_range(ed, op, start, end_cur, RangeKind::Exclusive);
    ed.vim.last_sneak = Some(((c1, c2), forward));
    ed.vim.last_horizontal_motion = LastHorizontalMotion::Sneak;
    if !ed.vim.replaying && op_is_change(op) {
        // No dot-repeat motion variant for sneak ops (plugin behavior,
        // not vim-core); record as a Change/Delete line op as a
        // best-effort fallback so `.` at least does something.
    }
}

/// Public(crate) entry: apply operator over a find motion (`df<x>` etc.).
/// Called by `Editor::apply_op_find` (the public controller API) so the
/// hjkl-vim `PendingState::OpFind` reducer can dispatch `ApplyOpFind` without
/// re-entering the FSM. `handle_op_find_target` now delegates here to avoid
/// logic duplication.
pub(crate) fn apply_op_find_motion<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    ch: char,
    forward: bool,
    till: bool,
    total_count: usize,
) {
    let motion = Motion::Find { ch, forward, till };
    apply_op_with_motion(ed, op, &motion, total_count);
    ed.vim.last_find = Some((ch, forward, till));
    if !ed.vim.replaying && op_is_change(op) {
        ed.vim.last_change = Some(LastChange::OpMotion {
            op,
            motion,
            count: total_count,
            inserted: None,
        });
    }
}

/// Shared implementation: map `ch` to `TextObject`, apply the operator, and
/// record `last_change`. Returns `false` when `ch` is not a known text-object
/// kind (caller should treat as a no-op). Called by `Editor::apply_op_text_obj`
/// (the public controller API) so hjkl-vim can dispatch without re-entering the FSM.
///
/// `_total_count` is accepted for API symmetry with `apply_op_find_motion` /
/// `apply_op_motion_key` but is currently unused — text objects don't repeat
/// in vim's current grammar. Kept for future-proofing.
pub(crate) fn apply_op_text_obj_inner<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    ch: char,
    inner: bool,
    total_count: usize,
) -> bool {
    // `total_count` drives bracket text objects: `2di{` targets the Nth
    // enclosing pair. Non-bracket objects ignore it (vim does too).
    let obj = match ch {
        'w' => TextObject::Word { big: false },
        'W' => TextObject::Word { big: true },
        '"' | '\'' | '`' => TextObject::Quote(ch),
        '(' | ')' | 'b' => TextObject::Bracket('('),
        '[' | ']' => TextObject::Bracket('['),
        '{' | '}' | 'B' => TextObject::Bracket('{'),
        '<' | '>' => TextObject::Bracket('<'),
        'p' => TextObject::Paragraph,
        't' => TextObject::XmlTag,
        's' => TextObject::Sentence,
        _ => return false,
    };
    apply_op_with_text_object(ed, op, obj, inner, total_count.max(1));
    if !ed.vim.replaying && op_is_change(op) {
        ed.vim.last_change = Some(LastChange::OpTextObj {
            op,
            obj,
            inner,
            inserted: None,
        });
    }
    true
}

/// Move `pos` back by one character, clamped to (0, 0).
pub(crate) fn retreat_one<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    pos: (usize, usize),
) -> (usize, usize) {
    let (r, c) = pos;
    if c > 0 {
        (r, c - 1)
    } else if r > 0 {
        let prev_len = buf_line_bytes(&ed.buffer, r - 1);
        (r - 1, prev_len)
    } else {
        (0, 0)
    }
}

/// Variant of begin_insert that doesn't push_undo (caller already did).
fn begin_insert_noundo<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
    reason: InsertReason,
) {
    let reason = if ed.vim.replaying {
        InsertReason::ReplayOnly
    } else {
        reason
    };
    let (row, col) = ed.cursor();
    ed.vim.insert_session = Some(InsertSession {
        count,
        row_min: row,
        row_max: row,
        before_rope: crate::types::Query::rope(&ed.buffer),
        reason,
        start_row: row,
        start_col: col,
    });
    ed.vim.mode = Mode::Insert;
    // Phase 6.3: keep current_mode in sync for callers that bypass step().
    ed.vim.current_mode = crate::VimMode::Insert;
    drop_blame_if_left_normal(ed);
}

// ─── Operator × Motion application ─────────────────────────────────────────

pub(crate) fn apply_op_with_motion<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    motion: &Motion,
    count: usize,
) {
    let start = ed.cursor();
    // Tentatively apply motion to find the endpoint. Operator context
    // so `l` on the last char advances past-last (standard vim
    // exclusive-motion endpoint behaviour), enabling `dl` / `cl` /
    // `yl` to cover the final char.
    apply_motion_cursor_ctx(ed, motion, count, true);
    let end = ed.cursor();
    let kind = motion_kind(motion);
    // Restore cursor before selecting (so Yank leaves cursor at start).
    ed.jump_cursor(start.0, start.1);

    // Comment is always linewise regardless of motion kind — toggle rows.
    if op == Operator::Comment {
        let top = start.0.min(end.0);
        let bot = start.0.max(end.0);
        ed.toggle_comment_range(top, bot);
        ed.vim.mode = Mode::Normal;
        return;
    }

    run_operator_over_range(ed, op, start, end, kind);
}

fn apply_op_with_text_object<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    obj: TextObject,
    inner: bool,
    count: usize,
) {
    let Some((mut start, mut end, mut kind)) = text_object_range(ed, obj, inner, count) else {
        return;
    };
    // vim's exclusive-motion adjustment (`:h exclusive`), applied to the
    // OPERATOR form of an inner bracket object spanning multiple lines (the
    // visual form keeps the raw charwise region). When the exclusive end sits
    // in column 0, pull it back to the end of the previous line and make the
    // motion inclusive; if the start is at or before the first non-blank of its
    // line, promote to linewise. This is what makes `di{` on a contentful
    // multi-line block collapse to bare braces ("{\n}") and a clean block
    // delete its body linewise.
    if inner
        && matches!(obj, TextObject::Bracket(_))
        && kind == RangeKind::Exclusive
        && end.0 > start.0
        && end.1 == 0
    {
        let prev = end.0 - 1;
        let prev_len = buf_line_chars(&ed.buffer, prev);
        let fnb = buf_line(&ed.buffer, start.0)
            .unwrap_or_default()
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .count();
        if start.1 <= fnb {
            start = (start.0, 0);
            end = (prev, prev_len);
            kind = RangeKind::Linewise;
        } else {
            end = (prev, prev_len.saturating_sub(1));
            kind = RangeKind::Inclusive;
        }
    }
    ed.jump_cursor(start.0, start.1);
    run_operator_over_range(ed, op, start, end, kind);
}

fn motion_kind(motion: &Motion) -> RangeKind {
    match motion {
        Motion::Up | Motion::Down | Motion::ScreenUp | Motion::ScreenDown => RangeKind::Linewise,
        Motion::FileTop | Motion::FileBottom => RangeKind::Linewise,
        Motion::ViewportTop | Motion::ViewportMiddle | Motion::ViewportBottom => {
            RangeKind::Linewise
        }
        Motion::WordEnd | Motion::BigWordEnd | Motion::WordEndBack | Motion::BigWordEndBack => {
            RangeKind::Inclusive
        }
        Motion::Find { .. } => RangeKind::Inclusive,
        Motion::MatchBracket => RangeKind::Inclusive,
        // `[(` / `])` etc. are exclusive: `d])` deletes up to but not including
        // the bracket; `d[(` deletes back to but not past the open bracket.
        Motion::UnmatchedBracket { .. } => RangeKind::Exclusive,
        // `$` now lands on the last char — operator ranges include it.
        Motion::LineEnd => RangeKind::Inclusive,
        // Linewise motions: +/-/_ land on the first non-blank of a line.
        Motion::FirstNonBlankNextLine
        | Motion::FirstNonBlankPrevLine
        | Motion::FirstNonBlankLine => RangeKind::Linewise,
        // [[/]]/[][/][ are charwise exclusive (land on the brace, brace excluded from operator).
        Motion::SectionBackward
        | Motion::SectionForward
        | Motion::SectionEndBackward
        | Motion::SectionEndForward => RangeKind::Exclusive,
        _ => RangeKind::Exclusive,
    }
}

/// Linewise change of rows `[top_row, end_row]` (vim `cc`/`cj`/`Vc`/`cip`…).
///
/// Deletes the spanned lines, leaves one line carrying the first row's
/// leading whitespace (when `autoindent` is on), parks the cursor after
/// the indent, and enters insert mode. Records the full linewise payload
/// to the yank + delete registers and sets `change_mark_start` for the
/// `[`/`]` deferral. Calls `push_undo` internally — callers must NOT also
/// call it.
fn change_linewise_rows<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top_row: usize,
    end_row: usize,
) {
    use hjkl_buffer::{Edit, MotionKind as BufKind, Position};
    // Vim `:h '[`: stash change start for `]` deferral on insert-exit.
    ed.vim.change_mark_start = Some((top_row, 0));
    ed.push_undo();
    ed.sync_buffer_content_from_textarea();
    // Read the cut payload first so yank reflects every original line.
    let payload = read_vim_range(ed, (top_row, 0), (end_row, 0), RangeKind::Linewise);
    // Drop every row after the first (rows [top_row+1, end_row]).
    if end_row > top_row {
        ed.mutate_edit(Edit::DeleteRange {
            start: Position::new(top_row + 1, 0),
            end: Position::new(end_row, 0),
            kind: BufKind::Line,
        });
    }
    // Preserve the first row's leading whitespace when autoindent is on;
    // wipe the whole line content otherwise (cursor lands at col 0).
    let indent_chars = if ed.settings.autoindent {
        let line = hjkl_buffer::rope_line_str(&crate::types::Query::rope(&ed.buffer), top_row);
        line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
    } else {
        0
    };
    let line_chars = buf_line_chars(&ed.buffer, top_row);
    if line_chars > indent_chars {
        ed.mutate_edit(Edit::DeleteRange {
            start: Position::new(top_row, indent_chars),
            end: Position::new(top_row, line_chars),
            kind: BufKind::Char,
        });
    }
    if !payload.is_empty() {
        ed.record_yank_to_host(payload.clone());
        ed.record_delete(payload, true);
    }
    buf_set_cursor_rc(&mut ed.buffer, top_row, indent_chars);
    ed.push_buffer_cursor_to_textarea();
    begin_insert_noundo(ed, 1, InsertReason::AfterChange);
}

fn run_operator_over_range<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    start: (usize, usize),
    end: (usize, usize),
    kind: RangeKind,
) {
    let (top, bot) = order(start, end);
    // Charwise empty range (same position). For Delete/Yank there is nothing to
    // act on. For Change, vim still enters insert at that point — `ci(` on `()`
    // and `ci{` on a whitespace-only block both place the cursor inside and
    // start inserting without deleting anything.
    if top == bot && !matches!(kind, RangeKind::Linewise) {
        if op == Operator::Change {
            ed.vim.change_mark_start = Some(top);
            ed.push_undo();
            begin_insert_noundo(ed, 1, InsertReason::AfterChange);
        }
        return;
    }

    match op {
        Operator::Yank => {
            let text = read_vim_range(ed, top, bot, kind);
            if !text.is_empty() {
                ed.record_yank_to_host(text.clone());
                ed.record_yank(text, matches!(kind, RangeKind::Linewise));
            }
            // Vim `:h '[` / `:h ']`: after a yank `[` = first yanked char,
            // `]` = last yanked char. Mode-aware: linewise snaps to line
            // edges; charwise uses the actual inclusive endpoint.
            let rbr = match kind {
                RangeKind::Linewise => {
                    let last_col = buf_line_chars(&ed.buffer, bot.0).saturating_sub(1);
                    (bot.0, last_col)
                }
                RangeKind::Inclusive => (bot.0, bot.1),
                RangeKind::Exclusive => (bot.0, bot.1.saturating_sub(1)),
            };
            ed.set_mark('[', top);
            ed.set_mark(']', rbr);
            buf_set_cursor_rc(&mut ed.buffer, top.0, top.1);
            ed.push_buffer_cursor_to_textarea();
        }
        Operator::Delete => {
            ed.push_undo();
            cut_vim_range(ed, top, bot, kind);
            // After a charwise / inclusive delete the buffer cursor is
            // placed at `start` by the edit path. In Normal mode the
            // cursor max col is `line_len - 1`; clamp it here so e.g.
            // `d$` doesn't leave the cursor one past the new line end.
            if !matches!(kind, RangeKind::Linewise) {
                clamp_cursor_to_normal_mode(ed);
            }
            ed.vim.mode = Mode::Normal;
            // Vim `:h '[` / `:h ']`: after a delete both marks park at
            // the cursor position where the deletion collapsed (the join
            // point). Set after the cut and clamp so the position is final.
            let pos = ed.cursor();
            ed.set_mark('[', pos);
            ed.set_mark(']', pos);
        }
        Operator::Change => {
            // Vim `:h '[`: `[` is set to the start of the changed range
            // before the cut. `]` is deferred to insert-exit (AfterChange
            // path in finish_insert_session) where the cursor sits on the
            // last inserted char.
            if matches!(kind, RangeKind::Linewise) {
                // Linewise change (`cj`/`ck`/`cip`/`cap`/…): preserve the
                // first line's indent and leave exactly one row open for
                // insert. The helper handles push_undo + insert entry.
                change_linewise_rows(ed, top.0, bot.0);
            } else {
                // Charwise change: cut the range and enter insert.
                ed.vim.change_mark_start = Some(top);
                ed.push_undo();
                cut_vim_range(ed, top, bot, kind);
                begin_insert_noundo(ed, 1, InsertReason::AfterChange);
            }
        }
        Operator::Uppercase | Operator::Lowercase | Operator::ToggleCase | Operator::Rot13 => {
            apply_case_op_to_selection(ed, op, top, bot, kind);
        }
        Operator::Indent | Operator::Outdent => {
            // Indent / outdent are always linewise even when triggered
            // by a char-wise motion (e.g. `>w` indents the whole line).
            ed.push_undo();
            if op == Operator::Indent {
                indent_rows(ed, top.0, bot.0, 1);
            } else {
                outdent_rows(ed, top.0, bot.0, 1);
            }
            ed.vim.mode = Mode::Normal;
        }
        Operator::Fold => {
            // Always linewise — fold the spanned rows regardless of the
            // motion's natural kind. Cursor lands on `top.0` to mirror
            // the visual `zf` path.
            if bot.0 >= top.0 {
                ed.apply_fold_op(crate::types::FoldOp::Add {
                    start_row: top.0,
                    end_row: bot.0,
                    closed: true,
                });
            }
            buf_set_cursor_rc(&mut ed.buffer, top.0, top.1);
            ed.push_buffer_cursor_to_textarea();
            ed.vim.mode = Mode::Normal;
        }
        Operator::Reflow => {
            ed.push_undo();
            reflow_rows(ed, top.0, bot.0);
            ed.vim.mode = Mode::Normal;
        }
        Operator::ReflowKeepCursor => {
            // `gw{motion}` — reflow like `gq` but restore the cursor to the
            // character it was on before the reflow (vim's gw behaviour).
            let saved = ed.cursor();
            ed.push_undo();
            let (before, after) = reflow_rows_keep_cursor(ed, top.0, bot.0);
            let (new_row, new_col) = reflow_keep_cursor(top.0, saved.0, saved.1, &before, &after);
            buf_set_cursor_rc(&mut ed.buffer, new_row, new_col);
            ed.push_buffer_cursor_to_textarea();
            ed.sticky_col = Some(new_col);
            ed.vim.mode = Mode::Normal;
        }
        Operator::AutoIndent => {
            // Always linewise — like Indent/Outdent.
            ed.push_undo();
            auto_indent_rows(ed, top.0, bot.0);
            ed.vim.mode = Mode::Normal;
        }
        Operator::Filter => {
            // Filter is not dispatched through run_operator_over_range.
            // The app calls Editor::filter_range directly with a command string.
            // Reaching this arm means a caller invoked run_operator_over_range
            // with Operator::Filter by mistake — silently no-op.
        }
        Operator::Comment => {
            // Comment is dispatched through Editor::toggle_comment_range.
            // Reaching this arm is a caller mistake — silently no-op.
        }
    }
}

// ─── Phase 4a pub range-mutation bridges ───────────────────────────────────
//
// These are `pub(crate)` entry points called by the five new pub methods on
// `Editor` (`delete_range`, `yank_range`, `change_range`, `indent_range`,
// `case_range`). They set `pending_register` from the caller-supplied char
// before delegating to the existing internal helpers so register semantics
// (unnamed `"`, named `"a`–`"z`, delete ring) are honoured exactly as in the
// FSM path.
//
// Do NOT call `run_operator_over_range` for Indent/Outdent or the three case
// operators — those share the FSM path but have dedicated parameter shapes
// (signed count, Operator-as-CaseOp) that map more cleanly to their own
// helpers.

/// Delete the range `[start, end)` (interpretation determined by `kind`) and
/// stash the deleted text in `register`. `'"'` is the unnamed register.
pub(crate) fn delete_range_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    start: (usize, usize),
    end: (usize, usize),
    kind: RangeKind,
    register: char,
) {
    ed.vim.pending_register = Some(register);
    run_operator_over_range(ed, Operator::Delete, start, end, kind);
}

/// Yank (copy) the range `[start, end)` into `register` without mutating the
/// buffer. `'"'` is the unnamed register.
pub(crate) fn yank_range_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    start: (usize, usize),
    end: (usize, usize),
    kind: RangeKind,
    register: char,
) {
    ed.vim.pending_register = Some(register);
    run_operator_over_range(ed, Operator::Yank, start, end, kind);
}

/// Delete the range `[start, end)` and enter Insert mode (vim `c` operator).
/// The deleted text is stashed in `register`. Mode transitions to Insert on
/// return; the caller must not issue further normal-mode ops until the insert
/// session ends.
pub(crate) fn change_range_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    start: (usize, usize),
    end: (usize, usize),
    kind: RangeKind,
    register: char,
) {
    ed.vim.pending_register = Some(register);
    run_operator_over_range(ed, Operator::Change, start, end, kind);
}

/// Indent (`count > 0`) or outdent (`count < 0`) the row span `[start.0,
/// end.0]`. `shiftwidth` overrides the editor's `settings().shiftwidth` for
/// this call; pass `0` to use the editor setting. The column parts of `start`
/// / `end` are ignored — indent is always linewise.
pub(crate) fn indent_range_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    start: (usize, usize),
    end: (usize, usize),
    count: i32,
    shiftwidth: u32,
) {
    if count == 0 {
        return;
    }
    let (top_row, bot_row) = if start.0 <= end.0 {
        (start.0, end.0)
    } else {
        (end.0, start.0)
    };
    // Temporarily override shiftwidth when the caller provides one.
    let original_sw = ed.settings().shiftwidth;
    if shiftwidth > 0 {
        ed.settings_mut().shiftwidth = shiftwidth as usize;
    }
    ed.push_undo();
    let abs_count = count.unsigned_abs() as usize;
    if count > 0 {
        indent_rows(ed, top_row, bot_row, abs_count);
    } else {
        outdent_rows(ed, top_row, bot_row, abs_count);
    }
    if shiftwidth > 0 {
        ed.settings_mut().shiftwidth = original_sw;
    }
    ed.vim.mode = Mode::Normal;
}

/// Apply a case transformation (`Uppercase` / `Lowercase` / `ToggleCase`) to
/// the range `[start, end)`. Only the three case `Operator` variants are valid;
/// other variants are silently ignored (no-op).
pub(crate) fn case_range_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    start: (usize, usize),
    end: (usize, usize),
    kind: RangeKind,
    op: Operator,
) {
    match op {
        Operator::Uppercase | Operator::Lowercase | Operator::ToggleCase | Operator::Rot13 => {}
        _ => return,
    }
    let (top, bot) = order(start, end);
    apply_case_op_to_selection(ed, op, top, bot, kind);
}

// ─── Phase 4e pub block-shape range-mutation bridges ───────────────────────
//
// These are `pub(crate)` entry points called by the four new pub methods on
// `Editor` (`delete_block`, `yank_block`, `change_block`, `indent_block`).
// They set `pending_register` from the caller-supplied char then delegate to
// `apply_block_operator` (after temporarily installing the 4-corner block as
// the engine's virtual VisualBlock selection). The editor's VisualBlock state
// fields (`block_anchor`, `block_vcol`) are overwritten, the op fires, then
// the fields are restored to their pre-call values. This ensures the engine's
// register / undo / mode semantics are exercised without requiring the caller
// to already be in VisualBlock mode.
//
// `indent_block` is a separate helper — it does not use `apply_block_operator`
// because indent/outdent are always linewise for blocks (vim behaviour).

/// Delete a rectangular VisualBlock selection. `top_row`/`bot_row` are
/// inclusive line bounds; `left_col`/`right_col` are inclusive char-column
/// bounds. Short lines that don't reach `right_col` lose only the chars
/// that exist (ragged-edge, matching engine FSM). `register` is honoured;
/// `'"'` selects the unnamed register.
pub(crate) fn delete_block_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top_row: usize,
    bot_row: usize,
    left_col: usize,
    right_col: usize,
    register: char,
) {
    ed.vim.pending_register = Some(register);
    let saved_anchor = ed.vim.block_anchor;
    let saved_vcol = ed.vim.block_vcol;
    ed.vim.block_anchor = (top_row, left_col);
    ed.vim.block_vcol = right_col;
    // Compute clamped col before the mutable borrow for buf_set_cursor_rc.
    let clamped = right_col.min(buf_line_chars(&ed.buffer, bot_row).saturating_sub(1));
    // Place cursor at bot_row / right_col so block_bounds resolves correctly.
    buf_set_cursor_rc(&mut ed.buffer, bot_row, clamped);
    apply_block_operator(ed, Operator::Delete, 1);
    // Restore — block_anchor/vcol are only meaningful in VisualBlock mode;
    // after the op we're in Normal so restoring is a no-op for the user but
    // keeps state coherent if the caller inspects fields.
    ed.vim.block_anchor = saved_anchor;
    ed.vim.block_vcol = saved_vcol;
}

/// Yank a rectangular VisualBlock selection into `register`.
pub(crate) fn yank_block_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top_row: usize,
    bot_row: usize,
    left_col: usize,
    right_col: usize,
    register: char,
) {
    ed.vim.pending_register = Some(register);
    let saved_anchor = ed.vim.block_anchor;
    let saved_vcol = ed.vim.block_vcol;
    ed.vim.block_anchor = (top_row, left_col);
    ed.vim.block_vcol = right_col;
    let clamped = right_col.min(buf_line_chars(&ed.buffer, bot_row).saturating_sub(1));
    buf_set_cursor_rc(&mut ed.buffer, bot_row, clamped);
    apply_block_operator(ed, Operator::Yank, 1);
    ed.vim.block_anchor = saved_anchor;
    ed.vim.block_vcol = saved_vcol;
}

/// Delete a rectangular VisualBlock selection and enter Insert mode (`c`).
/// The deleted text is stashed in `register`. Mode is Insert on return.
pub(crate) fn change_block_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top_row: usize,
    bot_row: usize,
    left_col: usize,
    right_col: usize,
    register: char,
) {
    ed.vim.pending_register = Some(register);
    let saved_anchor = ed.vim.block_anchor;
    let saved_vcol = ed.vim.block_vcol;
    ed.vim.block_anchor = (top_row, left_col);
    ed.vim.block_vcol = right_col;
    let clamped = right_col.min(buf_line_chars(&ed.buffer, bot_row).saturating_sub(1));
    buf_set_cursor_rc(&mut ed.buffer, bot_row, clamped);
    apply_block_operator(ed, Operator::Change, 1);
    ed.vim.block_anchor = saved_anchor;
    ed.vim.block_vcol = saved_vcol;
}

/// Indent (`count > 0`) or outdent (`count < 0`) rows `top_row..=bot_row`.
/// Column bounds are ignored — vim's block indent is always linewise.
/// `count == 0` is a no-op.
pub(crate) fn indent_block_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top_row: usize,
    bot_row: usize,
    count: i32,
) {
    if count == 0 {
        return;
    }
    ed.push_undo();
    let abs = count.unsigned_abs() as usize;
    if count > 0 {
        indent_rows(ed, top_row, bot_row, abs);
    } else {
        outdent_rows(ed, top_row, bot_row, abs);
    }
    ed.vim.mode = Mode::Normal;
}

/// Auto-indent (v1 dumb shiftwidth) the row span `[start.0, end.0]`. Column
/// parts are ignored — auto-indent is always linewise. See
/// `auto_indent_rows` for the algorithm and its v1 limitations.
pub(crate) fn auto_indent_range_bridge<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    start: (usize, usize),
    end: (usize, usize),
) {
    let (top_row, bot_row) = if start.0 <= end.0 {
        (start.0, end.0)
    } else {
        (end.0, start.0)
    };
    ed.push_undo();
    auto_indent_rows(ed, top_row, bot_row);
    ed.vim.mode = Mode::Normal;
}

// ─── Phase 4b pub text-object resolution bridges ───────────────────────────
//
// These are `pub(crate)` entry points called by the four new pub methods on
// `Editor` (`text_object_inner_word`, `text_object_around_word`,
// `text_object_inner_big_word`, `text_object_around_big_word`). They delegate
// to `word_text_object` — the existing private resolver — without touching any
// operator, register, or mode state. Pure functions: only `&Editor` required.

/// Resolve the range of `iw` (inner word) at the current cursor position.
/// Returns `None` if no word exists at the cursor.
pub(crate) fn text_object_inner_word_bridge<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    word_text_object(ed, true, false)
}

/// Resolve the range of `aw` (around word) at the current cursor position.
/// Includes trailing whitespace (or leading whitespace if no trailing exists).
pub(crate) fn text_object_around_word_bridge<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    word_text_object(ed, false, false)
}

/// Resolve the range of `iW` (inner WORD) at the current cursor position.
/// A WORD is any run of non-whitespace characters (no punctuation splitting).
pub(crate) fn text_object_inner_big_word_bridge<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    word_text_object(ed, true, true)
}

/// Resolve the range of `aW` (around WORD) at the current cursor position.
/// Includes trailing whitespace (or leading whitespace if no trailing exists).
pub(crate) fn text_object_around_big_word_bridge<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    word_text_object(ed, false, true)
}

// ─── Phase 4c pub text-object resolution bridges (quote + bracket) ──────────
//
// `pub(crate)` entry points called by the four new pub methods on `Editor`
// (`text_object_inner_quote`, `text_object_around_quote`,
// `text_object_inner_bracket`, `text_object_around_bracket`). They delegate to
// `quote_text_object` / `bracket_text_object` — the existing private resolvers
// — without touching any operator, register, or mode state.
//
// `bracket_text_object` returns `Option<(Pos, Pos, RangeKind)>`; the bridges
// strip the `RangeKind` tag so callers see a uniform
// `Option<((usize,usize),(usize,usize))>` shape, consistent with 4b.

/// Resolve the range of `i<quote>` (inner quote) at the current cursor
/// position. `quote` is one of `'"'`, `'\''`, or `` '`' ``. Returns `None`
/// when the cursor's line contains fewer than two occurrences of `quote`.
pub(crate) fn text_object_inner_quote_bridge<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    quote: char,
) -> Option<((usize, usize), (usize, usize))> {
    quote_text_object(ed, quote, true)
}

/// Resolve the range of `a<quote>` (around quote) at the current cursor
/// position. Includes surrounding whitespace on one side per vim semantics.
pub(crate) fn text_object_around_quote_bridge<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    quote: char,
) -> Option<((usize, usize), (usize, usize))> {
    quote_text_object(ed, quote, false)
}

/// Resolve the range of `i<bracket>` (inner bracket pair). `open` must be
/// one of `'('`, `'{'`, `'['`, `'<'`; the corresponding close is derived
/// internally. Returns `None` when no enclosing pair is found. The returned
/// range excludes the bracket characters themselves. Multi-line bracket pairs
/// whose content spans more than one line are reported as a charwise range
/// covering the first content character through the last content character
/// (RangeKind metadata is stripped — callers receive start/end only).
pub(crate) fn text_object_inner_bracket_bridge<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    open: char,
) -> Option<((usize, usize), (usize, usize))> {
    bracket_text_object(ed, open, true, 1).map(|(s, e, _kind)| (s, e))
}

/// Resolve the range of `a<bracket>` (around bracket pair). Includes the
/// bracket characters themselves. `open` must be one of `'('`, `'{'`, `'['`,
/// `'<'`.
pub(crate) fn text_object_around_bracket_bridge<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    open: char,
) -> Option<((usize, usize), (usize, usize))> {
    bracket_text_object(ed, open, false, 1).map(|(s, e, _kind)| (s, e))
}

// ── Sentence bridges (is / as) ─────────────────────────────────────────────

/// Resolve the range of `is` (inner sentence) at the cursor. Excludes
/// trailing whitespace.
pub(crate) fn text_object_inner_sentence_bridge<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    sentence_text_object(ed, true)
}

/// Resolve the range of `as` (around sentence) at the cursor. Includes
/// trailing whitespace.
pub(crate) fn text_object_around_sentence_bridge<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    sentence_text_object(ed, false)
}

// ── Paragraph bridges (ip / ap) ────────────────────────────────────────────

/// Resolve the range of `ip` (inner paragraph) at the cursor. A paragraph
/// is a block of non-blank lines bounded by blank lines or buffer edges.
pub(crate) fn text_object_inner_paragraph_bridge<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    paragraph_text_object(ed, true)
}

/// Resolve the range of `ap` (around paragraph) at the cursor. Includes one
/// trailing blank line when present.
pub(crate) fn text_object_around_paragraph_bridge<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    paragraph_text_object(ed, false)
}

// ── Tag bridges (it / at) ──────────────────────────────────────────────────

/// Resolve the range of `it` (inner tag) at the cursor. Matches XML/HTML-style
/// `<tag>...</tag>` pairs; returns the range of inner content between the open
/// and close tags.
pub(crate) fn text_object_inner_tag_bridge<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    tag_text_object(ed, true)
}

/// Resolve the range of `at` (around tag) at the cursor. Includes the open
/// and close tag delimiters themselves.
pub(crate) fn text_object_around_tag_bridge<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    tag_text_object(ed, false)
}

// ─── Rope utility helpers ──────────────────────────────────────────────────

/// Return row `r` from a rope as an owned `String`, stripping the
/// trailing `\n` that ropey includes on non-final lines.
pub(crate) fn rope_line_to_str(rope: &ropey::Rope, r: usize) -> String {
    let s = rope.line(r).to_string();
    // ropey includes the newline; strip it so callers see bare content.
    if s.ends_with('\n') {
        s[..s.len() - 1].to_string()
    } else {
        s
    }
}

/// Join rows `lo..=hi` from a rope into a single `String` separated by
/// `\n`. Callers must ensure `lo <= hi < rope.len_lines()`.
pub(crate) fn rope_row_range_str(rope: &ropey::Rope, lo: usize, hi: usize) -> String {
    let n = rope.len_lines();
    let lo = lo.min(n.saturating_sub(1));
    let hi = hi.min(n.saturating_sub(1));
    if lo > hi {
        return String::new();
    }
    // Use byte-slice to grab the full range in one rope walk.
    let start_byte = rope.line_to_byte(lo);
    // End byte: start of line hi+1, minus the newline separator, or
    // len_bytes() when hi is the last line.
    let end_byte = if hi + 1 < n {
        // line_to_byte(hi+1) points at the \n-terminated start of
        // the next line; step back one byte to drop that trailing \n.
        rope.line_to_byte(hi + 1).saturating_sub(1)
    } else {
        rope.len_bytes()
    };
    rope.byte_slice(start_byte..end_byte).to_string()
}

/// Snapshot all rows from a rope as `Vec<String>` (no trailing `\n`).
/// Use only when the caller truly needs mutable per-row access; prefer
/// rope iterators otherwise.
pub(crate) fn rope_to_lines_vec(rope: &ropey::Rope) -> Vec<String> {
    let n = rope.len_lines();
    (0..n).map(|r| rope_line_to_str(rope, r)).collect()
}

/// Pure greedy word-wrap of a slice of lines to `width` chars.
/// Returns `(original_slice, wrapped_lines)`.
/// Blank lines are preserved as paragraph separators.
fn greedy_wrap(original: &[String], width: usize) -> Vec<String> {
    let mut wrapped: Vec<String> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let flush = |para: &mut Vec<String>, out: &mut Vec<String>, width: usize| {
        if para.is_empty() {
            return;
        }
        let words = para.join(" ");
        let mut current = String::new();
        for word in words.split_whitespace() {
            let extra = if current.is_empty() {
                word.chars().count()
            } else {
                current.chars().count() + 1 + word.chars().count()
            };
            if extra > width && !current.is_empty() {
                out.push(std::mem::take(&mut current));
                current.push_str(word);
            } else if current.is_empty() {
                current.push_str(word);
            } else {
                current.push(' ');
                current.push_str(word);
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
        para.clear();
    };
    for line in original {
        if line.trim().is_empty() {
            flush(&mut paragraph, &mut wrapped, width);
            wrapped.push(String::new());
        } else {
            paragraph.push(line.clone());
        }
    }
    flush(&mut paragraph, &mut wrapped, width);
    wrapped
}

/// Greedy word-wrap the rows in `[top, bot]` to `settings.textwidth`.
/// Splits on blank-line boundaries so paragraph structure is
/// preserved. Each paragraph's words are joined with single spaces
/// before re-wrapping. Cursor lands at `(top, 0)` after the call
/// (via `ed.restore`).
fn reflow_rows<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top: usize,
    bot: usize,
) {
    let width = ed.settings().textwidth.max(1);
    let mut lines: Vec<String> = rope_to_lines_vec(&crate::types::Query::rope(&ed.buffer));
    let bot = bot.min(lines.len().saturating_sub(1));
    if top > bot {
        return;
    }
    let original = lines[top..=bot].to_vec();
    let wrapped = greedy_wrap(&original, width);

    // vim leaves the cursor on the last NON-BLANK line of the reflowed range
    // (a trailing blank from `ap` etc. is not counted).
    let last_offset = wrapped
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .unwrap_or(0);
    let last_row = top + last_offset;

    // Splice back. push_undo above means `u` reverses.
    let after: Vec<String> = lines.split_off(bot + 1);
    lines.truncate(top);
    lines.extend(wrapped);
    lines.extend(after);
    ed.restore(lines, (last_row, 0));
    move_first_non_whitespace(ed);
    ed.mark_content_dirty();
}

/// Same reflow as `reflow_rows` but also returns the pre-reflow slice
/// and the wrapped lines so the caller can compute a character-preserving
/// cursor position via [`reflow_keep_cursor`].
fn reflow_rows_keep_cursor<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top: usize,
    bot: usize,
) -> (Vec<String>, Vec<String>) {
    let width = ed.settings().textwidth.max(1);
    let mut lines: Vec<String> = rope_to_lines_vec(&crate::types::Query::rope(&ed.buffer));
    let bot = bot.min(lines.len().saturating_sub(1));
    if top > bot {
        return (Vec::new(), Vec::new());
    }
    let original = lines[top..=bot].to_vec();
    let wrapped = greedy_wrap(&original, width);

    let after: Vec<String> = lines.split_off(bot + 1);
    lines.truncate(top);
    lines.extend(wrapped.clone());
    lines.extend(after);
    ed.restore(lines, (top, 0));
    ed.mark_content_dirty();
    (original, wrapped)
}

/// Compute the new `(row, col)` that preserves the character the cursor
/// was on after `reflow_rows` has been applied to `[top, bot]`.
///
/// Algorithm (mirrors nvim's `gw` behaviour):
/// 1. Count the char-index of `(cursor_row, cursor_col)` relative to the
///    start of line `top` in `before_lines` (the pre-reflow snapshot).
/// 2. Walk the `after_lines` (the wrapped output) to find the row/col
///    that has the same char index.
///
/// If the cursor was past the end of the reflowed content (e.g. beyond
/// the last char), we clamp to the last char of the last reflowed line.
fn reflow_keep_cursor(
    top: usize,
    cursor_row: usize,
    cursor_col: usize,
    before_lines: &[String],
    after_lines: &[String],
) -> (usize, usize) {
    // Char offset of cursor within the before_lines range.
    // Each line contributes its chars; lines are separated by a single
    // space in the collapsed paragraph — but since reflow joins everything
    // and re-wraps with spaces, counting by chars-per-line (plus the
    // conceptual space separator between lines) mirrors the join.
    //
    // The simpler approach (which nvim appears to use): the cursor offset
    // within the range is the sum of chars in lines before cursor_row
    // (each + 1 for the space/newline separator) plus cursor_col, then
    // find that position in the wrapped text.
    //
    // Actually, since reflow collapses whitespace (split_whitespace),
    // the simplest approach is to track the cursor's char in the ORIGINAL
    // concatenated text and find it in the reflowed text.

    // Build the original range text as it appears when joined for wrapping:
    // same as what reflow does internally — join with spaces.
    // But we want raw character index, so we accumulate char counts per line
    // (without the trailing newline).
    let relative_row = cursor_row.saturating_sub(top);
    let mut char_offset: usize = 0;
    for (i, line) in before_lines.iter().enumerate() {
        if i == relative_row {
            // Add clamped col within this line.
            let line_len = line.chars().count();
            char_offset += cursor_col.min(line_len);
            break;
        }
        // Each line contributes its chars plus a newline (or space boundary).
        char_offset += line.chars().count() + 1;
    }

    // Now find char_offset in after_lines.
    let mut remaining = char_offset;
    for (i, line) in after_lines.iter().enumerate() {
        let len = line.chars().count();
        if remaining <= len {
            // The col is clamped to line_len - 1 in Normal mode.
            let col = remaining.min(if len == 0 { 0 } else { len.saturating_sub(1) });
            return (top + i, col);
        }
        // Not on this line; subtract line len + 1 (newline separator).
        remaining = remaining.saturating_sub(len + 1);
    }

    // Cursor was beyond the end of the reflowed content — clamp to last line.
    let last = after_lines.len().saturating_sub(1);
    let last_len = after_lines
        .get(last)
        .map(|l| l.chars().count())
        .unwrap_or(0);
    let col = if last_len == 0 { 0 } else { last_len - 1 };
    (top + last, col)
}

/// Transform the range `[top, bot]` (vim `RangeKind`) in place with
/// the given case operator. Cursor lands on `top` afterward — vim
/// convention for `gU{motion}` / `gu{motion}` / `g~{motion}`.
/// Preserves the textarea yank buffer (vim's case operators don't
/// touch registers).
fn apply_case_op_to_selection<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    top: (usize, usize),
    bot: (usize, usize),
    kind: RangeKind,
) {
    use hjkl_buffer::Edit;
    ed.push_undo();
    let saved_yank = ed.yank().to_string();
    let saved_yank_linewise = ed.vim.yank_linewise;
    let selection = cut_vim_range(ed, top, bot, kind);
    let transformed = match op {
        Operator::Uppercase => selection.to_uppercase(),
        Operator::Lowercase => selection.to_lowercase(),
        Operator::ToggleCase => toggle_case_str(&selection),
        Operator::Rot13 => rot13_str(&selection),
        _ => unreachable!(),
    };
    if !transformed.is_empty() {
        let cursor = buf_cursor_pos(&ed.buffer);
        ed.mutate_edit(Edit::InsertStr {
            at: cursor,
            text: transformed,
        });
    }
    buf_set_cursor_rc(&mut ed.buffer, top.0, top.1);
    ed.push_buffer_cursor_to_textarea();
    ed.set_yank(saved_yank);
    ed.vim.yank_linewise = saved_yank_linewise;
    ed.vim.mode = Mode::Normal;
}

/// Prepend `count * shiftwidth` spaces to each row in `[top, bot]`.
/// Rows that are empty are skipped (vim leaves blank lines alone when
/// indenting). `shiftwidth` is read from `editor.settings()` so
/// `:set shiftwidth=N` takes effect on the next operation.
fn indent_rows<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top: usize,
    bot: usize,
    count: usize,
) {
    ed.sync_buffer_content_from_textarea();
    let width = ed.settings().shiftwidth * count.max(1);
    let pad: String = " ".repeat(width);
    let mut lines: Vec<String> = rope_to_lines_vec(&crate::types::Query::rope(&ed.buffer));
    let bot = bot.min(lines.len().saturating_sub(1));
    for line in lines.iter_mut().take(bot + 1).skip(top) {
        if !line.is_empty() {
            line.insert_str(0, &pad);
        }
    }
    // Restore cursor to first non-blank of the top row so the next
    // vertical motion aims sensibly — matches vim's `>>` convention.
    ed.restore(lines, (top, 0));
    move_first_non_whitespace(ed);
}

/// Remove up to `count * shiftwidth` leading spaces (or tabs) from
/// each row in `[top, bot]`. Rows with less leading whitespace have
/// all their indent stripped, not clipped to zero length.
fn outdent_rows<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top: usize,
    bot: usize,
    count: usize,
) {
    ed.sync_buffer_content_from_textarea();
    let width = ed.settings().shiftwidth * count.max(1);
    let mut lines: Vec<String> = rope_to_lines_vec(&crate::types::Query::rope(&ed.buffer));
    let bot = bot.min(lines.len().saturating_sub(1));
    for line in lines.iter_mut().take(bot + 1).skip(top) {
        let strip: usize = line
            .chars()
            .take(width)
            .take_while(|c| *c == ' ' || *c == '\t')
            .count();
        if strip > 0 {
            let byte_len: usize = line.chars().take(strip).map(|c| c.len_utf8()).sum();
            line.drain(..byte_len);
        }
    }
    ed.restore(lines, (top, 0));
    move_first_non_whitespace(ed);
}

/// Count the number of open/close bracket pairs on a single line for the
/// auto-indent depth scanner. Only bare bracket scanning — does NOT handle
/// string literals or comments (v1 limitation, documented on
/// `auto_indent_range_bridge`).
/// Net bracket count `(open - close)` for a single line, skipping
/// brackets inside `//` line comments, `"..."` string literals, and
/// `'X'` char literals.
///
/// String / char escapes (`\"`, `\'`, `\\`) are honored so the closing
/// quote isn't missed when the literal contains a backslash.
///
/// Limitations:
/// - Block comments `/* ... */` are NOT tracked across lines (a single
///   line `/* foo { bar } */` is correctly skipped only because the
///   `/*` and `*/` are on the same line and we'd see `{` after `/*`).
///   For v1 we leave this since block comments mid-code are rare.
/// - Raw string literals `r"..."` / `r#"..."#` are NOT special-cased.
/// - Lifetime annotations like `'a` look like an unterminated char
///   literal — handled by the heuristic that a char literal MUST close
///   within the line; if the closing `'` isn't found, treat the `'` as
///   a normal character (lifetime).
///
/// Pre-fix the scan was naive — `//! ... }` on a doc comment
/// decremented depth, cascading wrong indentation through the rest of
/// the file. This caused ~19% of lines to mis-indent on a real Rust
/// source diagnostic.
fn bracket_net(line: &str) -> i32 {
    let mut net: i32 = 0;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            // `//` → rest of line is a comment, stop.
            '/' if chars.peek() == Some(&'/') => return net,
            '"' => {
                // String literal — consume until unescaped closing `"`.
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            chars.next();
                        } // skip escape byte
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '\'' => {
                // Char literal OR lifetime. A char literal closes within
                // a few chars (one or two for escapes). A lifetime is
                // `'ident` with no closing quote.
                //
                // Strategy: peek ahead for a closing `'`. If found
                // within ~4 chars, consume as char literal. Otherwise
                // treat the `'` as the start of a lifetime — leave the
                // remaining chars to be scanned normally.
                let saved: Vec<char> = chars.clone().take(5).collect();
                let close_idx = if saved.first() == Some(&'\\') {
                    saved.iter().skip(2).position(|&c| c == '\'').map(|p| p + 2)
                } else {
                    saved.iter().skip(1).position(|&c| c == '\'').map(|p| p + 1)
                };
                if let Some(idx) = close_idx {
                    for _ in 0..=idx {
                        chars.next();
                    }
                }
                // If no close found, leave chars alone — lifetime path.
            }
            '{' | '(' | '[' => net += 1,
            '}' | ')' | ']' => net -= 1,
            _ => {}
        }
    }
    net
}

/// Reindent rows `[top, bot]` using shiftwidth-based bracket-depth counting.
///
/// The indent for each line is computed as follows:
/// 1. Scan all rows from 0 up to the target row, accumulating a bracket depth
///    (`depth`) from net open − close brackets per line. The scan starts at row
///    0 to give correct depth for code that appears mid-buffer.
/// 2. For the target line, peek at its first non-whitespace character:
///    if it is a close bracket (`}`, `)`, `]`) then `effective_depth =
///    depth.saturating_sub(1)`; otherwise `effective_depth = depth`.
/// 3. Strip the line's existing leading whitespace and prepend
///    `effective_depth × indent_unit` where `indent_unit` is `"\t"` when
///    `expandtab == false` or `" " × shiftwidth` when `expandtab == true`.
/// 4. Empty / whitespace-only lines are left empty (no trailing whitespace).
/// 5. After computing the new line, advance `depth` by the line's bracket
///    net count (open − close), where the leading close-bracket already
///    contributed `−1` to the net of its own line.
///
/// **v1 limitation**: the bracket scan is naive — it does not skip brackets
/// inside string literals (`"{"`, `'['`) or comments (`// {`). Code with
/// such patterns will produce incorrect indent depths. Tree-sitter / LSP
/// indentation is deferred to a follow-up.
fn auto_indent_rows<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top: usize,
    bot: usize,
) {
    ed.sync_buffer_content_from_textarea();
    let shiftwidth = ed.settings().shiftwidth;
    let expandtab = ed.settings().expandtab;
    let indent_unit: String = if expandtab {
        " ".repeat(shiftwidth)
    } else {
        "\t".to_string()
    };

    let mut lines: Vec<String> = rope_to_lines_vec(&crate::types::Query::rope(&ed.buffer));
    let bot = bot.min(lines.len().saturating_sub(1));

    // Accumulate bracket depth from row 0 up to `top - 1` so we start with
    // the correct depth for the first line of the target range.
    let mut depth: i32 = 0;
    for line in lines.iter().take(top) {
        depth += bracket_net(line);
        if depth < 0 {
            depth = 0;
        }
    }

    for line in lines.iter_mut().take(bot + 1).skip(top) {
        let trimmed_owned = line.trim_start().to_owned();
        // Empty / whitespace-only lines stay empty.
        if trimmed_owned.is_empty() {
            *line = String::new();
            // depth contribution from an empty line is zero; no bracket scan needed.
            continue;
        }

        // Detect leading close-bracket for effective depth.
        let starts_with_close = trimmed_owned
            .chars()
            .next()
            .is_some_and(|c| matches!(c, '}' | ')' | ']'));
        // Chain continuation: a line starting with `.` (e.g. `.foo()`)
        // hangs off the previous expression and gets one extra indent
        // level, matching cargo fmt / clang-format conventions for
        // method chains like:
        //   let x = foo()
        //       .bar()
        //       .baz();
        // Range expressions (`..`) and try-chains (`?.`) are out of
        // scope for v1 — single leading `.` is the common case.
        let starts_with_dot = trimmed_owned.starts_with('.')
            && !trimmed_owned.starts_with("..")
            && !trimmed_owned.starts_with(".;");
        let effective_depth = if starts_with_close {
            depth.saturating_sub(1)
        } else if starts_with_dot {
            depth.saturating_add(1)
        } else {
            depth
        } as usize;

        // Build new line: indent × depth + stripped content.
        let new_line = format!("{}{}", indent_unit.repeat(effective_depth), trimmed_owned);

        // Advance depth by this line's net bracket count (scan trimmed content).
        depth += bracket_net(&trimmed_owned);
        if depth < 0 {
            depth = 0;
        }

        *line = new_line;
    }

    // Restore cursor to the first non-blank of `top` (vim parity for `==`).
    ed.restore(lines, (top, 0));
    move_first_non_whitespace(ed);
    // Record the touched row range so the host can display a visual flash.
    ed.last_indent_range = Some((top, bot));
}

fn toggle_case_str(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_lowercase() {
                c.to_uppercase().next().unwrap_or(c)
            } else if c.is_uppercase() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

fn order(a: (usize, usize), b: (usize, usize)) -> ((usize, usize), (usize, usize)) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Clamp the buffer cursor to normal-mode valid position: col may not
/// exceed `line.chars().count().saturating_sub(1)` (or 0 on an empty
/// line). Vim applies this clamp on every return to Normal mode after an
/// operator or Esc-from-insert.
fn clamp_cursor_to_normal_mode<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    let (row, col) = ed.cursor();
    let line_chars = buf_line_chars(&ed.buffer, row);
    let max_col = line_chars.saturating_sub(1);
    if col > max_col {
        buf_set_cursor_rc(&mut ed.buffer, row, max_col);
        ed.push_buffer_cursor_to_textarea();
    }
}

// ─── dd/cc/yy ──────────────────────────────────────────────────────────────

/// Expand a linewise `[start, end]` row range so it fully covers every CLOSED
/// fold it overlaps — vim's rule that a linewise operator on a closed fold acts
/// on the whole fold. Loops until stable so nested closed folds are absorbed.
fn expand_linewise_over_closed_folds(
    buf: &hjkl_buffer::Buffer,
    mut start: usize,
    mut end: usize,
) -> (usize, usize) {
    let folds = buf.folds();
    if folds.is_empty() {
        return (start, end);
    }
    loop {
        let mut changed = false;
        for f in &folds {
            if !f.closed {
                continue;
            }
            // Does this closed fold overlap the current range?
            if f.start_row <= end && f.end_row >= start {
                if f.start_row < start {
                    start = f.start_row;
                    changed = true;
                }
                if f.end_row > end {
                    end = f.end_row;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    (start, end)
}

fn execute_line_op<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    count: usize,
) {
    let (row, col) = ed.cursor();
    let total = buf_row_count(&ed.buffer);
    // Vim: `[count]op` for a linewise operator implies a `count_` motion that
    // moves `count - 1` lines down. On the last line that motion can't move at
    // all, so the whole operator aborts (E16) — `2dd`/`2yy`/`5>>`/`5<<` on the
    // final line are no-ops, not "operate on the one remaining line". When the
    // cursor is above the last line the motion clamps to the buffer end instead.
    //
    // A trailing newline is stored as a phantom empty final row, so the last
    // *content* line is one above it; use that as the boundary.
    let last_content_row = if total >= 2
        && buf_line(&ed.buffer, total - 1)
            .map(|s| s.is_empty())
            .unwrap_or(false)
    {
        total - 2
    } else {
        total.saturating_sub(1)
    };
    if count >= 2 && row >= last_content_row {
        return;
    }
    let end_row = (row + count.saturating_sub(1)).min(total.saturating_sub(1));

    // Vim: a linewise operator (`dd`/`yy`/`cc`/`>>`/…) with the cursor on a
    // CLOSED fold operates on the ENTIRE fold, not just the cursor line. Expand
    // the `[row, end_row]` range to cover any closed fold it touches (repeats
    // until stable so nested folds are absorbed too).
    let (row, end_row) = expand_linewise_over_closed_folds(&ed.buffer, row, end_row);

    match op {
        Operator::Yank => {
            // yy must not move the cursor.
            let text = read_vim_range(ed, (row, col), (end_row, 0), RangeKind::Linewise);
            if !text.is_empty() {
                ed.record_yank_to_host(text.clone());
                ed.record_yank(text, true);
            }
            // Vim `:h '[` / `:h ']`: yy/Nyy — linewise yank; `[` =
            // (top_row, 0), `]` = (bot_row, last_col).
            let last_col = buf_line_chars(&ed.buffer, end_row).saturating_sub(1);
            ed.set_mark('[', (row, 0));
            ed.set_mark(']', (end_row, last_col));
            buf_set_cursor_rc(&mut ed.buffer, row, col);
            ed.push_buffer_cursor_to_textarea();
            ed.vim.mode = Mode::Normal;
        }
        Operator::Delete => {
            ed.push_undo();
            let deleted_through_last = end_row + 1 >= total;
            cut_vim_range(ed, (row, col), (end_row, 0), RangeKind::Linewise);
            // Vim's `dd` / `Ndd` leaves the cursor on the *first
            // non-blank* of the line that now occupies `row` — or, if
            // the deletion consumed the last line, the line above it.
            let total_after = buf_row_count(&ed.buffer);
            let raw_target = if deleted_through_last {
                row.saturating_sub(1).min(total_after.saturating_sub(1))
            } else {
                row.min(total_after.saturating_sub(1))
            };
            // Clamp off the trailing phantom empty row that arises from a
            // buffer with a trailing newline (stored as ["...", ""]). If
            // the target row is the trailing empty row and there is a real
            // content row above it, use that instead — matching vim's view
            // that the trailing `\n` is a terminator, not a separator.
            let target_row = if raw_target > 0
                && raw_target + 1 == total_after
                && buf_line(&ed.buffer, raw_target)
                    .map(|s| s.is_empty())
                    .unwrap_or(false)
            {
                raw_target - 1
            } else {
                raw_target
            };
            buf_set_cursor_rc(&mut ed.buffer, target_row, 0);
            ed.push_buffer_cursor_to_textarea();
            move_first_non_whitespace(ed);
            ed.sticky_col = Some(ed.cursor().1);
            ed.vim.mode = Mode::Normal;
            // Vim `:h '[` / `:h ']`: dd/Ndd — both marks park at the
            // post-delete cursor position (the join point).
            let pos = ed.cursor();
            ed.set_mark('[', pos);
            ed.set_mark(']', pos);
        }
        Operator::Change => {
            // `cc` / `3cc`: delegate to the shared linewise-change helper
            // which preserves the first line's indent, leaves one row open,
            // and enters insert mode.
            change_linewise_rows(ed, row, end_row);
        }
        Operator::Uppercase | Operator::Lowercase | Operator::ToggleCase | Operator::Rot13 => {
            // `gUU` / `guu` / `g~~` / `g??` — linewise case/rot13 transform over
            // [row, end_row]. Preserve cursor on `row` (first non-blank
            // lines up with vim's behaviour).
            apply_case_op_to_selection(ed, op, (row, col), (end_row, 0), RangeKind::Linewise);
            // After case-op on a linewise range vim puts the cursor on
            // the first non-blank of the starting line.
            move_first_non_whitespace(ed);
        }
        Operator::Indent | Operator::Outdent => {
            // `>>` / `N>>` / `<<` / `N<<` — linewise indent / outdent.
            ed.push_undo();
            if op == Operator::Indent {
                indent_rows(ed, row, end_row, 1);
            } else {
                outdent_rows(ed, row, end_row, 1);
            }
            ed.sticky_col = Some(ed.cursor().1);
            ed.vim.mode = Mode::Normal;
        }
        // No doubled form — `zfzf` is two consecutive `zf` chords.
        Operator::Fold => unreachable!("Fold has no line-op double"),
        Operator::Reflow => {
            // `gqq` / `Ngqq` — reflow `count` rows starting at the cursor.
            ed.push_undo();
            reflow_rows(ed, row, end_row);
            move_first_non_whitespace(ed);
            ed.sticky_col = Some(ed.cursor().1);
            ed.vim.mode = Mode::Normal;
        }
        Operator::ReflowKeepCursor => {
            // `gww` / `Ngww` — reflow `count` rows starting at the cursor,
            // but leave the cursor at the character it was on before reflow.
            let saved = ed.cursor();
            ed.push_undo();
            let (before, after) = reflow_rows_keep_cursor(ed, row, end_row);
            let (new_row, new_col) = reflow_keep_cursor(row, saved.0, saved.1, &before, &after);
            buf_set_cursor_rc(&mut ed.buffer, new_row, new_col);
            ed.push_buffer_cursor_to_textarea();
            ed.sticky_col = Some(new_col);
            ed.vim.mode = Mode::Normal;
        }
        Operator::AutoIndent => {
            // `==` / `N==` — auto-indent `count` rows starting at cursor.
            ed.push_undo();
            auto_indent_rows(ed, row, end_row);
            ed.sticky_col = Some(ed.cursor().1);
            ed.vim.mode = Mode::Normal;
        }
        Operator::Filter => {
            // Filter is dispatched through Editor::filter_range, not here.
        }
        Operator::Comment => {
            // Comment is dispatched through Editor::toggle_comment_range, not here.
            // The doubled `gcc` path calls toggle_comment_range directly in
            // apply_after_g, then records last_change. execute_line_op should
            // not be reached for Comment — no-op if it is.
        }
    }
}

// ─── Visual mode operators ─────────────────────────────────────────────────

pub(crate) fn apply_visual_operator<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    count: usize,
) {
    // `count` is the number of indent levels for `>` / `<` (vim `2>` = two
    // shiftwidths); other visual operators ignore it.
    let levels = count.max(1);
    match ed.vim.mode {
        Mode::VisualLine => {
            let cursor_row = buf_cursor_pos(&ed.buffer).row;
            let top = cursor_row.min(ed.vim.visual_line_anchor);
            let bot = cursor_row.max(ed.vim.visual_line_anchor);
            ed.vim.yank_linewise = true;
            match op {
                Operator::Yank => {
                    let text = read_vim_range(ed, (top, 0), (bot, 0), RangeKind::Linewise);
                    if !text.is_empty() {
                        ed.record_yank_to_host(text.clone());
                        ed.record_yank(text, true);
                    }
                    buf_set_cursor_rc(&mut ed.buffer, top, 0);
                    ed.push_buffer_cursor_to_textarea();
                    ed.vim.mode = Mode::Normal;
                }
                Operator::Delete => {
                    ed.push_undo();
                    cut_vim_range(ed, (top, 0), (bot, 0), RangeKind::Linewise);
                    ed.vim.mode = Mode::Normal;
                }
                Operator::Change => {
                    // Vim `Vc` / `Vjc`: same linewise-change semantics as
                    // `cc` — preserve first line's indent, enter insert.
                    change_linewise_rows(ed, top, bot);
                }
                Operator::Uppercase
                | Operator::Lowercase
                | Operator::ToggleCase
                | Operator::Rot13 => {
                    let bot = buf_cursor_pos(&ed.buffer)
                        .row
                        .max(ed.vim.visual_line_anchor);
                    apply_case_op_to_selection(ed, op, (top, 0), (bot, 0), RangeKind::Linewise);
                    move_first_non_whitespace(ed);
                }
                Operator::Indent | Operator::Outdent => {
                    ed.push_undo();
                    let (cursor_row, _) = ed.cursor();
                    let bot = cursor_row.max(ed.vim.visual_line_anchor);
                    if op == Operator::Indent {
                        indent_rows(ed, top, bot, levels);
                    } else {
                        outdent_rows(ed, top, bot, levels);
                    }
                    ed.vim.mode = Mode::Normal;
                }
                Operator::Reflow => {
                    ed.push_undo();
                    let (cursor_row, _) = ed.cursor();
                    let bot = cursor_row.max(ed.vim.visual_line_anchor);
                    reflow_rows(ed, top, bot);
                    ed.vim.mode = Mode::Normal;
                }
                Operator::ReflowKeepCursor => {
                    let saved = ed.cursor();
                    ed.push_undo();
                    let (cursor_row, _) = ed.cursor();
                    let bot = cursor_row.max(ed.vim.visual_line_anchor);
                    let (before, after) = reflow_rows_keep_cursor(ed, top, bot);
                    let (new_row, new_col) =
                        reflow_keep_cursor(top, saved.0, saved.1, &before, &after);
                    buf_set_cursor_rc(&mut ed.buffer, new_row, new_col);
                    ed.push_buffer_cursor_to_textarea();
                    ed.vim.mode = Mode::Normal;
                }
                Operator::AutoIndent => {
                    ed.push_undo();
                    let (cursor_row, _) = ed.cursor();
                    let bot = cursor_row.max(ed.vim.visual_line_anchor);
                    auto_indent_rows(ed, top, bot);
                    ed.vim.mode = Mode::Normal;
                }
                // Filter is dispatched through Editor::filter_range, not here.
                Operator::Filter => {}
                // Comment is dispatched through the app layer (engine_actions.rs), not here.
                Operator::Comment => {}
                // Visual `zf` is handled inline in `handle_after_z`,
                // never routed through this dispatcher.
                Operator::Fold => unreachable!("Visual zf takes its own path"),
            }
        }
        Mode::Visual => {
            ed.vim.yank_linewise = false;
            let anchor = ed.vim.visual_anchor;
            let cursor = ed.cursor();
            let (top, bot) = order(anchor, cursor);
            match op {
                Operator::Yank => {
                    let text = read_vim_range(ed, top, bot, RangeKind::Inclusive);
                    if !text.is_empty() {
                        ed.record_yank_to_host(text.clone());
                        ed.record_yank(text, false);
                    }
                    buf_set_cursor_rc(&mut ed.buffer, top.0, top.1);
                    ed.push_buffer_cursor_to_textarea();
                    ed.vim.mode = Mode::Normal;
                }
                Operator::Delete => {
                    ed.push_undo();
                    cut_vim_range(ed, top, bot, RangeKind::Inclusive);
                    ed.vim.mode = Mode::Normal;
                }
                Operator::Change => {
                    ed.push_undo();
                    cut_vim_range(ed, top, bot, RangeKind::Inclusive);
                    begin_insert_noundo(ed, 1, InsertReason::AfterChange);
                }
                Operator::Uppercase
                | Operator::Lowercase
                | Operator::ToggleCase
                | Operator::Rot13 => {
                    // Anchor stays where the visual selection started.
                    let anchor = ed.vim.visual_anchor;
                    let cursor = ed.cursor();
                    let (top, bot) = order(anchor, cursor);
                    apply_case_op_to_selection(ed, op, top, bot, RangeKind::Inclusive);
                }
                Operator::Indent | Operator::Outdent => {
                    ed.push_undo();
                    let anchor = ed.vim.visual_anchor;
                    let cursor = ed.cursor();
                    let (top, bot) = order(anchor, cursor);
                    if op == Operator::Indent {
                        indent_rows(ed, top.0, bot.0, levels);
                    } else {
                        outdent_rows(ed, top.0, bot.0, levels);
                    }
                    ed.vim.mode = Mode::Normal;
                }
                Operator::Reflow => {
                    ed.push_undo();
                    let anchor = ed.vim.visual_anchor;
                    let cursor = ed.cursor();
                    let (top, bot) = order(anchor, cursor);
                    reflow_rows(ed, top.0, bot.0);
                    ed.vim.mode = Mode::Normal;
                }
                Operator::ReflowKeepCursor => {
                    let saved = ed.cursor();
                    ed.push_undo();
                    let anchor = ed.vim.visual_anchor;
                    let cursor = ed.cursor();
                    let (top, bot) = order(anchor, cursor);
                    let (before, after) = reflow_rows_keep_cursor(ed, top.0, bot.0);
                    let (new_row, new_col) =
                        reflow_keep_cursor(top.0, saved.0, saved.1, &before, &after);
                    buf_set_cursor_rc(&mut ed.buffer, new_row, new_col);
                    ed.push_buffer_cursor_to_textarea();
                    ed.vim.mode = Mode::Normal;
                }
                Operator::AutoIndent => {
                    ed.push_undo();
                    let anchor = ed.vim.visual_anchor;
                    let cursor = ed.cursor();
                    let (top, bot) = order(anchor, cursor);
                    auto_indent_rows(ed, top.0, bot.0);
                    ed.vim.mode = Mode::Normal;
                }
                // Filter is dispatched through Editor::filter_range, not here.
                Operator::Filter => {}
                // Comment is dispatched through the app layer (engine_actions.rs), not here.
                Operator::Comment => {}
                Operator::Fold => unreachable!("Visual zf takes its own path"),
            }
        }
        Mode::VisualBlock => apply_block_operator(ed, op, levels),
        _ => {}
    }
}

/// Compute `(top_row, bot_row, left_col, right_col)` for the current
/// VisualBlock selection. Columns are inclusive on both ends. Uses the
/// tracked virtual column (updated by h/l, preserved across j/k) so
/// ragged / empty rows don't collapse the block's width.
fn block_bounds<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> (usize, usize, usize, usize) {
    let (ar, ac) = ed.vim.block_anchor;
    let (cr, _) = ed.cursor();
    let cc = ed.vim.block_vcol;
    let top = ar.min(cr);
    let bot = ar.max(cr);
    let left = ac.min(cc);
    let right = ac.max(cc);
    (top, bot, left, right)
}

/// Update the virtual column after a motion in VisualBlock mode.
/// Horizontal motions sync `block_vcol` to the new cursor column;
/// vertical / non-h/l motions leave it alone so the intended column
/// survives clamping to shorter lines.
pub(crate) fn update_block_vcol<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    motion: &Motion,
) {
    match motion {
        Motion::Left
        | Motion::Right
        | Motion::WordFwd
        | Motion::BigWordFwd
        | Motion::WordBack
        | Motion::BigWordBack
        | Motion::WordEnd
        | Motion::BigWordEnd
        | Motion::WordEndBack
        | Motion::BigWordEndBack
        | Motion::LineStart
        | Motion::FirstNonBlank
        | Motion::LineEnd
        | Motion::Find { .. }
        | Motion::FindRepeat { .. }
        | Motion::MatchBracket => {
            ed.vim.block_vcol = ed.cursor().1;
        }
        // Up / Down / FileTop / FileBottom / Search — preserve vcol.
        _ => {}
    }
}

/// Yank / delete / change / replace a rectangular selection. Yanked text
/// is stored as one string per row joined with `\n` so pasting reproduces
/// the block as sequential lines. (Vim's true block-paste reinserts as
/// columns; we render the content with our char-wise paste path.)
fn apply_block_operator<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    count: usize,
) {
    let (top, bot, left, right) = block_bounds(ed);
    // Snapshot the block text for yank / clipboard.
    let yank = block_yank(ed, top, bot, left, right);

    match op {
        Operator::Yank => {
            if !yank.is_empty() {
                ed.record_yank_to_host(yank.clone());
                ed.record_yank(yank, false);
            }
            ed.vim.mode = Mode::Normal;
            ed.jump_cursor(top, left);
        }
        Operator::Delete => {
            ed.push_undo();
            delete_block_contents(ed, top, bot, left, right);
            if !yank.is_empty() {
                ed.record_yank_to_host(yank.clone());
                ed.record_delete(yank, false);
            }
            ed.vim.mode = Mode::Normal;
            ed.jump_cursor(top, left);
        }
        Operator::Change => {
            ed.push_undo();
            delete_block_contents(ed, top, bot, left, right);
            if !yank.is_empty() {
                ed.record_yank_to_host(yank.clone());
                ed.record_delete(yank, false);
            }
            ed.jump_cursor(top, left);
            begin_insert_noundo(
                ed,
                1,
                InsertReason::BlockChange {
                    top,
                    bot,
                    col: left,
                },
            );
        }
        Operator::Uppercase | Operator::Lowercase | Operator::ToggleCase | Operator::Rot13 => {
            ed.push_undo();
            transform_block_case(ed, op, top, bot, left, right);
            ed.vim.mode = Mode::Normal;
            ed.jump_cursor(top, left);
        }
        Operator::Indent | Operator::Outdent => {
            // VisualBlock `>` / `<` falls back to linewise indent over
            // the block's row range — vim does the same (column-wise
            // indent/outdent doesn't make sense).
            ed.push_undo();
            if op == Operator::Indent {
                indent_rows(ed, top, bot, count.max(1));
            } else {
                outdent_rows(ed, top, bot, count.max(1));
            }
            ed.vim.mode = Mode::Normal;
        }
        Operator::Fold => unreachable!("Visual zf takes its own path"),
        Operator::Reflow => {
            // Reflow over the block falls back to linewise reflow over
            // the row range — column slicing for `gq` doesn't make
            // sense.
            ed.push_undo();
            reflow_rows(ed, top, bot);
            ed.vim.mode = Mode::Normal;
        }
        Operator::ReflowKeepCursor => {
            // `gw` over a block: same fallback as `gq` but restore cursor.
            let saved = ed.cursor();
            ed.push_undo();
            let (before, after) = reflow_rows_keep_cursor(ed, top, bot);
            let (new_row, new_col) = reflow_keep_cursor(top, saved.0, saved.1, &before, &after);
            buf_set_cursor_rc(&mut ed.buffer, new_row, new_col);
            ed.push_buffer_cursor_to_textarea();
            ed.vim.mode = Mode::Normal;
        }
        Operator::AutoIndent => {
            // AutoIndent over the block falls back to linewise
            // auto-indent over the row range.
            ed.push_undo();
            auto_indent_rows(ed, top, bot);
            ed.vim.mode = Mode::Normal;
        }
        // Filter is dispatched through Editor::filter_range, not here.
        Operator::Filter => {}
        // Comment is dispatched through the app layer (engine_actions.rs), not here.
        Operator::Comment => {}
    }
}

/// In-place case transform over the rectangular block
/// `(top..=bot, left..=right)`. Rows shorter than `left` are left
/// untouched — vim behaves the same way (ragged blocks).
fn transform_block_case<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    top: usize,
    bot: usize,
    left: usize,
    right: usize,
) {
    let mut lines: Vec<String> = rope_to_lines_vec(&crate::types::Query::rope(&ed.buffer));
    for r in top..=bot.min(lines.len().saturating_sub(1)) {
        let chars: Vec<char> = lines[r].chars().collect();
        if left >= chars.len() {
            continue;
        }
        let end = (right + 1).min(chars.len());
        let head: String = chars[..left].iter().collect();
        let mid: String = chars[left..end].iter().collect();
        let tail: String = chars[end..].iter().collect();
        let transformed = match op {
            Operator::Uppercase => mid.to_uppercase(),
            Operator::Lowercase => mid.to_lowercase(),
            Operator::ToggleCase => toggle_case_str(&mid),
            Operator::Rot13 => rot13_str(&mid),
            _ => mid,
        };
        lines[r] = format!("{head}{transformed}{tail}");
    }
    let saved_yank = ed.yank().to_string();
    let saved_linewise = ed.vim.yank_linewise;
    ed.restore(lines, (top, left));
    ed.set_yank(saved_yank);
    ed.vim.yank_linewise = saved_linewise;
}

fn block_yank<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    top: usize,
    bot: usize,
    left: usize,
    right: usize,
) -> String {
    let rope = crate::types::Query::rope(&ed.buffer);
    let n = rope.len_lines();
    let mut rows: Vec<String> = Vec::new();
    for r in top..=bot {
        if r >= n {
            break;
        }
        let line = rope_line_to_str(&rope, r);
        let chars: Vec<char> = line.chars().collect();
        let end = (right + 1).min(chars.len());
        if left >= chars.len() {
            rows.push(String::new());
        } else {
            rows.push(chars[left..end].iter().collect());
        }
    }
    rows.join("\n")
}

fn delete_block_contents<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top: usize,
    bot: usize,
    left: usize,
    right: usize,
) {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let last_row = bot.min(buf_row_count(&ed.buffer).saturating_sub(1));
    if last_row < top {
        return;
    }
    ed.mutate_edit(Edit::DeleteRange {
        start: Position::new(top, left),
        end: Position::new(last_row, right),
        kind: MotionKind::Block,
    });
    ed.push_buffer_cursor_to_textarea();
}

/// Replace each character cell in the block with `ch`.
pub(crate) fn block_replace<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    ch: char,
) {
    let (top, bot, left, right) = block_bounds(ed);
    ed.push_undo();
    ed.sync_buffer_content_from_textarea();
    let mut lines: Vec<String> = rope_to_lines_vec(&crate::types::Query::rope(&ed.buffer));
    for r in top..=bot.min(lines.len().saturating_sub(1)) {
        let chars: Vec<char> = lines[r].chars().collect();
        if left >= chars.len() {
            continue;
        }
        let end = (right + 1).min(chars.len());
        let before: String = chars[..left].iter().collect();
        let middle: String = std::iter::repeat_n(ch, end - left).collect();
        let after: String = chars[end..].iter().collect();
        lines[r] = format!("{before}{middle}{after}");
    }
    reset_textarea_lines(ed, lines);
    ed.vim.mode = Mode::Normal;
    ed.jump_cursor(top, left);
}

/// Replace buffer content with `lines` while preserving the cursor.
/// Used by indent / outdent / block_replace to wholesale rewrite
/// rows without going through the per-edit funnel.
fn reset_textarea_lines<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    lines: Vec<String>,
) {
    let cursor = ed.cursor();
    crate::types::BufferEdit::replace_all(&mut ed.buffer, &lines.join("\n"));
    buf_set_cursor_rc(&mut ed.buffer, cursor.0, cursor.1);
    ed.mark_content_dirty();
}

// ─── Visual-line helpers ───────────────────────────────────────────────────

// ─── Text-object range computation ─────────────────────────────────────────

/// Cursor position as `(row, col)`.
type Pos = (usize, usize);

/// Returns `(start, end, kind)` where `end` is *exclusive* (one past the
/// last character to act on). `kind` is `Linewise` for line-oriented text
/// objects like paragraphs and `Exclusive` otherwise.
pub(crate) fn text_object_range<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    obj: TextObject,
    inner: bool,
    count: usize,
) -> Option<(Pos, Pos, RangeKind)> {
    match obj {
        TextObject::Word { big } => {
            word_text_object(ed, inner, big).map(|(s, e)| (s, e, RangeKind::Exclusive))
        }
        TextObject::Quote(q) => {
            quote_text_object(ed, q, inner).map(|(s, e)| (s, e, RangeKind::Exclusive))
        }
        TextObject::Bracket(open) => bracket_text_object(ed, open, inner, count),
        TextObject::Paragraph => {
            paragraph_text_object(ed, inner).map(|(s, e)| (s, e, RangeKind::Linewise))
        }
        TextObject::XmlTag => tag_text_object(ed, inner).map(|(s, e)| (s, e, RangeKind::Exclusive)),
        TextObject::Sentence => {
            sentence_text_object(ed, inner).map(|(s, e)| (s, e, RangeKind::Exclusive))
        }
    }
}

/// `(` / `)` — walk to the next sentence boundary in `forward` direction.
/// Returns `(row, col)` of the boundary's first non-whitespace cell, or
/// `None` when already at the buffer's edge in that direction.
fn sentence_boundary<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    forward: bool,
) -> Option<(usize, usize)> {
    let rope = crate::types::Query::rope(&ed.buffer);
    let n_lines = rope.len_lines();
    if n_lines == 0 {
        return None;
    }
    // Per-line char counts (excluding trailing \n) for pos↔idx conversion.
    let line_lens: Vec<usize> = (0..n_lines)
        .map(|r| rope_line_to_str(&rope, r).chars().count())
        .collect();
    let pos_to_idx = |pos: (usize, usize)| -> usize {
        let idx: usize = line_lens.iter().take(pos.0).map(|&len| len + 1).sum();
        idx + pos.1
    };
    let idx_to_pos = |mut idx: usize| -> (usize, usize) {
        for (r, &len) in line_lens.iter().enumerate() {
            if idx <= len {
                return (r, idx);
            }
            idx -= len + 1;
        }
        let last = n_lines.saturating_sub(1);
        (last, line_lens[last])
    };
    // Build flat char vector: rope chars already include \n between lines.
    // ropey's last line has no trailing \n; intermediate ones do.
    let mut chars: Vec<char> = rope.chars().collect();
    // Strip a trailing \n if ropey emitted one on the final line.
    if chars.last() == Some(&'\n') {
        chars.pop();
    }
    if chars.is_empty() {
        return None;
    }
    let total = chars.len();
    let cursor_idx = pos_to_idx(ed.cursor()).min(total - 1);
    let is_terminator = |c: char| matches!(c, '.' | '?' | '!');

    if forward {
        // Walk forward looking for a terminator run followed by
        // whitespace; land on the first non-whitespace cell after.
        let mut i = cursor_idx + 1;
        while i < total {
            if is_terminator(chars[i]) {
                while i + 1 < total && is_terminator(chars[i + 1]) {
                    i += 1;
                }
                if i + 1 >= total {
                    return None;
                }
                if chars[i + 1].is_whitespace() {
                    let mut j = i + 1;
                    while j < total && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if j >= total {
                        return None;
                    }
                    return Some(idx_to_pos(j));
                }
            }
            i += 1;
        }
        None
    } else {
        // Walk backward to find the start of the current sentence (if
        // we're already at the start, jump to the previous sentence's
        // start instead).
        let find_start = |from: usize| -> Option<usize> {
            let mut start = from;
            while start > 0 {
                let prev = chars[start - 1];
                if prev.is_whitespace() {
                    let mut k = start - 1;
                    while k > 0 && chars[k - 1].is_whitespace() {
                        k -= 1;
                    }
                    if k > 0 && is_terminator(chars[k - 1]) {
                        break;
                    }
                }
                start -= 1;
            }
            while start < total && chars[start].is_whitespace() {
                start += 1;
            }
            (start < total).then_some(start)
        };
        let current_start = find_start(cursor_idx)?;
        if current_start < cursor_idx {
            return Some(idx_to_pos(current_start));
        }
        // Already at the sentence start — step over the boundary into
        // the previous sentence and find its start.
        let mut k = current_start;
        while k > 0 && chars[k - 1].is_whitespace() {
            k -= 1;
        }
        if k == 0 {
            return None;
        }
        let prev_start = find_start(k - 1)?;
        Some(idx_to_pos(prev_start))
    }
}

/// `is` / `as` — sentence: text up to and including the next sentence
/// terminator (`.`, `?`, `!`). Vim treats `.`/`?`/`!` followed by
/// whitespace (or end-of-line) as a boundary; runs of consecutive
/// terminators stay attached to the same sentence. `as` extends to
/// include trailing whitespace; `is` does not.
fn sentence_text_object<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    inner: bool,
) -> Option<((usize, usize), (usize, usize))> {
    let rope = crate::types::Query::rope(&ed.buffer);
    let n_lines = rope.len_lines();
    if n_lines == 0 {
        return None;
    }
    // Flatten the buffer so a sentence can span lines (vim's behaviour).
    // Newlines count as whitespace for boundary detection.
    let line_lens: Vec<usize> = (0..n_lines)
        .map(|r| rope_line_to_str(&rope, r).chars().count())
        .collect();
    let pos_to_idx = |pos: (usize, usize)| -> usize {
        let idx: usize = line_lens.iter().take(pos.0).map(|&len| len + 1).sum();
        idx + pos.1
    };
    let idx_to_pos = |mut idx: usize| -> (usize, usize) {
        for (r, &len) in line_lens.iter().enumerate() {
            if idx <= len {
                return (r, idx);
            }
            idx -= len + 1;
        }
        let last = n_lines.saturating_sub(1);
        (last, line_lens[last])
    };
    let mut chars: Vec<char> = rope.chars().collect();
    if chars.last() == Some(&'\n') {
        chars.pop();
    }
    if chars.is_empty() {
        return None;
    }

    let cursor_idx = pos_to_idx(ed.cursor()).min(chars.len() - 1);
    let is_terminator = |c: char| matches!(c, '.' | '?' | '!');

    // Walk backward from cursor to find the start of the current
    // sentence. A boundary is: whitespace immediately after a run of
    // terminators (or start-of-buffer).
    let mut start = cursor_idx;
    while start > 0 {
        let prev = chars[start - 1];
        if prev.is_whitespace() {
            // Check if the whitespace follows a terminator — if so,
            // we've crossed a sentence boundary; the sentence begins
            // at the first non-whitespace cell *after* this run.
            let mut k = start - 1;
            while k > 0 && chars[k - 1].is_whitespace() {
                k -= 1;
            }
            if k > 0 && is_terminator(chars[k - 1]) {
                break;
            }
        }
        start -= 1;
    }
    // Skip leading whitespace (vim doesn't include it in the
    // sentence body).
    while start < chars.len() && chars[start].is_whitespace() {
        start += 1;
    }
    if start >= chars.len() {
        return None;
    }

    // Walk forward to the sentence end (last terminator before the
    // next whitespace boundary).
    let mut end = start;
    while end < chars.len() {
        if is_terminator(chars[end]) {
            // Consume any consecutive terminators (e.g. `?!`).
            while end + 1 < chars.len() && is_terminator(chars[end + 1]) {
                end += 1;
            }
            // If followed by whitespace or end-of-buffer, that's the
            // boundary.
            if end + 1 >= chars.len() || chars[end + 1].is_whitespace() {
                break;
            }
        }
        end += 1;
    }
    // Inclusive end → exclusive end_idx.
    let end_idx = (end + 1).min(chars.len());

    let final_end = if inner {
        end_idx
    } else {
        // `as`: include trailing whitespace (but stop before the next
        // newline so we don't gobble a paragraph break — vim keeps
        // sentences within a paragraph for the trailing-ws extension).
        let mut e = end_idx;
        while e < chars.len() && chars[e].is_whitespace() && chars[e] != '\n' {
            e += 1;
        }
        e
    };

    Some((idx_to_pos(start), idx_to_pos(final_end)))
}

/// `it` / `at` — XML tag pair text object. Builds a flat char index of
/// the buffer, walks `<...>` tokens to pair tags via a stack, and
/// returns the innermost pair containing the cursor.
fn tag_text_object<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    inner: bool,
) -> Option<((usize, usize), (usize, usize))> {
    let rope = crate::types::Query::rope(&ed.buffer);
    let n_lines = rope.len_lines();
    if n_lines == 0 {
        return None;
    }
    // Flatten char positions so we can compare cursor against tag
    // ranges without per-row arithmetic. `\n` between lines counts as
    // a single char.
    let line_lens: Vec<usize> = (0..n_lines)
        .map(|r| rope_line_to_str(&rope, r).chars().count())
        .collect();
    let pos_to_idx = |pos: (usize, usize)| -> usize {
        let idx: usize = line_lens.iter().take(pos.0).map(|&len| len + 1).sum();
        idx + pos.1
    };
    let idx_to_pos = |mut idx: usize| -> (usize, usize) {
        for (r, &len) in line_lens.iter().enumerate() {
            if idx <= len {
                return (r, idx);
            }
            idx -= len + 1;
        }
        let last = n_lines.saturating_sub(1);
        (last, line_lens[last])
    };
    let mut chars: Vec<char> = rope.chars().collect();
    if chars.last() == Some(&'\n') {
        chars.pop();
    }
    let cursor_idx = pos_to_idx(ed.cursor());

    // Walk `<...>` tokens. Track open tags on a stack; on a matching
    // close pop and consider the pair a candidate when the cursor lies
    // inside its content range. Innermost wins (replace whenever a
    // tighter range turns up). Also track the first complete pair that
    // starts at or after the cursor so we can fall back to a forward
    // scan (targets.vim-style) when the cursor isn't inside any tag.
    let mut stack: Vec<(usize, usize, String)> = Vec::new(); // (open_start, content_start, name)
    let mut innermost: Option<(usize, usize, usize, usize)> = None;
    let mut next_after: Option<(usize, usize, usize, usize)> = None;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '<' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && chars[j] != '>' {
            j += 1;
        }
        if j >= chars.len() {
            break;
        }
        let inside: String = chars[i + 1..j].iter().collect();
        let close_end = j + 1;
        let trimmed = inside.trim();
        if trimmed.starts_with('!') || trimmed.starts_with('?') {
            i = close_end;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('/') {
            let name = rest.split_whitespace().next().unwrap_or("").to_string();
            if !name.is_empty()
                && let Some(stack_idx) = stack.iter().rposition(|(_, _, n)| *n == name)
            {
                let (open_start, content_start, _) = stack[stack_idx].clone();
                stack.truncate(stack_idx);
                let content_end = i;
                let candidate = (open_start, content_start, content_end, close_end);
                // A pair encloses the cursor when the cursor lies anywhere
                // within the whole pair span — including ON the open or close
                // tag itself (vim `it`/`at` operate on the tag under the
                // cursor, not just its content). Innermost (tightest span)
                // wins; closes are seen innermost-first so the first enclosing
                // candidate is already the tightest.
                if cursor_idx >= open_start && cursor_idx < close_end {
                    innermost = match innermost {
                        Some((os, _, _, ce)) if os <= open_start && close_end <= ce => {
                            Some(candidate)
                        }
                        None => Some(candidate),
                        existing => existing,
                    };
                } else if open_start >= cursor_idx && next_after.is_none() {
                    next_after = Some(candidate);
                }
            }
        } else if !trimmed.ends_with('/') {
            let name: String = trimmed
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                stack.push((i, close_end, name));
            }
        }
        i = close_end;
    }

    let (open_start, content_start, content_end, close_end) = innermost.or(next_after)?;
    if inner {
        Some((idx_to_pos(content_start), idx_to_pos(content_end)))
    } else {
        Some((idx_to_pos(open_start), idx_to_pos(close_end)))
    }
}

fn is_wordchar(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

// `is_keyword_char` lives in hjkl-buffer (used by word motions);
// engine re-uses it via `hjkl_buffer::is_keyword_char` so there's
// one parser, one default, one bug surface.
pub(crate) use hjkl_buffer::is_keyword_char;

/// Classify a vim abbreviation lhs into its type.
///
/// - **Full**: every char in `lhs` is a keyword char (full-id).
/// - **End**: the last char is a keyword char, at least one other is not (end-id).
/// - **None**: the last char is a non-keyword char (non-id).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AbbrevKind {
    /// All keyword chars (full-id).
    Full,
    /// Last char keyword, others include non-keyword (end-id).
    End,
    /// Last char is non-keyword (non-id).
    NonKw,
}

pub(crate) fn abbrev_kind(lhs: &str, iskeyword: &str) -> AbbrevKind {
    let chars: Vec<char> = lhs.chars().collect();
    if chars.is_empty() {
        return AbbrevKind::NonKw;
    }
    let last = *chars.last().unwrap();
    let last_is_kw = is_keyword_char(last, iskeyword);
    if !last_is_kw {
        return AbbrevKind::NonKw;
    }
    // last is keyword — check if all chars are keyword
    let all_kw = chars.iter().all(|&c| is_keyword_char(c, iskeyword));
    if all_kw {
        AbbrevKind::Full
    } else {
        AbbrevKind::End
    }
}

/// Try to match and expand an abbreviation given the text before the cursor.
///
/// # Parameters
/// - `abbrevs` — the active abbreviation table (insert-mode entries).
/// - `line_before` — the text on the current line *before* the cursor (char slice).
/// - `mincol` — first column index (0-based, char-indexed) that belongs to the
///   current insert session on the **same row as the cursor**.  Chars before
///   `mincol` were in the buffer before insert mode started and must NOT be
///   consumed as part of the lhs.  When the cursor is on a different row than
///   `start_row`, `mincol` is treated as 0 (the entire line was typed in this
///   session).
/// - `trigger` — what the user did (typed a non-kw char, pressed CR/Esc/C-]).
/// - `iskeyword` — the active iskeyword spec string.
///
/// Returns `Some((lhs_char_len, rhs))` on a match, where `lhs_char_len` is the
/// number of characters to delete before the cursor (the lhs), and `rhs` is the
/// text to insert in their place.  Returns `None` when no abbreviation matches.
pub(crate) fn try_abbrev_expand(
    abbrevs: &[Abbrev],
    line_before: &str,
    mincol: usize,
    trigger: AbbrevTrigger,
    iskeyword: &str,
) -> Option<(usize, String)> {
    let chars: Vec<char> = line_before.chars().collect();
    let cursor_col = chars.len(); // col of the cursor (0-based)

    for abbrev in abbrevs {
        if !abbrev.insert {
            continue;
        }
        let lhs_chars: Vec<char> = abbrev.lhs.chars().collect();
        if lhs_chars.is_empty() {
            continue;
        }
        let lhs_len = lhs_chars.len();

        // Determine the lhs type.
        let kind = abbrev_kind(&abbrev.lhs, iskeyword);

        // Trigger rules by lhs type.
        match kind {
            AbbrevKind::Full | AbbrevKind::End => {
                // full-id / end-id: trigger char must be a NON-keyword char
                // (space, punctuation, CR, Esc, C-]).
                let trigger_char_is_kw = match trigger {
                    AbbrevTrigger::NonKeyword(c) => is_keyword_char(c, iskeyword),
                    AbbrevTrigger::CtrlBracket | AbbrevTrigger::Cr | AbbrevTrigger::Esc => false,
                };
                if trigger_char_is_kw {
                    // A keyword trigger char would extend the word — no expand.
                    continue;
                }
            }
            AbbrevKind::NonKw => {
                // non-id: only expand on CR, Esc, C-].  NOT on regular typed chars.
                match trigger {
                    AbbrevTrigger::Cr | AbbrevTrigger::Esc | AbbrevTrigger::CtrlBracket => {}
                    AbbrevTrigger::NonKeyword(_) => continue,
                }
            }
        }

        // Check that the text before the cursor ends with the lhs.
        if cursor_col < lhs_len {
            continue;
        }
        let lhs_start_col = cursor_col - lhs_len;

        // Enforce mincol: the lhs must not start before the insert-start column.
        if lhs_start_col < mincol {
            continue;
        }

        // Compare chars.
        let text_slice: &[char] = &chars[lhs_start_col..cursor_col];
        if text_slice != lhs_chars.as_slice() {
            continue;
        }

        // Check "front" rule: the char immediately before the lhs.
        if lhs_start_col > 0 {
            let ch_before = chars[lhs_start_col - 1];
            match kind {
                AbbrevKind::Full => {
                    // full-id: char before lhs must be a non-keyword char.
                    // Single-char full-id exception: if the char before is a
                    // non-keyword char that is NOT space/tab, it is NOT recognised
                    // (vim `:h abbreviations`: "A word in front of a full-id abbrev
                    // is a non-keyword char; but a single char abbrev is not
                    // recognised after a non-blank, non-keyword char").
                    // Actually vim's rule: full-id is not recognised if the char
                    // before is a NON-keyword char other than space/tab AND the lhs
                    // is a single keyword char. For multi-char full-id the rule is
                    // just "char before must be non-keyword".
                    if is_keyword_char(ch_before, iskeyword) {
                        continue; // char before is keyword → lhs is part of a longer word
                    }
                    if lhs_len == 1 && ch_before != ' ' && ch_before != '\t' {
                        // single-char full-id: non-blank non-keyword before → skip
                        continue;
                    }
                }
                AbbrevKind::End => {
                    // end-id: no constraint on the char before (any char is fine,
                    // including keyword chars — the non-keyword prefix of the lhs
                    // acts as the boundary).
                }
                AbbrevKind::NonKw => {
                    // non-id: the char before the lhs must be blank (space/tab) or
                    // it must be the start of the typed portion (mincol boundary).
                    if ch_before != ' ' && ch_before != '\t' {
                        continue;
                    }
                }
            }
        }
        // lhs_start_col == 0 means the lhs starts at the very beginning of the
        // line (or at the insert-start position); all types accept this.

        return Some((lhs_len, abbrev.rhs.clone()));
    }

    None
}

/// Check abbreviations and apply the expansion if a match is found.
///
/// Reads the current cursor position and the text before it, calls
/// `try_abbrev_expand`, and if a match is found, deletes the `lhs` chars
/// and inserts the `rhs`. Returns `true` if an expansion was applied.
///
/// `trigger` is what the user did; the trigger char itself is NOT inserted
/// here — the caller inserts it (or not, in the case of `C-]`).
pub(crate) fn check_and_apply_abbrev<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    trigger: AbbrevTrigger,
) -> bool {
    use hjkl_buffer::{Edit, Position};

    // Collect the data we need without holding borrows.
    let cursor = buf_cursor_pos(&ed.buffer);
    let row = cursor.row;
    let col = cursor.col;
    let line_before: String = {
        let line = buf_line(&ed.buffer, row).unwrap_or_default();
        line.chars().take(col).collect()
    };
    let (mincol, on_start_row) = if let Some(ref s) = ed.vim.insert_session {
        if row == s.start_row {
            (s.start_col, true)
        } else {
            (0, false)
        }
    } else {
        (0, false)
    };
    // If cursor is before the insert start column on the same row, no lhs possible.
    if on_start_row && col <= mincol {
        return false;
    }

    let iskeyword = ed.settings.iskeyword.clone();
    let abbrevs = ed.vim.abbrevs.clone();

    let Some((lhs_len, rhs)) =
        try_abbrev_expand(&abbrevs, &line_before, mincol, trigger, &iskeyword)
    else {
        return false;
    };

    // Delete `lhs_len` chars before the cursor.
    let lhs_start = col.saturating_sub(lhs_len);
    if lhs_len > 0 {
        ed.mutate_edit(Edit::DeleteRange {
            start: Position::new(row, lhs_start),
            end: Position::new(row, col),
            kind: hjkl_buffer::MotionKind::Char,
        });
    }

    // Insert rhs at the (now updated) cursor position.
    let insert_pos = Position::new(row, lhs_start);
    if !rhs.is_empty() {
        ed.mutate_edit(Edit::InsertStr {
            at: insert_pos,
            text: rhs.clone(),
        });
    }

    // Move cursor to end of inserted rhs.
    let new_col = lhs_start + rhs.chars().count();
    buf_set_cursor_rc(&mut ed.buffer, row, new_col);
    ed.push_buffer_cursor_to_textarea();

    true
}

fn word_text_object<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    inner: bool,
    big: bool,
) -> Option<((usize, usize), (usize, usize))> {
    let (row, col) = ed.cursor();
    let line = buf_line(&ed.buffer, row)?;
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let at = col.min(chars.len().saturating_sub(1));
    let classify = |c: char| -> u8 {
        if c.is_whitespace() {
            0
        } else if big || is_wordchar(c) {
            1
        } else {
            2
        }
    };
    let cls = classify(chars[at]);
    let mut start = at;
    while start > 0 && classify(chars[start - 1]) == cls {
        start -= 1;
    }
    let mut end = at;
    while end + 1 < chars.len() && classify(chars[end + 1]) == cls {
        end += 1;
    }
    // Byte-offset helpers.
    let char_byte = |i: usize| {
        if i >= chars.len() {
            line.len()
        } else {
            line.char_indices().nth(i).map(|(b, _)| b).unwrap_or(0)
        }
    };
    let mut start_col = char_byte(start);
    // Exclusive end: byte index of char AFTER the last-included char.
    let mut end_col = char_byte(end + 1);
    if !inner {
        // `aw` — include trailing whitespace; if there's no trailing ws, absorb leading ws.
        let mut t = end + 1;
        let mut included_trailing = false;
        while t < chars.len() && chars[t].is_whitespace() {
            included_trailing = true;
            t += 1;
        }
        if included_trailing {
            end_col = char_byte(t);
        } else {
            let mut s = start;
            while s > 0 && chars[s - 1].is_whitespace() {
                s -= 1;
            }
            start_col = char_byte(s);
        }
    }
    Some(((row, start_col), (row, end_col)))
}

fn quote_text_object<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    q: char,
    inner: bool,
) -> Option<((usize, usize), (usize, usize))> {
    let (row, col) = ed.cursor();
    let line = buf_line(&ed.buffer, row)?;
    let bytes = line.as_bytes();
    let q_byte = q as u8;
    // Find opening and closing quote on the same line.
    let mut positions: Vec<usize> = Vec::new();
    for (i, &b) in bytes.iter().enumerate() {
        if b == q_byte {
            positions.push(i);
        }
    }
    if positions.len() < 2 {
        return None;
    }
    let mut open_idx: Option<usize> = None;
    let mut close_idx: Option<usize> = None;
    for pair in positions.chunks(2) {
        if pair.len() < 2 {
            break;
        }
        if col >= pair[0] && col <= pair[1] {
            open_idx = Some(pair[0]);
            close_idx = Some(pair[1]);
            break;
        }
        if col < pair[0] {
            open_idx = Some(pair[0]);
            close_idx = Some(pair[1]);
            break;
        }
    }
    let open = open_idx?;
    let close = close_idx?;
    // End columns are *exclusive* — one past the last character to act on.
    if inner {
        if close <= open + 1 {
            return None;
        }
        Some(((row, open + 1), (row, close)))
    } else {
        // `da<q>` — "around" includes the surrounding whitespace on one
        // side: trailing whitespace if any exists after the closing quote;
        // otherwise leading whitespace before the opening quote. This
        // matches vim's `:help text-objects` behaviour and avoids leaving
        // a double-space when the quoted span sits mid-sentence.
        let after_close = close + 1; // byte index after closing quote
        if after_close < bytes.len() && bytes[after_close].is_ascii_whitespace() {
            // Eat trailing whitespace run.
            let mut end = after_close;
            while end < bytes.len() && bytes[end].is_ascii_whitespace() {
                end += 1;
            }
            Some(((row, open), (row, end)))
        } else if open > 0 && bytes[open - 1].is_ascii_whitespace() {
            // Eat leading whitespace run.
            let mut start = open;
            while start > 0 && bytes[start - 1].is_ascii_whitespace() {
                start -= 1;
            }
            Some(((row, start), (row, close + 1)))
        } else {
            Some(((row, open), (row, close + 1)))
        }
    }
}

fn bracket_text_object<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    open: char,
    inner: bool,
    count: usize,
) -> Option<(Pos, Pos, RangeKind)> {
    let close = match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        _ => return None,
    };
    let (row, col) = ed.cursor();
    let lines = rope_to_lines_vec(&crate::types::Query::rope(&ed.buffer));
    let lines = lines.as_slice();
    // If the cursor sits ON the closing bracket, vim anchors the pair to that
    // bracket: the close is at the cursor and the open is found by scanning
    // backward from just before it. Without this, `find_open_bracket` counts
    // the cursor's own close, increments depth, and skips past its matching
    // open — making `di}`/`di{`-on-`}` a silent no-op.
    let cursor_char = lines.get(row).and_then(|l| l.chars().nth(col));
    let (open_pos, close_pos) = if cursor_char == Some(close) {
        let open_pos = if col > 0 {
            find_open_bracket(lines, row, col - 1, open, close)
        } else if row > 0 {
            let pr = row - 1;
            let pc = lines[pr].chars().count().saturating_sub(1);
            find_open_bracket(lines, pr, pc, open, close)
        } else {
            None
        }?;
        (open_pos, (row, col))
    } else {
        // Walk backward from cursor to find unbalanced opening. When the
        // cursor isn't inside any pair, fall back to scanning forward for
        // the next opening bracket (targets.vim-style: `ci(` works when
        // cursor is before the `(` on the same line or below).
        let open_pos = find_open_bracket(lines, row, col, open, close)
            .or_else(|| find_next_open(lines, row, col, open))?;
        let close_pos = find_close_bracket(lines, open_pos.0, open_pos.1 + 1, open, close)?;
        (open_pos, close_pos)
    };
    // Count: `2i{` / `2a{` target the Nth enclosing pair. Expand outward from
    // the innermost pair, re-anchoring to each enclosing bracket in turn. Stop
    // early (and use the outermost found) if there aren't `count` levels.
    let (open_pos, close_pos) = {
        let (mut op, mut cp) = (open_pos, close_pos);
        for _ in 1..count.max(1) {
            let outer = if op.1 > 0 {
                find_open_bracket(lines, op.0, op.1 - 1, open, close)
            } else if op.0 > 0 {
                let pr = op.0 - 1;
                let pc = lines[pr].chars().count().saturating_sub(1);
                find_open_bracket(lines, pr, pc, open, close)
            } else {
                None
            };
            let Some(oo) = outer else { break };
            let Some(oc) = find_close_bracket(lines, oo.0, oo.1 + 1, open, close) else {
                break;
            };
            op = oo;
            cp = oc;
        }
        (op, cp)
    };
    // End positions are *exclusive*.
    if inner {
        // The inner region is the raw charwise span from just after `{` to just
        // before `}`. Returned as Exclusive: the VISUAL path uses it directly
        // (so `vi{` is charwise — `vi{d` → "{}"), while the OPERATOR path
        // (`di{`/`ci{`) applies vim's exclusive-motion adjustment in
        // `apply_op_with_text_object` to collapse a contentful multi-line block
        // to bare braces ("{\n}") or promote a clean one to linewise.
        // Inner start = position just after `{`. When `{` is the last char on
        // its line, the inner region begins at the start of the next line (so
        // the exclusive-motion adjustment can promote to linewise). `advance_pos`
        // stops at end-of-line, so wrap explicitly here.
        let open_line_len = lines[open_pos.0].chars().count();
        let inner_start = if open_pos.1 + 1 >= open_line_len && open_pos.0 + 1 < lines.len() {
            (open_pos.0 + 1, 0)
        } else {
            advance_pos(lines, open_pos)
        };
        // Empty inner (`{}` / `( )` degenerate) → empty range at the inner
        // start. `di{` then no-ops; `ci{` inserts at that point.
        if inner_start.0 > close_pos.0
            || (inner_start.0 == close_pos.0 && inner_start.1 >= close_pos.1)
        {
            return Some((inner_start, inner_start, RangeKind::Exclusive));
        }
        // Whitespace-only multi-line inner: vim's `di{` is a no-op and `ci{`
        // inserts at the inner start without deleting the whitespace. Model as
        // an empty range at the inner start. Detected when every char strictly
        // between the braces (excluding newlines) is a space/tab, and there is
        // at least one — an inner of only newlines (empty lines) does NOT count
        // and falls through to the normal collapse.
        if close_pos.0 > open_pos.0 {
            let mut saw_ws = false;
            let mut saw_other = false;
            for r in inner_start.0..=close_pos.0 {
                let line: Vec<char> = lines
                    .get(r)
                    .map(|l| l.chars().collect())
                    .unwrap_or_default();
                let from = if r == inner_start.0 { inner_start.1 } else { 0 };
                let to = if r == close_pos.0 {
                    close_pos.1
                } else {
                    line.len()
                };
                for &c in line
                    .iter()
                    .take(to.min(line.len()))
                    .skip(from.min(line.len()))
                {
                    if c == ' ' || c == '\t' {
                        saw_ws = true;
                    } else {
                        saw_other = true;
                    }
                }
            }
            if saw_ws && !saw_other {
                return Some((inner_start, inner_start, RangeKind::Exclusive));
            }
        }
        Some((inner_start, close_pos, RangeKind::Exclusive))
    } else {
        Some((
            open_pos,
            advance_pos(lines, close_pos),
            RangeKind::Exclusive,
        ))
    }
}

fn find_open_bracket(
    lines: &[String],
    row: usize,
    col: usize,
    open: char,
    close: char,
) -> Option<(usize, usize)> {
    let mut depth: i32 = 0;
    let mut r = row;
    let mut c = col as isize;
    loop {
        let cur = &lines[r];
        let chars: Vec<char> = cur.chars().collect();
        // Clamp `c` to the line length: callers may seed `col` past
        // EOL on virtual-cursor lines (e.g., insert mode after `o`)
        // so direct indexing would panic on empty / short lines.
        if (c as usize) >= chars.len() {
            c = chars.len() as isize - 1;
        }
        while c >= 0 {
            let ch = chars[c as usize];
            if ch == close {
                depth += 1;
            } else if ch == open {
                if depth == 0 {
                    return Some((r, c as usize));
                }
                depth -= 1;
            }
            c -= 1;
        }
        if r == 0 {
            return None;
        }
        r -= 1;
        c = lines[r].chars().count() as isize - 1;
    }
}

fn find_close_bracket(
    lines: &[String],
    row: usize,
    start_col: usize,
    open: char,
    close: char,
) -> Option<(usize, usize)> {
    let mut depth: i32 = 0;
    let mut r = row;
    let mut c = start_col;
    loop {
        let cur = &lines[r];
        let chars: Vec<char> = cur.chars().collect();
        while c < chars.len() {
            let ch = chars[c];
            if ch == open {
                depth += 1;
            } else if ch == close {
                if depth == 0 {
                    return Some((r, c));
                }
                depth -= 1;
            }
            c += 1;
        }
        if r + 1 >= lines.len() {
            return None;
        }
        r += 1;
        c = 0;
    }
}

/// Forward scan from `(row, col)` for the next occurrence of `open`.
/// Multi-line. Used by bracket text objects to support targets.vim-style
/// "search forward when not currently inside a pair" behaviour.
fn find_next_open(lines: &[String], row: usize, col: usize, open: char) -> Option<(usize, usize)> {
    let mut r = row;
    let mut c = col;
    while r < lines.len() {
        let chars: Vec<char> = lines[r].chars().collect();
        while c < chars.len() {
            if chars[c] == open {
                return Some((r, c));
            }
            c += 1;
        }
        r += 1;
        c = 0;
    }
    None
}

fn advance_pos(lines: &[String], pos: (usize, usize)) -> (usize, usize) {
    let (r, c) = pos;
    let line_len = lines[r].chars().count();
    if c < line_len {
        (r, c + 1)
    } else if r + 1 < lines.len() {
        (r + 1, 0)
    } else {
        pos
    }
}

fn paragraph_text_object<H: crate::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    inner: bool,
) -> Option<((usize, usize), (usize, usize))> {
    let (row, _) = ed.cursor();
    let rope = crate::types::Query::rope(&ed.buffer);
    let n_lines = rope.len_lines();
    if n_lines == 0 {
        return None;
    }
    // A paragraph is a run of non-blank lines.
    let is_blank = |r: usize| -> bool {
        if r >= n_lines {
            return true;
        }
        rope_line_to_str(&rope, r).trim().is_empty()
    };
    if is_blank(row) {
        return None;
    }
    let mut top = row;
    while top > 0 && !is_blank(top - 1) {
        top -= 1;
    }
    let mut bot = row;
    while bot + 1 < n_lines && !is_blank(bot + 1) {
        bot += 1;
    }
    // For `ap`, include one trailing blank line if present.
    if !inner && bot + 1 < n_lines && is_blank(bot + 1) {
        bot += 1;
    }
    let end_col = rope_line_to_str(&rope, bot).chars().count();
    Some(((top, 0), (bot, end_col)))
}

// ─── Individual commands ───────────────────────────────────────────────────

/// Read the text in a vim-shaped range without mutating. Used by
/// `Operator::Yank` so we can pipe the same range translation as
/// [`cut_vim_range`] but skip the delete + inverse extraction.
fn read_vim_range<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    start: (usize, usize),
    end: (usize, usize),
    kind: RangeKind,
) -> String {
    let (top, bot) = order(start, end);
    ed.sync_buffer_content_from_textarea();
    let rope = crate::types::Query::rope(&ed.buffer);
    let n_lines = rope.len_lines();
    match kind {
        RangeKind::Linewise => {
            let lo = top.0;
            let hi = bot.0.min(n_lines.saturating_sub(1));
            let mut text = rope_row_range_str(&rope, lo, hi);
            text.push('\n');
            text
        }
        RangeKind::Inclusive | RangeKind::Exclusive => {
            let inclusive = matches!(kind, RangeKind::Inclusive);
            // Walk row-by-row collecting chars in `[top, end_exclusive)`.
            let mut out = String::new();
            for row in top.0..=bot.0 {
                if row >= n_lines {
                    break;
                }
                let line = rope_line_to_str(&rope, row);
                let lo = if row == top.0 { top.1 } else { 0 };
                let hi_unclamped = if row == bot.0 {
                    if inclusive { bot.1 + 1 } else { bot.1 }
                } else {
                    line.chars().count() + 1
                };
                let row_chars: Vec<char> = line.chars().collect();
                let hi = hi_unclamped.min(row_chars.len());
                if lo < hi {
                    out.push_str(&row_chars[lo..hi].iter().collect::<String>());
                }
                if row < bot.0 {
                    out.push('\n');
                }
            }
            out
        }
    }
}

/// Cut a vim-shaped range through the Buffer edit funnel and return
/// the deleted text. Translates vim's `RangeKind`
/// (Linewise/Inclusive/Exclusive) into the buffer's
/// `hjkl_buffer::MotionKind` (Line/Char) and applies the right end-
/// position adjustment so inclusive motions actually include the bot
/// cell. Pushes the cut text into the clipboard via `record_yank_to_host`
/// and the textarea yank buffer (still observed by `p`/`P` until the paste
/// path is ported), and updates `yank_linewise` for linewise cuts.
fn cut_vim_range<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    start: (usize, usize),
    end: (usize, usize),
    kind: RangeKind,
) -> String {
    use hjkl_buffer::{Edit, MotionKind as BufKind, Position};
    let (top, bot) = order(start, end);
    ed.sync_buffer_content_from_textarea();
    let (buf_start, buf_end, buf_kind) = match kind {
        RangeKind::Linewise => (
            Position::new(top.0, 0),
            Position::new(bot.0, 0),
            BufKind::Line,
        ),
        RangeKind::Inclusive => {
            let line_chars = buf_line_chars(&ed.buffer, bot.0);
            // Advance one cell past `bot` so the buffer's exclusive
            // `cut_chars` actually drops the inclusive endpoint. Wrap
            // to the next row when bot already sits on the last char.
            let next = if bot.1 < line_chars {
                Position::new(bot.0, bot.1 + 1)
            } else if bot.0 + 1 < buf_row_count(&ed.buffer) {
                Position::new(bot.0 + 1, 0)
            } else {
                Position::new(bot.0, line_chars)
            };
            (Position::new(top.0, top.1), next, BufKind::Char)
        }
        RangeKind::Exclusive => (
            Position::new(top.0, top.1),
            Position::new(bot.0, bot.1),
            BufKind::Char,
        ),
    };
    let inverse = ed.mutate_edit(Edit::DeleteRange {
        start: buf_start,
        end: buf_end,
        kind: buf_kind,
    });
    let text = match inverse {
        Edit::InsertStr { text, .. } => text,
        _ => String::new(),
    };
    if !text.is_empty() {
        ed.record_yank_to_host(text.clone());
        ed.record_delete(text.clone(), matches!(kind, RangeKind::Linewise));
    }
    ed.push_buffer_cursor_to_textarea();
    text
}

/// `D` / `C` — delete from cursor to end of line through the edit
/// funnel. Pushes the deleted text to the clipboard via `record_yank_to_host`
/// and the textarea's yank buffer (still observed by `p`/`P` until the paste
/// path is ported). Cursor lands at the deletion start so the caller
/// can decide whether to step it left (`D`) or open insert mode (`C`).
fn delete_to_eol<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(&ed.buffer);
    let line_chars = buf_line_chars(&ed.buffer, cursor.row);
    if cursor.col >= line_chars {
        return;
    }
    let inverse = ed.mutate_edit(Edit::DeleteRange {
        start: cursor,
        end: Position::new(cursor.row, line_chars),
        kind: MotionKind::Char,
    });
    if let Edit::InsertStr { text, .. } = inverse
        && !text.is_empty()
    {
        ed.record_yank_to_host(text.clone());
        ed.vim.yank_linewise = false;
        ed.set_yank(text);
    }
    buf_set_cursor_pos(&mut ed.buffer, cursor);
    ed.push_buffer_cursor_to_textarea();
}

fn do_char_delete<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    forward: bool,
    count: usize,
) {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.push_undo();
    ed.sync_buffer_content_from_textarea();
    // Collect deleted chars so we can write them to the unnamed register
    // (vim's `x`/`X` populate `"` so that `xp` round-trips the char).
    let mut deleted = String::new();
    for _ in 0..count {
        let cursor = buf_cursor_pos(&ed.buffer);
        let line_chars = buf_line_chars(&ed.buffer, cursor.row);
        if forward {
            // `x` — delete the char under the cursor. Vim no-ops on
            // an empty line; the buffer would drop a row otherwise.
            if cursor.col >= line_chars {
                continue;
            }
            let inverse = ed.mutate_edit(Edit::DeleteRange {
                start: cursor,
                end: Position::new(cursor.row, cursor.col + 1),
                kind: MotionKind::Char,
            });
            if let Edit::InsertStr { text, .. } = inverse {
                deleted.push_str(&text);
            }
        } else {
            // `X` — delete the char before the cursor.
            if cursor.col == 0 {
                continue;
            }
            let inverse = ed.mutate_edit(Edit::DeleteRange {
                start: Position::new(cursor.row, cursor.col - 1),
                end: cursor,
                kind: MotionKind::Char,
            });
            if let Edit::InsertStr { text, .. } = inverse {
                // X deletes backwards; prepend so the register text
                // matches reading order (first deleted char first).
                deleted = text + &deleted;
            }
        }
    }
    if !deleted.is_empty() {
        ed.record_yank_to_host(deleted.clone());
        ed.record_delete(deleted, false);
    }
    ed.push_buffer_cursor_to_textarea();
}

/// Vim `Ctrl-a` / `Ctrl-x` — find the next number at or after the cursor on the
/// current line, add `delta`, leave the cursor on the last digit of the result.
/// Recognises `0x`/`0X` hex literals (incremented in hex, width preserved) as
/// well as signed decimals. No-op if the line has no number to the right.
pub(crate) fn adjust_number<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    delta: i64,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(&ed.buffer);
    let row = cursor.row;
    let chars: Vec<char> = match buf_line(&ed.buffer, row) {
        Some(l) => l.chars().collect(),
        None => return false,
    };
    let len = chars.len();

    // Scan from the cursor for the start of the leftmost number — a `0x`/`0X`
    // hex literal takes priority over a bare decimal at the same position.
    let is_hex_prefix = |i: usize| {
        chars[i] == '0'
            && i + 1 < len
            && matches!(chars[i + 1], 'x' | 'X')
            && chars.get(i + 2).is_some_and(|c| c.is_ascii_hexdigit())
    };
    let mut i = cursor.col;
    let mut hex = false;
    loop {
        if i >= len {
            return false;
        }
        if is_hex_prefix(i) {
            hex = true;
            break;
        }
        if chars[i].is_ascii_digit() {
            break;
        }
        i += 1;
    }

    let (span_start, span_end, new_s) = if hex {
        // `0x` + hex digits. Increment the value, preserve the digit width.
        let digits_start = i + 2;
        let mut digits_end = digits_start;
        while digits_end < len && chars[digits_end].is_ascii_hexdigit() {
            digits_end += 1;
        }
        let hexs: String = chars[digits_start..digits_end].iter().collect();
        let Ok(n) = u64::from_str_radix(&hexs, 16) else {
            return false;
        };
        let new_val = (n as i128 + delta as i128).max(0) as u64;
        let width = digits_end - digits_start;
        let prefix: String = chars[i..digits_start].iter().collect();
        (i, digits_end, format!("{prefix}{new_val:0width$x}"))
    } else {
        // Signed decimal.
        let digit_start = i;
        let span_start = if digit_start > 0 && chars[digit_start - 1] == '-' {
            digit_start - 1
        } else {
            digit_start
        };
        let mut span_end = digit_start;
        while span_end < len && chars[span_end].is_ascii_digit() {
            span_end += 1;
        }
        let s: String = chars[span_start..span_end].iter().collect();
        let Ok(n) = s.parse::<i64>() else {
            return false;
        };
        (span_start, span_end, n.saturating_add(delta).to_string())
    };

    ed.push_undo();
    let span_start_pos = Position::new(row, span_start);
    let span_end_pos = Position::new(row, span_end);
    ed.mutate_edit(Edit::DeleteRange {
        start: span_start_pos,
        end: span_end_pos,
        kind: MotionKind::Char,
    });
    ed.mutate_edit(Edit::InsertStr {
        at: span_start_pos,
        text: new_s.clone(),
    });
    let new_len = new_s.chars().count();
    buf_set_cursor_rc(&mut ed.buffer, row, span_start + new_len.saturating_sub(1));
    ed.push_buffer_cursor_to_textarea();
    true
}

pub(crate) fn replace_char<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    ch: char,
    count: usize,
) {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.push_undo();
    ed.sync_buffer_content_from_textarea();
    for _ in 0..count {
        let cursor = buf_cursor_pos(&ed.buffer);
        let line_chars = buf_line_chars(&ed.buffer, cursor.row);
        if cursor.col >= line_chars {
            break;
        }
        ed.mutate_edit(Edit::DeleteRange {
            start: cursor,
            end: Position::new(cursor.row, cursor.col + 1),
            kind: MotionKind::Char,
        });
        ed.mutate_edit(Edit::InsertChar { at: cursor, ch });
    }
    // Vim leaves the cursor on the last replaced char.
    crate::motions::move_left(&mut ed.buffer, 1);
    ed.push_buffer_cursor_to_textarea();
}

fn toggle_case_at_cursor<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(&ed.buffer);
    let Some(c) = buf_line(&ed.buffer, cursor.row).and_then(|l| l.chars().nth(cursor.col)) else {
        return;
    };
    let toggled = if c.is_uppercase() {
        c.to_lowercase().next().unwrap_or(c)
    } else {
        c.to_uppercase().next().unwrap_or(c)
    };
    ed.mutate_edit(Edit::DeleteRange {
        start: cursor,
        end: Position::new(cursor.row, cursor.col + 1),
        kind: MotionKind::Char,
    });
    ed.mutate_edit(Edit::InsertChar {
        at: cursor,
        ch: toggled,
    });
}

fn join_line<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    use hjkl_buffer::{Edit, Position};
    ed.sync_buffer_content_from_textarea();
    let row = buf_cursor_pos(&ed.buffer).row;
    if row + 1 >= buf_row_count(&ed.buffer) {
        return;
    }
    let cur_line = buf_line(&ed.buffer, row).unwrap_or_default();
    let next_raw = buf_line(&ed.buffer, row + 1).unwrap_or_default();
    let next_trimmed = next_raw.trim_start();
    let cur_chars = cur_line.chars().count();
    let next_chars = next_raw.chars().count();
    // `J` inserts a single space iff both sides are non-empty after
    // stripping the next line's leading whitespace.
    let separator = if !cur_line.is_empty() && !next_trimmed.is_empty() {
        " "
    } else {
        ""
    };
    let joined = format!("{cur_line}{separator}{next_trimmed}");
    ed.mutate_edit(Edit::Replace {
        start: Position::new(row, 0),
        end: Position::new(row + 1, next_chars),
        with: joined,
    });
    // Vim parks the cursor on the inserted space — or at the join
    // point when no space went in (which is the same column either
    // way, since the space sits exactly at `cur_chars`).
    buf_set_cursor_rc(&mut ed.buffer, row, cur_chars);
    ed.push_buffer_cursor_to_textarea();
}

/// `gJ` — join the next line onto the current one without inserting a
/// separating space or stripping leading whitespace.
fn join_line_raw<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    use hjkl_buffer::Edit;
    ed.sync_buffer_content_from_textarea();
    let row = buf_cursor_pos(&ed.buffer).row;
    if row + 1 >= buf_row_count(&ed.buffer) {
        return;
    }
    let join_col = buf_line_chars(&ed.buffer, row);
    ed.mutate_edit(Edit::JoinLines {
        row,
        count: 1,
        with_space: false,
    });
    // Vim leaves the cursor at the join point (end of original line).
    buf_set_cursor_rc(&mut ed.buffer, row, join_col);
    ed.push_buffer_cursor_to_textarea();
}

/// Visual-mode `J` (`with_space = true`) / `gJ` (`with_space = false`) — join
/// every line spanned by the selection into one. A single-line selection joins
/// the current line with the one below (matching normal-mode `J`).
pub(crate) fn visual_join<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    with_space: bool,
) {
    let cursor_row = buf_cursor_pos(&ed.buffer).row;
    let (top, bot) = match ed.vim.mode {
        Mode::VisualLine => (
            cursor_row.min(ed.vim.visual_line_anchor),
            cursor_row.max(ed.vim.visual_line_anchor),
        ),
        Mode::VisualBlock => {
            let a = ed.vim.block_anchor.0;
            (a.min(cursor_row), a.max(cursor_row))
        }
        Mode::Visual => {
            let a = ed.vim.visual_anchor.0;
            (a.min(cursor_row), a.max(cursor_row))
        }
        _ => return,
    };
    // N selected lines → N-1 joins; a single line still does one join (with the
    // line below) like normal-mode `J`.
    let joins = (bot - top).max(1);
    ed.push_undo();
    buf_set_cursor_rc(&mut ed.buffer, top, 0);
    ed.push_buffer_cursor_to_textarea();
    for _ in 0..joins {
        if with_space {
            join_line(ed);
        } else {
            join_line_raw(ed);
        }
    }
    ed.vim.mode = Mode::Normal;
    ed.sticky_col = Some(buf_cursor_pos(&ed.buffer).col);
}

/// `[count]%` — go to the line at `count` percent of the file (vim: line
/// `(count * line_count + 99) / 100`), cursor on the first non-blank.
pub(crate) fn goto_percent<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    count: usize,
) {
    let rows = buf_row_count(&ed.buffer);
    if rows == 0 {
        return;
    }
    // Exclude the phantom trailing empty line (a file ending in `\n` is N lines
    // in vim, not N+1) so the percentage matches nvim.
    let total = if rows >= 2
        && buf_line(&ed.buffer, rows - 1)
            .map(|s| s.is_empty())
            .unwrap_or(false)
    {
        rows - 1
    } else {
        rows
    };
    // 1-based target line, clamped to the buffer (vim: ceil(count*lines/100)).
    let line = (count * total).div_ceil(100).clamp(1, total);
    let pre = ed.cursor();
    ed.jump_cursor(line - 1, 0);
    move_first_non_whitespace(ed);
    ed.sticky_col = Some(ed.cursor().1);
    if ed.cursor() != pre {
        ed.push_jump(pre);
    }
}

/// Indent width of a leading-whitespace prefix, counting a `\t` as advancing
/// to the next `tabstop` boundary and a space as one column.
fn indent_width(s: &str, tabstop: usize) -> usize {
    let ts = tabstop.max(1);
    let mut w = 0usize;
    for c in s.chars() {
        match c {
            ' ' => w += 1,
            '\t' => w += ts - (w % ts),
            _ => break,
        }
    }
    w
}

/// Build a leading-whitespace string of `width` columns honoring `expandtab`
/// (spaces) vs `noexpandtab` (tabs for full `tabstop` runs, spaces remainder).
fn build_indent(width: usize, settings: &crate::editor::Settings) -> String {
    if settings.expandtab {
        return " ".repeat(width);
    }
    let ts = settings.tabstop.max(1);
    let tabs = width / ts;
    let spaces = width % ts;
    format!("{}{}", "\t".repeat(tabs), " ".repeat(spaces))
}

/// `]p` / `[p` reindent: shift every line of `text` so the FIRST line's indent
/// matches `target_width` columns; later lines keep their relative offset.
fn reindent_block(text: &str, target_width: usize, settings: &crate::editor::Settings) -> String {
    let ts = settings.tabstop.max(1);
    let lines: Vec<&str> = text.split('\n').collect();
    let first_width = lines.first().map(|l| indent_width(l, ts)).unwrap_or(0);
    let delta = target_width as isize - first_width as isize;
    lines
        .iter()
        .map(|line| {
            let trimmed = line.trim_start_matches([' ', '\t']);
            if trimmed.is_empty() {
                // Preserve blank lines as truly empty (vim does not indent them).
                return String::new();
            }
            let old_w = indent_width(line, ts) as isize;
            let new_w = (old_w + delta).max(0) as usize;
            format!("{}{}", build_indent(new_w, settings), trimmed)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn do_paste<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    before: bool,
    count: usize,
    cursor_after: bool,
    reindent: bool,
) {
    use hjkl_buffer::{Edit, Position};
    ed.push_undo();
    // Resolve the source register: `"reg` prefix (consumed) or the
    // unnamed register otherwise. Read text + linewise from the
    // selected slot rather than the global `vim.yank_linewise` so
    // pasting from `"0` after a delete still uses the yank's layout.
    let selector = ed.vim.pending_register.take();
    let (yank, linewise) = match selector.and_then(|c| ed.registers().read(c)) {
        Some(slot) => (slot.text.clone(), slot.linewise),
        // Read both fields from the unnamed slot rather than mixing the
        // slot's text with `vim.yank_linewise`. The cached vim flag is
        // per-editor, so a register imported from another editor (e.g.
        // cross-buffer yank/paste) carried the wrong linewise without
        // this — pasting a linewise yank inserted at the char cursor.
        None => {
            let s = &ed.registers().unnamed;
            (s.text.clone(), s.linewise)
        }
    };
    // Vim `:h '[` / `:h ']`: after paste `[` = first inserted char of
    // the final paste, `]` = last inserted char of the final paste.
    // We track (lo, hi) across iterations; the last value wins.
    let mut paste_mark: Option<((usize, usize), (usize, usize))> = None;
    // Capture the cursor row before any paste iterations. Vim's
    // linewise `[count]p` lands the cursor on the FIRST pasted line
    // (original_row + 1), not on the last iteration's paste row.
    // Without this snapshot the per-iteration cursor advancement leaves
    // the cursor at `original_row + count` instead.
    let original_row_for_linewise_after = if linewise && !before {
        // Fold-aware: `p` on a closed fold pastes after the fold, so the first
        // pasted line is `fold_end + 1`, not `cursor_row + 1`.
        let r = buf_cursor_pos(&ed.buffer).row;
        let (_, fold_end) = expand_linewise_over_closed_folds(&ed.buffer, r, r);
        Some(fold_end)
    } else {
        None
    };
    for _ in 0..count {
        ed.sync_buffer_content_from_textarea();
        let yank = yank.clone();
        if yank.is_empty() {
            continue;
        }
        if linewise {
            // Linewise paste: insert payload as fresh row(s) above
            // (`P`) or below (`p`) the cursor's row. Cursor lands on
            // the first non-blank of the first pasted line.
            let mut text = yank.trim_matches('\n').to_string();
            let row = buf_cursor_pos(&ed.buffer).row;
            // `]p` / `[p` — reindent the pasted block to the current line.
            if reindent {
                let cur_line = buf_line(&ed.buffer, row).unwrap_or_default();
                let target_w = indent_width(&cur_line, ed.settings.tabstop.max(1));
                text = reindent_block(&text, target_w, &ed.settings);
            }
            // Fold-aware: linewise paste lands relative to the whole CLOSED
            // fold, not just the cursor line — `p` after the fold's last row,
            // `P` before its first row (vim behaviour). No fold → unchanged.
            let (fold_start, fold_end) = expand_linewise_over_closed_folds(&ed.buffer, row, row);
            let target_row = if before {
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(fold_start, 0),
                    text: format!("{text}\n"),
                });
                fold_start
            } else {
                let line_chars = buf_line_chars(&ed.buffer, fold_end);
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(fold_end, line_chars),
                    text: format!("\n{text}"),
                });
                fold_end + 1
            };
            buf_set_cursor_rc(&mut ed.buffer, target_row, 0);
            crate::motions::move_first_non_blank(&mut ed.buffer);
            ed.push_buffer_cursor_to_textarea();
            // Linewise: `[` = (target_row, 0), `]` = (bot_row, last_col).
            let payload_lines = text.lines().count().max(1);
            let bot_row = target_row + payload_lines - 1;
            let bot_last_col = buf_line_chars(&ed.buffer, bot_row).saturating_sub(1);
            paste_mark = Some(((target_row, 0), (bot_row, bot_last_col)));
        } else {
            // Charwise paste. `P` inserts at cursor (shifting cell
            // right); `p` inserts after cursor (advance one cell
            // first, clamped to the end of the line).
            let cursor = buf_cursor_pos(&ed.buffer);
            let at = if before {
                cursor
            } else {
                let line_chars = buf_line_chars(&ed.buffer, cursor.row);
                Position::new(cursor.row, (cursor.col + 1).min(line_chars))
            };
            ed.mutate_edit(Edit::InsertStr {
                at,
                text: yank.clone(),
            });
            // Vim parks the cursor on the last char of the pasted text
            // (do_insert_str leaves it one past the end). `gp` instead
            // leaves the cursor just AFTER the pasted text, so skip the
            // step-back there.
            if !cursor_after && ed.cursor().1 > 0 {
                crate::motions::move_left(&mut ed.buffer, 1);
                ed.push_buffer_cursor_to_textarea();
            }
            // Charwise: `[` = insert start, `]` = last pasted char.
            let lo = (at.row, at.col);
            let hi = if cursor_after {
                let c = ed.cursor();
                (c.0, c.1.saturating_sub(1))
            } else {
                ed.cursor()
            };
            paste_mark = Some((lo, hi));
        }
    }
    if let Some((lo, hi)) = paste_mark {
        ed.set_mark('[', lo);
        ed.set_mark(']', hi);
    }
    // `gp` / `gP` linewise: cursor lands on the line just AFTER the pasted
    // block (the `]` mark's row + 1), at column 0, clamped to the last row.
    if cursor_after && linewise {
        if let Some((_, (bot_row, _))) = paste_mark {
            let last_row = buf_row_count(&ed.buffer).saturating_sub(1);
            let target = (bot_row + 1).min(last_row);
            buf_set_cursor_rc(&mut ed.buffer, target, 0);
            ed.push_buffer_cursor_to_textarea();
        }
    } else if let Some(orig_row) = original_row_for_linewise_after {
        // Linewise `p` (after) with count: cursor lands on the FIRST pasted
        // line (original_row + 1) — vim parity. The per-iteration loop
        // moves cursor to each paste's target_row, so without this reset
        // `5p` would land at original_row + 5 instead of original_row + 1.
        let first_target = orig_row.saturating_add(1);
        buf_set_cursor_rc(&mut ed.buffer, first_target, 0);
        crate::motions::move_first_non_blank(&mut ed.buffer);
        ed.push_buffer_cursor_to_textarea();
    }
    // Any paste re-anchors the sticky column to the new cursor position.
    ed.sticky_col = Some(buf_cursor_pos(&ed.buffer).col);
}

/// Visual-mode `p` / `P` — replace the active selection with the register.
/// With `p` the deleted selection lands in the unnamed register (vim's swap);
/// with `P` (`before = true`) the source register is preserved so it can be
/// pasted over multiple selections in turn.
pub(crate) fn visual_paste<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    before: bool,
) {
    use hjkl_buffer::{Edit, Position};
    ed.sync_buffer_content_from_textarea();

    // Resolve the source register (selector or unnamed) BEFORE the delete
    // overwrites the unnamed register with the cut selection.
    let selector = ed.vim.pending_register.take();
    let (reg_text, reg_linewise) = match selector.and_then(|c| ed.registers().read(c)) {
        Some(slot) => (slot.text.clone(), slot.linewise),
        None => {
            let s = &ed.registers().unnamed;
            (s.text.clone(), s.linewise)
        }
    };
    // For `P`, snapshot the unnamed register so we can restore it afterwards.
    let saved_unnamed = before.then(|| ed.registers().unnamed.clone());

    let mode = ed.vim.mode;
    ed.push_undo();

    match mode {
        Mode::VisualLine => {
            let cursor_row = buf_cursor_pos(&ed.buffer).row;
            let top = cursor_row.min(ed.vim.visual_line_anchor);
            let bot = cursor_row.max(ed.vim.visual_line_anchor);
            // Delete the selected lines into the unnamed register.
            cut_vim_range(ed, (top, 0), (bot, 0), RangeKind::Linewise);
            // Insert the register as fresh line(s) where the selection was.
            let text = reg_text.trim_matches('\n').to_string();
            let line_count = buf_row_count(&ed.buffer);
            if top >= line_count {
                // Selection reached the end of the buffer: append below the
                // (new) last line.
                let last = line_count.saturating_sub(1);
                let lc = buf_line_chars(&ed.buffer, last);
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(last, lc),
                    text: format!("\n{text}"),
                });
                buf_set_cursor_rc(&mut ed.buffer, last + 1, 0);
            } else {
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(top, 0),
                    text: format!("{text}\n"),
                });
                buf_set_cursor_rc(&mut ed.buffer, top, 0);
            }
            crate::motions::move_first_non_blank(&mut ed.buffer);
            ed.push_buffer_cursor_to_textarea();
        }
        Mode::Visual | Mode::VisualBlock => {
            let anchor = if mode == Mode::VisualBlock {
                ed.vim.block_anchor
            } else {
                ed.vim.visual_anchor
            };
            let cursor = ed.cursor();
            let (top, bot) = order(anchor, cursor);
            // Delete the selection into the unnamed register.
            cut_vim_range(ed, top, bot, RangeKind::Inclusive);
            // Insert the register text where the selection started.
            if reg_linewise {
                // Linewise register into a charwise hole: open a line below.
                let text = reg_text.trim_matches('\n').to_string();
                let lc = buf_line_chars(&ed.buffer, top.0);
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(top.0, lc),
                    text: format!("\n{text}"),
                });
                buf_set_cursor_rc(&mut ed.buffer, top.0 + 1, 0);
                crate::motions::move_first_non_blank(&mut ed.buffer);
            } else {
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(top.0, top.1),
                    text: reg_text.clone(),
                });
                // Park the cursor on the last char of the inserted text.
                let inserted_len = reg_text.chars().count();
                let last_col = top.1 + inserted_len.saturating_sub(1);
                buf_set_cursor_rc(&mut ed.buffer, top.0, last_col);
            }
            ed.push_buffer_cursor_to_textarea();
        }
        _ => {}
    }

    // `P` preserves the source register; restore the snapshot.
    if let Some(slot) = saved_unnamed {
        ed.registers_mut().unnamed = slot;
    }
    ed.vim.mode = Mode::Normal;
    ed.sticky_col = Some(buf_cursor_pos(&ed.buffer).col);
}

/// Visual-mode `<C-a>` / `<C-x>` and `g<C-a>` / `g<C-x>`. Adds `delta` to the
/// first number on each selected line. When `sequential` is true the increment
/// grows by `delta` for each successive number found (vim's `g<C-a>`): the
/// first gets `delta`, the second `2*delta`, and so on.
pub(crate) fn adjust_number_visual<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    delta: i64,
    sequential: bool,
) {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let mode = ed.vim.mode;
    let cursor = buf_cursor_pos(&ed.buffer);

    // Resolve the row range + the per-row start column to scan from.
    let (top, bot, mut scan_col_first, block_left) = match mode {
        Mode::VisualLine => {
            let t = cursor.row.min(ed.vim.visual_line_anchor);
            let b = cursor.row.max(ed.vim.visual_line_anchor);
            (t, b, 0usize, None)
        }
        Mode::Visual => {
            let (a, c) = order(ed.vim.visual_anchor, (cursor.row, cursor.col));
            (a.0, c.0, a.1, None)
        }
        Mode::VisualBlock => {
            let (a, c) = order(ed.vim.block_anchor, (cursor.row, cursor.col));
            let left = a.1.min(c.1);
            (a.0, c.0, left, Some(left))
        }
        _ => return,
    };

    ed.push_undo();
    let mut found_count: i64 = 0;
    for row in top..=bot {
        let start_col = match block_left {
            Some(left) => left,
            None => {
                // First row of a charwise selection starts at the anchor/cursor
                // column; subsequent rows start at column 0.
                let c = if row == top { scan_col_first } else { 0 };
                scan_col_first = 0;
                c
            }
        };
        let chars: Vec<char> = match buf_line(&ed.buffer, row) {
            Some(l) => l.chars().collect(),
            None => continue,
        };
        let Some(digit_start) =
            (start_col.min(chars.len())..chars.len()).find(|&i| chars[i].is_ascii_digit())
        else {
            continue;
        };
        let span_start = if digit_start > 0 && chars[digit_start - 1] == '-' {
            digit_start - 1
        } else {
            digit_start
        };
        let mut span_end = digit_start;
        while span_end < chars.len() && chars[span_end].is_ascii_digit() {
            span_end += 1;
        }
        let s: String = chars[span_start..span_end].iter().collect();
        let Ok(n) = s.parse::<i64>() else {
            continue;
        };
        found_count += 1;
        let this_delta = if sequential {
            delta.saturating_mul(found_count)
        } else {
            delta
        };
        let new_s = n.saturating_add(this_delta).to_string();
        let span_start_pos = Position::new(row, span_start);
        let span_end_pos = Position::new(row, span_end);
        ed.mutate_edit(Edit::DeleteRange {
            start: span_start_pos,
            end: span_end_pos,
            kind: MotionKind::Char,
        });
        ed.mutate_edit(Edit::InsertStr {
            at: span_start_pos,
            text: new_s,
        });
    }
    // Vim leaves the cursor at the start of the selection.
    buf_set_cursor_rc(&mut ed.buffer, top, block_left.unwrap_or(0));
    ed.push_buffer_cursor_to_textarea();
    ed.vim.mode = Mode::Normal;
    ed.sticky_col = Some(buf_cursor_pos(&ed.buffer).col);
}

pub(crate) fn do_undo<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    if let Some(entry) = ed.buffer.pop_undo_entry() {
        let (cur_rope, cur_cursor) = ed.snapshot();
        ed.buffer.push_redo_entry(hjkl_buffer::UndoEntry {
            rope: cur_rope,
            cursor: cur_cursor,
            timestamp: entry.timestamp,
        });
        ed.restore_rope(entry.rope, entry.cursor);
    }
    ed.vim.mode = Mode::Normal;
    // The restored cursor came from a snapshot taken in insert mode
    // (before the insert started) and may be past the last valid
    // normal-mode column. Clamp it now, same as Esc-from-insert does.
    clamp_cursor_to_normal_mode(ed);
}

pub(crate) fn do_redo<H: crate::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    if let Some(entry) = ed.buffer.pop_redo_entry() {
        let (cur_rope, cur_cursor) = ed.snapshot();
        let before = cur_rope.clone();
        ed.buffer.push_undo_entry(hjkl_buffer::UndoEntry {
            rope: cur_rope,
            cursor: cur_cursor,
            timestamp: entry.timestamp,
        });
        ed.cap_undo();
        ed.restore_rope(entry.rope, entry.cursor);
        // vim parks the cursor at the START of the reapplied change, not the
        // end-of-insert position stored in the redo snapshot. Recompute it from
        // the first character that differs between the pre- and post-redo text.
        let after = crate::types::Query::rope(&ed.buffer);
        if let Some((row, col)) = first_diff_pos(&before, &after) {
            buf_set_cursor_rc(&mut ed.buffer, row, col);
            ed.push_buffer_cursor_to_textarea();
        }
    }
    ed.vim.mode = Mode::Normal;
    clamp_cursor_to_normal_mode(ed);
}

/// First `(row, col)` where two ropes differ, or `None` if identical. Used to
/// place the cursor at the start of a redone change (vim parity).
fn first_diff_pos(a: &ropey::Rope, b: &ropey::Rope) -> Option<(usize, usize)> {
    let rows = a.len_lines().max(b.len_lines());
    for r in 0..rows {
        let la = if r < a.len_lines() {
            hjkl_buffer::rope_line_str(a, r)
        } else {
            String::new()
        };
        let lb = if r < b.len_lines() {
            hjkl_buffer::rope_line_str(b, r)
        } else {
            String::new()
        };
        if la != lb {
            let col = la
                .chars()
                .zip(lb.chars())
                .take_while(|(x, y)| x == y)
                .count();
            return Some((r, col));
        }
    }
    None
}

// ─── Dot repeat ────────────────────────────────────────────────────────────

/// Replay-side helper: insert `text` at the cursor through the
/// edit funnel, then leave insert mode (the original change ended
/// with Esc, so the dot-repeat must end the same way — including
/// the cursor step-back vim does on Esc-from-insert).
fn replay_insert_and_finish<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    text: &str,
) {
    use hjkl_buffer::{Edit, Position};
    let cursor = ed.cursor();
    ed.mutate_edit(Edit::InsertStr {
        at: Position::new(cursor.0, cursor.1),
        text: text.to_string(),
    });
    if ed.vim.insert_session.take().is_some() {
        if ed.cursor().1 > 0 {
            crate::motions::move_left(&mut ed.buffer, 1);
            ed.push_buffer_cursor_to_textarea();
        }
        ed.vim.mode = Mode::Normal;
    }
}

pub(crate) fn replay_last_change<H: crate::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    outer_count: usize,
) {
    let Some(change) = ed.vim.last_change.clone() else {
        return;
    };
    ed.vim.replaying = true;
    let scale = if outer_count > 0 { outer_count } else { 1 };
    match change {
        LastChange::OpMotion {
            op,
            motion,
            count,
            inserted,
        } => {
            let total = count.max(1) * scale;
            apply_op_with_motion(ed, op, &motion, total);
            if let Some(text) = inserted {
                replay_insert_and_finish(ed, &text);
            }
        }
        LastChange::OpTextObj {
            op,
            obj,
            inner,
            inserted,
        } => {
            // Dot-repeat replays the text object at count 1 (the original
            // count is not retained in `LastChange::OpTextObj`).
            apply_op_with_text_object(ed, op, obj, inner, 1);
            if let Some(text) = inserted {
                replay_insert_and_finish(ed, &text);
            }
        }
        LastChange::LineOp {
            op,
            count,
            inserted,
        } => {
            let total = count.max(1) * scale;
            execute_line_op(ed, op, total);
            if let Some(text) = inserted {
                replay_insert_and_finish(ed, &text);
            }
        }
        LastChange::CharDel { forward, count } => {
            do_char_delete(ed, forward, count * scale);
        }
        LastChange::ReplaceChar { ch, count } => {
            replace_char(ed, ch, count * scale);
        }
        LastChange::ToggleCase { count } => {
            for _ in 0..count * scale {
                ed.push_undo();
                toggle_case_at_cursor(ed);
            }
        }
        LastChange::JoinLine { count } => {
            for _ in 0..count * scale {
                ed.push_undo();
                join_line(ed);
            }
        }
        LastChange::Paste {
            before,
            count,
            cursor_after,
            reindent,
        } => {
            do_paste(ed, before, count * scale, cursor_after, reindent);
        }
        LastChange::GnOp {
            op,
            forward,
            inserted,
        } => {
            gn_operate(ed, Some(op), forward, 1);
            if let Some(text) = inserted {
                replay_insert_and_finish(ed, &text);
            }
        }
        LastChange::ReplaceMode { text } => {
            use hjkl_buffer::{Edit, MotionKind, Position};
            ed.push_undo();
            for ch in text.chars() {
                let cursor = buf_cursor_pos(&ed.buffer);
                let line_chars = buf_line_chars(&ed.buffer, cursor.row);
                if cursor.col < line_chars {
                    // Overtype the char under the cursor.
                    ed.mutate_edit(Edit::DeleteRange {
                        start: cursor,
                        end: Position::new(cursor.row, cursor.col + 1),
                        kind: MotionKind::Char,
                    });
                }
                ed.mutate_edit(Edit::InsertChar { at: cursor, ch });
                buf_set_cursor_rc(&mut ed.buffer, cursor.row, cursor.col + 1);
            }
            // Esc step-back onto the last overtyped char.
            if ed.cursor().1 > 0 {
                crate::motions::move_left(&mut ed.buffer, 1);
            }
            ed.push_buffer_cursor_to_textarea();
        }
        LastChange::DeleteToEol { inserted } => {
            use hjkl_buffer::{Edit, Position};
            ed.push_undo();
            delete_to_eol(ed);
            if let Some(text) = inserted {
                let cursor = ed.cursor();
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(cursor.0, cursor.1),
                    text,
                });
            }
        }
        LastChange::OpenLine { above, inserted } => {
            use hjkl_buffer::{Edit, Position};
            ed.push_undo();
            ed.sync_buffer_content_from_textarea();
            let row = buf_cursor_pos(&ed.buffer).row;
            if above {
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(row, 0),
                    text: "\n".to_string(),
                });
                let folds = crate::buffer_impl::SnapshotFoldProvider::from_buffer(&ed.buffer);
                crate::motions::move_up(&mut ed.buffer, &folds, 1, &mut ed.sticky_col);
            } else {
                let line_chars = buf_line_chars(&ed.buffer, row);
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(row, line_chars),
                    text: "\n".to_string(),
                });
            }
            ed.push_buffer_cursor_to_textarea();
            let cursor = ed.cursor();
            ed.mutate_edit(Edit::InsertStr {
                at: Position::new(cursor.0, cursor.1),
                text: inserted,
            });
        }
        LastChange::InsertAt {
            entry,
            inserted,
            count,
        } => {
            use hjkl_buffer::{Edit, Position};
            ed.push_undo();
            match entry {
                InsertEntry::I => {}
                InsertEntry::ShiftI => move_first_non_whitespace(ed),
                InsertEntry::A => {
                    crate::motions::move_right_to_end(&mut ed.buffer, 1);
                    ed.push_buffer_cursor_to_textarea();
                }
                InsertEntry::ShiftA => {
                    crate::motions::move_line_end(&mut ed.buffer);
                    crate::motions::move_right_to_end(&mut ed.buffer, 1);
                    ed.push_buffer_cursor_to_textarea();
                }
            }
            for _ in 0..count.max(1) {
                let cursor = ed.cursor();
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(cursor.0, cursor.1),
                    text: inserted.clone(),
                });
            }
        }
    }
    ed.vim.replaying = false;
}

// ─── Extracting inserted text for replay ───────────────────────────────────

/// The substring of `after` that differs from `before` (first-diff to
/// last-diff). Unlike [`extract_inserted`] this works for equal-length or
/// shorter results, so it captures `R` overstrike text for dot-repeat.
fn changed_run(before: &str, after: &str) -> String {
    let a: Vec<char> = before.chars().collect();
    let b: Vec<char> = after.chars().collect();
    let prefix = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    let max_suffix = a.len().min(b.len()) - prefix;
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(max_suffix)
        .take_while(|(x, y)| x == y)
        .count();
    b[prefix..b.len() - suffix].iter().collect()
}

fn extract_inserted(before: &str, after: &str) -> String {
    let before_chars: Vec<char> = before.chars().collect();
    let after_chars: Vec<char> = after.chars().collect();
    if after_chars.len() <= before_chars.len() {
        return String::new();
    }
    let prefix = before_chars
        .iter()
        .zip(after_chars.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let max_suffix = before_chars.len() - prefix;
    let suffix = before_chars
        .iter()
        .rev()
        .zip(after_chars.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    after_chars[prefix..after_chars.len() - suffix]
        .iter()
        .collect()
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod comment_continuation_tests {
    use super::*;
    use crate::{DefaultHost, Editor, Options};
    use hjkl_buffer::Buffer;

    fn make_editor_with_lang(lang: &str, content: &str) -> Editor<Buffer, DefaultHost> {
        let buf = Buffer::from_str(content);
        let host = DefaultHost::new();
        let opts = Options {
            filetype: lang.to_string(),
            formatoptions: "ro".to_string(),
            ..Options::default()
        };
        Editor::new(buf, host, opts)
    }

    #[test]
    fn detect_rust_doc_comment() {
        let result = detect_comment_on_line("rust", "/// foo bar");
        assert!(result.is_some());
        let (indent, prefix) = result.unwrap();
        assert_eq!(indent, "");
        assert_eq!(prefix, "/// ");
    }

    #[test]
    fn detect_rust_inner_doc_comment() {
        let result = detect_comment_on_line("rust", "//! crate docs");
        assert!(result.is_some());
        let (_, prefix) = result.unwrap();
        assert_eq!(prefix, "//! ");
    }

    #[test]
    fn detect_rust_plain_comment() {
        let result = detect_comment_on_line("rust", "// normal comment");
        assert!(result.is_some());
        let (_, prefix) = result.unwrap();
        assert_eq!(prefix, "// ");
    }

    #[test]
    fn detect_indented_comment() {
        let result = detect_comment_on_line("rust", "    // indented");
        assert!(result.is_some());
        let (indent, prefix) = result.unwrap();
        assert_eq!(indent, "    ");
        assert_eq!(prefix, "// ");
    }

    #[test]
    fn detect_python_hash() {
        let result = detect_comment_on_line("python", "# comment");
        assert!(result.is_some());
        let (_, prefix) = result.unwrap();
        assert_eq!(prefix, "# ");
    }

    #[test]
    fn detect_lua_double_dash() {
        let result = detect_comment_on_line("lua", "-- a lua comment");
        assert!(result.is_some());
        let (_, prefix) = result.unwrap();
        assert_eq!(prefix, "-- ");
    }

    #[test]
    fn detect_non_comment_is_none() {
        assert!(detect_comment_on_line("rust", "let x = 1;").is_none());
        assert!(detect_comment_on_line("python", "x = 1").is_none());
    }

    #[test]
    fn detect_bare_double_slash_still_matches() {
        // A line that is exactly `//` with nothing after.
        assert!(detect_comment_on_line("rust", "//").is_some());
    }

    #[test]
    fn rust_doc_before_plain() {
        // `///` must match before `//`.
        let result = detect_comment_on_line("rust", "/// outer doc");
        let (_, prefix) = result.unwrap();
        assert_eq!(prefix, "/// ", "/// must match before //");
    }

    #[test]
    fn continue_comment_returns_prefix_for_comment_row() {
        let ed = make_editor_with_lang("rust", "/// hello\n");
        let cont = continue_comment(&ed.buffer, &ed.settings, 0);
        assert_eq!(cont, Some("/// ".to_string()));
    }

    #[test]
    fn continue_comment_returns_none_for_non_comment() {
        let ed = make_editor_with_lang("rust", "let x = 1;\n");
        let cont = continue_comment(&ed.buffer, &ed.settings, 0);
        assert!(cont.is_none());
    }

    #[test]
    fn continue_comment_returns_none_when_filetype_empty() {
        let buf = Buffer::from_str("// hello\n");
        let host = DefaultHost::new();
        // filetype defaults to "" in Options::default().
        let ed = Editor::new(buf, host, Options::default());
        let cont = continue_comment(&ed.buffer, &ed.settings, 0);
        assert!(cont.is_none());
    }
}

#[cfg(test)]
mod comment_toggle_tests {
    use super::*;
    use crate::{DefaultHost, Editor, Options};
    use hjkl_buffer::Buffer;

    fn make_rust_editor(content: &str) -> Editor<Buffer, DefaultHost> {
        let buf = Buffer::from_str(content);
        let host = DefaultHost::new();
        let opts = Options {
            filetype: "rust".to_string(),
            ..Options::default()
        };
        Editor::new(buf, host, opts)
    }

    fn line(ed: &Editor<Buffer, DefaultHost>, row: usize) -> String {
        buf_line(&ed.buffer, row).unwrap_or_default()
    }

    // ── gcc: toggle comment on current line ──────────────────────────────────

    #[test]
    fn gcc_comments_rust_line() {
        let mut ed = make_rust_editor("let x = 1;");
        ed.toggle_comment_range(0, 0);
        assert_eq!(line(&ed, 0), "// let x = 1;");
    }

    #[test]
    fn gcc_uncomments_rust_line() {
        let mut ed = make_rust_editor("// let x = 1;");
        ed.toggle_comment_range(0, 0);
        assert_eq!(line(&ed, 0), "let x = 1;");
    }

    #[test]
    fn gcc_indent_preserving() {
        // Marker inserted after leading whitespace, not at column 0.
        let mut ed = make_rust_editor("    let x = 1;");
        ed.toggle_comment_range(0, 0);
        assert_eq!(line(&ed, 0), "    // let x = 1;");
    }

    #[test]
    fn gcc_indent_preserving_uncomment() {
        let mut ed = make_rust_editor("    // let x = 1;");
        ed.toggle_comment_range(0, 0);
        assert_eq!(line(&ed, 0), "    let x = 1;");
    }

    // ── Multi-line toggle ────────────────────────────────────────────────────

    #[test]
    fn toggle_multi_line_all_uncommented() {
        let content = "let a = 1;\nlet b = 2;\nlet c = 3;";
        let mut ed = make_rust_editor(content);
        ed.toggle_comment_range(0, 2);
        assert_eq!(line(&ed, 0), "// let a = 1;");
        assert_eq!(line(&ed, 1), "// let b = 2;");
        assert_eq!(line(&ed, 2), "// let c = 3;");
    }

    #[test]
    fn toggle_multi_line_all_commented() {
        let content = "// let a = 1;\n// let b = 2;\n// let c = 3;";
        let mut ed = make_rust_editor(content);
        ed.toggle_comment_range(0, 2);
        assert_eq!(line(&ed, 0), "let a = 1;");
        assert_eq!(line(&ed, 1), "let b = 2;");
        assert_eq!(line(&ed, 2), "let c = 3;");
    }

    // ── Mixed state → all gets commented (vim-commentary parity) ────────────

    #[test]
    fn toggle_mixed_state_comments_all() {
        // 3 uncommented + 2 commented → all 5 get commented.
        let content = "let a = 1;\n// let b = 2;\nlet c = 3;\n// let d = 4;\nlet e = 5;";
        let mut ed = make_rust_editor(content);
        ed.toggle_comment_range(0, 4);
        for r in 0..5 {
            assert!(
                line(&ed, r).trim_start().starts_with("//"),
                "row {r} not commented: {:?}",
                line(&ed, r)
            );
        }
    }

    // ── Blank lines skipped ──────────────────────────────────────────────────

    #[test]
    fn blank_lines_not_commented() {
        let content = "let a = 1;\n\nlet b = 2;";
        let mut ed = make_rust_editor(content);
        ed.toggle_comment_range(0, 2);
        assert_eq!(line(&ed, 0), "// let a = 1;");
        assert_eq!(line(&ed, 1), ""); // blank — untouched
        assert_eq!(line(&ed, 2), "// let b = 2;");
    }

    // ── Python hash comments ─────────────────────────────────────────────────

    #[test]
    fn python_comment_toggle() {
        let buf = Buffer::from_str("x = 1\ny = 2");
        let host = DefaultHost::new();
        let opts = Options {
            filetype: "python".to_string(),
            ..Options::default()
        };
        let mut ed = Editor::new(buf, host, opts);
        ed.toggle_comment_range(0, 1);
        assert_eq!(line(&ed, 0), "# x = 1");
        assert_eq!(line(&ed, 1), "# y = 2");
        // Toggle back.
        ed.toggle_comment_range(0, 1);
        assert_eq!(line(&ed, 0), "x = 1");
        assert_eq!(line(&ed, 1), "y = 2");
    }

    // ── commentstring override ───────────────────────────────────────────────

    #[test]
    fn commentstring_override_via_setting() {
        let buf = Buffer::from_str("hello world");
        let host = DefaultHost::new();
        let opts = Options {
            filetype: "rust".to_string(),
            ..Options::default()
        };
        let mut ed = Editor::new(buf, host, opts);
        // Override with a custom marker.
        ed.settings_mut().commentstring = "# %s".to_string();
        ed.toggle_comment_range(0, 0);
        assert_eq!(line(&ed, 0), "# hello world");
    }

    // ── Unknown language → no-op ─────────────────────────────────────────────

    #[test]
    fn unknown_lang_no_op() {
        let buf = Buffer::from_str("hello");
        let host = DefaultHost::new();
        let opts = Options::default(); // filetype = ""
        let mut ed = Editor::new(buf, host, opts);
        ed.toggle_comment_range(0, 0);
        // Should be unchanged — no comment string for "".
        assert_eq!(line(&ed, 0), "hello");
    }
}

// ─── g& tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod g_ampersand_tests {
    use super::*;
    use crate::{DefaultHost, Editor, Options};
    use hjkl_buffer::{Buffer, rope_line_str};

    fn make_editor(content: &str) -> Editor<Buffer, DefaultHost> {
        let buf = Buffer::from_str(content);
        let host = DefaultHost::new();
        Editor::new(buf, host, Options::default())
    }

    fn buf_line(ed: &Editor<Buffer, DefaultHost>, row: usize) -> String {
        let rope = ed.buffer().rope();
        rope_line_str(&rope, row).trim_end_matches('\n').to_string()
    }

    /// `g&` repeats last `:s/foo/bar/` over every line (no /g flag → first
    /// match per line only).
    #[test]
    fn g_ampersand_repeats_last_substitute_on_whole_buffer() {
        let mut ed = make_editor("foo\nfoo bar foo\nbaz");
        // Simulate a prior `:s/foo/bar/` by setting last_substitute directly.
        let cmd = crate::substitute::parse_substitute("/foo/bar/").unwrap();
        ed.set_last_substitute(cmd);
        // Cursor on line 0 (to confirm g& operates on ALL lines, not just current).
        apply_after_g(&mut ed, '&', 1);
        assert_eq!(buf_line(&ed, 0), "bar");
        // No /g flag — only first match per line.
        assert_eq!(buf_line(&ed, 1), "bar bar foo");
        assert_eq!(buf_line(&ed, 2), "baz");
    }

    /// `g&` with /g flag replaces all matches per line.
    #[test]
    fn g_ampersand_with_g_flag_replaces_all_per_line() {
        let mut ed = make_editor("foo foo\nfoo");
        let cmd = crate::substitute::parse_substitute("/foo/bar/g").unwrap();
        ed.set_last_substitute(cmd);
        apply_after_g(&mut ed, '&', 1);
        assert_eq!(buf_line(&ed, 0), "bar bar");
        assert_eq!(buf_line(&ed, 1), "bar");
    }

    /// `g&` with no prior substitute is a no-op.
    #[test]
    fn g_ampersand_noop_when_no_prior_substitute() {
        let mut ed = make_editor("foo\nbar");
        // No last_substitute set — must not panic, must not change buffer.
        apply_after_g(&mut ed, '&', 1);
        assert_eq!(buf_line(&ed, 0), "foo");
        assert_eq!(buf_line(&ed, 1), "bar");
    }
}

// ─── Sneak motion tests ───────────────────────────────────────────────────

#[cfg(test)]
mod sneak_tests {
    use super::*;
    use crate::{DefaultHost, Editor, Options};
    use hjkl_buffer::Buffer;

    fn make_editor(content: &str) -> Editor<Buffer, DefaultHost> {
        let buf = Buffer::from_str(content);
        let host = DefaultHost::new();
        Editor::new(buf, host, Options::default())
    }

    /// `s ba` from [0,0] on "foo bar baz qux\n" → cursor at [0,4] (start of "ba" in "bar").
    #[test]
    fn sneak_forward_jumps_to_two_char_digraph() {
        let mut ed = make_editor("foo bar baz qux\n");
        ed.jump_cursor(0, 0);
        ed.sneak('b', 'a', true, 1);
        assert_eq!(ed.cursor(), (0, 4), "cursor should land on 'ba' in 'bar'");
    }

    /// `S ba` from [0,12] on "foo bar baz qux\n" → cursor at [0,8] ("ba" in "baz").
    #[test]
    fn sneak_backward_jumps_to_prior_match() {
        let mut ed = make_editor("foo bar baz qux\n");
        ed.jump_cursor(0, 12);
        ed.sneak('b', 'a', false, 1);
        assert_eq!(
            ed.cursor(),
            (0, 8),
            "backward sneak should find 'ba' in 'baz'"
        );
    }

    /// After sneak forward to "bar", `;` (sneak-repeat) jumps to next "ba" ("baz").
    #[test]
    fn sneak_repeat_semicolon_next_match() {
        let mut ed = make_editor("foo bar baz qux\n");
        ed.jump_cursor(0, 0);
        // First sneak: lands at [0,4]
        ed.sneak('b', 'a', true, 1);
        assert_eq!(ed.cursor(), (0, 4));
        // Repeat via execute_motion FindRepeat (which routes through sneak if last was sneak)
        execute_motion(&mut ed, Motion::FindRepeat { reverse: false }, 1);
        assert_eq!(ed.cursor(), (0, 8), "semicolon should jump to next 'ba'");
    }

    /// After sneak forward from [0,0] to [0,4], `,` (reverse) — no prior "ba" → stays.
    #[test]
    fn sneak_repeat_comma_prev_match() {
        let mut ed = make_editor("foo bar baz qux\n");
        ed.jump_cursor(0, 0);
        ed.sneak('b', 'a', true, 1);
        assert_eq!(ed.cursor(), (0, 4));
        // Reverse repeat — no "ba" before col 4, so cursor must not move.
        let pre = ed.cursor();
        execute_motion(&mut ed, Motion::FindRepeat { reverse: true }, 1);
        assert_eq!(
            ed.cursor(),
            pre,
            "comma with no prior match should leave cursor unchanged"
        );
    }

    /// `S ba` from [0,12] jumps backward.
    #[test]
    fn sneak_s_searches_backward() {
        let mut ed = make_editor("foo bar baz qux\n");
        ed.jump_cursor(0, 12);
        ed.sneak('b', 'a', false, 1);
        assert_eq!(ed.cursor(), (0, 8));
    }

    /// `2s ba` from [0,0] jumps to 2nd "ba" occurrence.
    #[test]
    fn sneak_with_count_jumps_to_nth() {
        let mut ed = make_editor("foo bar baz qux\n");
        ed.jump_cursor(0, 0);
        ed.sneak('b', 'a', true, 2);
        assert_eq!(ed.cursor(), (0, 8), "count=2 should jump to 2nd 'ba'");
    }

    /// `s xx` with no match — cursor stays put.
    #[test]
    fn sneak_no_match_cursor_stays() {
        let mut ed = make_editor("foo bar baz qux\n");
        ed.jump_cursor(0, 0);
        let pre = ed.cursor();
        ed.sneak('x', 'x', true, 1);
        assert_eq!(ed.cursor(), pre, "no match should leave cursor unchanged");
    }

    /// `dsab` on "hello ab world\n" from [0,0] → deletes up to 'ab', leaving "ab world\n".
    #[test]
    fn operator_pending_dsab_deletes_to_digraph() {
        let mut ed = make_editor("hello ab world\n");
        ed.jump_cursor(0, 0);
        ed.apply_op_sneak(Operator::Delete, 'a', 'b', true, 1);
        // Buffer content after exclusive delete from [0,0] to [0,6] (start of "ab").
        let content = ed.content();
        assert!(
            content.starts_with("ab world"),
            "dsab should delete 'hello ' leaving 'ab world'; got: {content:?}"
        );
    }

    /// Cross-line sneak: "foo\nbar baz\n", cursor [0,0], `s ba` → [1,0].
    #[test]
    fn sneak_cross_line_match() {
        let mut ed = make_editor("foo\nbar baz\n");
        ed.jump_cursor(0, 0);
        ed.sneak('b', 'a', true, 1);
        assert_eq!(ed.cursor(), (1, 0), "sneak should cross line boundary");
    }

    /// `last_sneak` is updated after `sneak()` so `;`/`,` can repeat.
    #[test]
    fn sneak_updates_last_sneak_state() {
        let mut ed = make_editor("foo bar baz\n");
        ed.jump_cursor(0, 0);
        ed.sneak('b', 'a', true, 1);
        let ls = ed.last_sneak();
        assert_eq!(
            ls,
            Some((('b', 'a'), true)),
            "last_sneak should record the digraph and direction"
        );
    }
}

// ─── [count]>> / [count]<< line-operator count tests ──────────────────────
//
// vim semantics (captured from `nvim --headless`, mirrored by the
// `tier2_indent_count` oracle corpus):
//   - `[count]op` operates on `count` lines from the cursor, clamped to the
//     buffer end.
//   - The implied `count_` motion moves `count - 1` lines down; on the last
//     line it can't move, so `[count>=2]>>` / `<<` is a complete no-op (E16).
#[cfg(test)]
mod indent_count_tests {
    use super::*;
    use crate::{DefaultHost, Editor, Options};
    use hjkl_buffer::Buffer;

    fn make_editor(content: &str) -> Editor<Buffer, DefaultHost> {
        let buf = Buffer::from_str(content);
        let mut ed = Editor::new(buf, DefaultHost::new(), Options::default());
        ed.settings_mut().expandtab = true;
        ed.settings_mut().shiftwidth = 4;
        ed
    }

    fn content(ed: &Editor<Buffer, DefaultHost>) -> String {
        (*ed.buffer().content_joined()).clone()
    }

    #[test]
    fn count_indent_operates_on_n_lines() {
        let mut ed = make_editor("a\nb\nc\nd\ne\nf\n");
        ed.jump_cursor(0, 0);
        execute_line_op(&mut ed, Operator::Indent, 3);
        assert_eq!(content(&ed), "    a\n    b\n    c\nd\ne\nf\n");
    }

    #[test]
    fn count_indent_clamps_to_buffer_end() {
        let mut ed = make_editor("a\nb\nc\nd\ne\nf\n");
        ed.jump_cursor(0, 0);
        execute_line_op(&mut ed, Operator::Indent, 10);
        assert_eq!(content(&ed), "    a\n    b\n    c\n    d\n    e\n    f\n");
    }

    #[test]
    fn count_outdent_clamps_to_buffer_end() {
        let mut ed = make_editor("    a\n    b\n    c\n");
        ed.jump_cursor(0, 0);
        execute_line_op(&mut ed, Operator::Outdent, 10);
        assert_eq!(content(&ed), "a\nb\nc\n");
    }

    #[test]
    fn count_indent_on_last_line_is_noop() {
        let mut ed = make_editor("a\nb\nc\n");
        ed.jump_cursor(2, 0); // last content line
        execute_line_op(&mut ed, Operator::Indent, 5);
        assert_eq!(
            content(&ed),
            "a\nb\nc\n",
            "5>> on last line must abort (E16)"
        );
    }

    #[test]
    fn count_indent_on_single_line_is_noop() {
        let mut ed = make_editor("x\n");
        ed.jump_cursor(0, 0);
        execute_line_op(&mut ed, Operator::Indent, 5);
        assert_eq!(content(&ed), "x\n", "5>> on the only line must abort (E16)");
    }

    #[test]
    fn count_outdent_on_last_line_is_noop() {
        let mut ed = make_editor("    a\n    b\n    c\n");
        ed.jump_cursor(2, 0);
        execute_line_op(&mut ed, Operator::Outdent, 5);
        assert_eq!(content(&ed), "    a\n    b\n    c\n");
    }

    #[test]
    fn single_indent_on_last_line_still_works() {
        // count == 1 needs no motion, so `>>` on the last line indents it.
        let mut ed = make_editor("a\nb\nc\n");
        ed.jump_cursor(2, 0);
        execute_line_op(&mut ed, Operator::Indent, 1);
        assert_eq!(content(&ed), "a\nb\n    c\n");
    }
}

// ── try_abbrev_expand unit tests ─────────────────────────────────────────────

#[cfg(test)]
mod abbrev_tests {
    use super::{Abbrev, AbbrevKind, AbbrevTrigger, abbrev_kind, try_abbrev_expand};
    use AbbrevKind::{End, Full, NonKw};

    const ISK: &str = "@,48-57,_,192-255"; // default iskeyword

    fn make_abbrev(lhs: &str, rhs: &str) -> Abbrev {
        Abbrev {
            lhs: lhs.to_string(),
            rhs: rhs.to_string(),
            insert: true,
            cmdline: false,
            noremap: false,
        }
    }

    fn expand(
        abbrevs: &[Abbrev],
        before: &str,
        mincol: usize,
        trig: AbbrevTrigger,
    ) -> Option<(usize, String)> {
        try_abbrev_expand(abbrevs, before, mincol, trig, ISK)
    }

    // ── abbrev_type classification ────────────────────────────────────────────

    #[test]
    fn fullid_all_keyword_chars() {
        assert_eq!(abbrev_kind("teh", ISK), Full);
        assert_eq!(abbrev_kind("abc123", ISK), Full);
        assert_eq!(abbrev_kind("_foo", ISK), Full);
    }

    #[test]
    fn endid_ends_with_kw_has_nonkw() {
        assert_eq!(abbrev_kind("#i", ISK), End);
        assert_eq!(abbrev_kind("#include", ISK), End);
    }

    #[test]
    fn nonid_ends_with_nonkw() {
        assert_eq!(abbrev_kind(";;", ISK), NonKw);
        assert_eq!(abbrev_kind("->", ISK), NonKw);
    }

    // ── full-id expansion ─────────────────────────────────────────────────────

    #[test]
    fn fullid_expands_on_space_trigger() {
        let abbrevs = [make_abbrev("teh", "the")];
        let r = expand(&abbrevs, "teh", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, Some((3, "the".to_string())));
    }

    #[test]
    fn fullid_expands_on_esc_trigger() {
        let abbrevs = [make_abbrev("teh", "the")];
        let r = expand(&abbrevs, "teh", 0, AbbrevTrigger::Esc);
        assert_eq!(r, Some((3, "the".to_string())));
    }

    #[test]
    fn fullid_expands_on_cr_trigger() {
        let abbrevs = [make_abbrev("teh", "the")];
        let r = expand(&abbrevs, "teh", 0, AbbrevTrigger::Cr);
        assert_eq!(r, Some((3, "the".to_string())));
    }

    #[test]
    fn fullid_expands_on_ctrl_bracket() {
        let abbrevs = [make_abbrev("teh", "the")];
        let r = expand(&abbrevs, "teh", 0, AbbrevTrigger::CtrlBracket);
        assert_eq!(r, Some((3, "the".to_string())));
    }

    #[test]
    fn fullid_does_not_expand_on_keyword_trigger() {
        // Typing a keyword char after "teh" would extend the word — no expand.
        let abbrevs = [make_abbrev("teh", "the")];
        let r = expand(&abbrevs, "teh", 0, AbbrevTrigger::NonKeyword('a'));
        // 'a' is keyword — should not trigger
        assert_eq!(r, None);
    }

    #[test]
    fn fullid_no_expand_when_lhs_not_at_end() {
        let abbrevs = [make_abbrev("teh", "the")];
        // "ateh" — 'a' before is keyword, so skip.
        let r = expand(&abbrevs, "ateh", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, None);
    }

    #[test]
    fn fullid_expands_after_nonkw_prefix() {
        let abbrevs = [make_abbrev("teh", "the")];
        // "!teh" — '!' before is non-keyword → expand.
        let r = expand(&abbrevs, "!teh", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, Some((3, "the".to_string())));
    }

    #[test]
    fn fullid_single_char_no_expand_after_nonblank_nonkw() {
        let abbrevs = [make_abbrev("a", "b")];
        // "!a" — '!' is non-blank non-keyword before single-char lhs → no expand.
        let r = expand(&abbrevs, "!a", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, None);
    }

    #[test]
    fn fullid_single_char_expands_after_space() {
        let abbrevs = [make_abbrev("a", "b")];
        // " a" — space before single-char lhs → expand.
        let r = expand(&abbrevs, " a", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, Some((1, "b".to_string())));
    }

    // ── mincol: pre-existing text must not be consumed ────────────────────────

    #[test]
    fn mincol_blocks_consuming_preexisting_text() {
        let abbrevs = [make_abbrev("teh", "the")];
        // "teh" is at cols 0..3, but insert started at col 3 → no match.
        let r = expand(&abbrevs, "teh", 3, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, None);
    }

    #[test]
    fn mincol_allows_match_starting_at_mincol() {
        let abbrevs = [make_abbrev("teh", "the")];
        // Existing text "!! " at 0..3, then user typed "teh" → mincol=3.
        // The char before the lhs is ' ' (non-keyword), so full-id expands.
        let r = expand(&abbrevs, "!! teh", 3, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, Some((3, "the".to_string())));
    }

    // ── end-id expansion ──────────────────────────────────────────────────────

    #[test]
    fn endid_expands_on_space_trigger() {
        let abbrevs = [make_abbrev("#i", "#include")];
        let r = expand(&abbrevs, "#i", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, Some((2, "#include".to_string())));
    }

    #[test]
    fn endid_expands_on_esc_trigger() {
        let abbrevs = [make_abbrev("#i", "#include")];
        let r = expand(&abbrevs, "#i", 0, AbbrevTrigger::Esc);
        assert_eq!(r, Some((2, "#include".to_string())));
    }

    // ── non-id expansion ──────────────────────────────────────────────────────

    #[test]
    fn nonid_expands_on_esc_trigger() {
        let abbrevs = [make_abbrev(";;", "std::endl;")];
        let r = expand(&abbrevs, ";;", 0, AbbrevTrigger::Esc);
        assert_eq!(r, Some((2, "std::endl;".to_string())));
    }

    #[test]
    fn nonid_expands_on_cr_trigger() {
        let abbrevs = [make_abbrev(";;", "std::endl;")];
        let r = expand(&abbrevs, ";;", 0, AbbrevTrigger::Cr);
        assert_eq!(r, Some((2, "std::endl;".to_string())));
    }

    #[test]
    fn nonid_does_not_expand_on_nonkw_trigger() {
        // non-id abbreviations must NOT expand on regular typed chars like space.
        let abbrevs = [make_abbrev(";;", "std::endl;")];
        let r = expand(&abbrevs, ";;", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, None);
    }

    #[test]
    fn nonid_expands_on_ctrl_bracket() {
        let abbrevs = [make_abbrev(";;", "std::endl;")];
        let r = expand(&abbrevs, ";;", 0, AbbrevTrigger::CtrlBracket);
        assert_eq!(r, Some((2, "std::endl;".to_string())));
    }

    // ── multiword rhs ─────────────────────────────────────────────────────────

    #[test]
    fn multiword_rhs_expansion() {
        let abbrevs = [make_abbrev("hw", "hello world")];
        let r = expand(&abbrevs, "hw", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, Some((2, "hello world".to_string())));
    }

    // ── empty / no match ─────────────────────────────────────────────────────

    #[test]
    fn no_match_returns_none() {
        let abbrevs = [make_abbrev("teh", "the")];
        let r = expand(&abbrevs, "xyz", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, None);
    }

    #[test]
    fn empty_abbrevs_returns_none() {
        let r = expand(&[], "teh", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, None);
    }

    #[test]
    fn empty_before_text_returns_none() {
        let abbrevs = [make_abbrev("teh", "the")];
        let r = expand(&abbrevs, "", 0, AbbrevTrigger::NonKeyword(' '));
        assert_eq!(r, None);
    }
}
