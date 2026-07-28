//! Debug-only `sticky_col` (vim's `curswant`) invariant — phase 0 of the
//! cursor-move refactor described in `docs/cursor-moves.md`.
//!
//! Moving the cursor and maintaining `sticky_col` are two separate actions
//! today and nothing forces the second, which is how `c022a3a4` happened:
//! `/pattern<CR>` moved the cursor without resetting `curswant`, so the next
//! `j` snapped back to the pre-search column. Phases 2–4 will migrate ~186
//! call sites onto `Editor::move_cursor`; this check is the net that makes a
//! misclassification during that migration fail loudly inside the test suite
//! instead of shipping as a silent behaviour change.
//!
//! Compiled out entirely in release: the whole module is
//! `#[cfg(debug_assertions)]` and both of its hooks in [`crate::dispatch_input`]
//! are gated the same way.

use crate::vim::{Mode, Pending};
use crate::vim_state::vim;
use hjkl_buffer::char_col_to_visual_col;
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_line, buf_line_chars};

/// Pre-keystroke state, captured before the FSM runs.
pub struct Pre {
    cursor: (usize, usize),
    dirty_gen: u64,
    mode: Mode,
    /// True when this keystroke started from a clean Normal/Visual state —
    /// no operator/find/register chord in flight, no count prefix. Only such
    /// keystrokes can be plain motions.
    clean_start: bool,
    /// True when `sticky_col` already satisfied [`holds`] before the key ran.
    /// A motion that merely carries a pre-existing violation forward is not
    /// the culprit; only the keystroke that *introduces* one is reported.
    was_legal: bool,
}

pub fn capture<H: hjkl_engine::Host>(ed: &Editor<hjkl_buffer::View, H>) -> Pre {
    let v = vim(ed);
    Pre {
        cursor: ed.cursor(),
        dirty_gen: ed.buffer().dirty_gen(),
        mode: v.mode,
        clean_start: matches!(v.pending, Pending::None)
            && v.count == 0
            && v.pending_register.is_none()
            && is_motion_mode(v.mode),
        was_legal: holds(ed),
    }
}

fn is_motion_mode(mode: Mode) -> bool {
    matches!(
        mode,
        Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock
    )
}

/// The invariant itself: is the editor's `(cursor, sticky_col)` pair one the
/// two curswant rules can produce? See [`assert_invariant`] for the three
/// legal shapes.
fn holds<H: hjkl_engine::Host>(ed: &Editor<hjkl_buffer::View, H>) -> bool {
    let (row, col) = ed.cursor();
    let Some(want) = ed.sticky_col() else {
        return true; // no motion has run yet
    };
    // sticky_col now stores a display column — convert the cursor's char
    // index to a display column before comparing.
    let line = buf_line(ed.buffer(), row).unwrap_or_default();
    let display_col = char_col_to_visual_col(&line, col, ed.settings().tabstop);
    if want == display_col {
        return true; // synced to the landed column
    }
    // A vertical motion clamped to a short row. Normal mode parks on the last
    // char (`len - 1`); Visual may sit one past the end, so both count as
    // "the end of this row".
    let line_len = buf_line_chars(ed.buffer(), row);
    want > display_col && col >= line_len.saturating_sub(1)
}

/// Check the `curswant` invariant after one keystroke has been dispatched.
///
/// # The rule
///
/// Already documented on `Editor::jump_cursor` and
/// `crate::vim::motion::apply_sticky_col`:
///
/// - **vertical** motions (`j`, `k`, and the `<C-e>`/`<C-y>` screen
///   equivalents) READ `sticky_col`, clamp the landing column to the row's
///   length, and leave `sticky_col` alone;
/// - **everything else** that moves the cursor — search hits, `gg`/`G`,
///   marks, clicks, word motions, `$`/`^`, `]d` — SETS `sticky_col` to the
///   column it landed on.
///
/// # What is checked
///
/// Rather than classify the keystroke as vertical or not (which is ambiguous:
/// `j` is vertical bare, but a target in `dj` / `fj` / `rj`, and a literal
/// char in Insert mode), the check tests the *state* the two rules can
/// produce. After a keystroke that moved the cursor to `(row, col)`,
/// `sticky_col` may only be:
///
/// 1. `None` — no motion has ever run on this editor; or
/// 2. `Some(col)` — the "everything else" rule; or
/// 3. `Some(want)` with `want > col` **and** `col` at the end of `row` —
///    the vertical rule parked on a row shorter than `want`.
///
/// Those are exactly the states phase 1's `Move` variants can reach (`Raw`
/// excepted, which is why `Raw` must stay justified per site). A stale
/// `curswant` left behind by a cursor move that forgot to sync — the
/// `c022a3a4` class — falls outside all three and trips the assert. Being
/// state-based rather than key-based, the check also does not need teaching
/// about new motions: one added later is covered the day it is written.
///
/// The check is on the *transition*, not on the state alone: it fires only
/// when the keystroke moved from a legal `(cursor, sticky_col)` pair to an
/// illegal one. A motion that inherits an already-stale `curswant` is not the
/// site that has to change, and reporting it would point the next phase at
/// the wrong file.
///
/// # What is deliberately *not* checked
///
/// Only keystrokes that were plain motions are checked — see [`Pre`] and the
/// guards below. Operators, edits, undo/redo, Insert mode and the `:` /
/// search command lines all move the cursor without being motions, and in
/// hjkl none of them maintain `curswant` today. Those are real deviations
/// from vim (vim resets `curswant` on every one of them) but they are a
/// different bug class from the one this net exists for, and they are
/// enumerated in the phase-0 report rather than silenced one by one. Widening
/// the check to cover them is a later phase's job — do not narrow it further.
pub fn assert_invariant<H: hjkl_engine::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    pre: Pre,
    input: hjkl_engine::Input,
) {
    // Not a plain motion: an operator/find/register chord or a count prefix
    // was already in flight when the key arrived.
    if !pre.clean_start {
        return;
    }
    // The keystroke edited the buffer, so it was an operator or an edit
    // command, not a motion.
    if ed.buffer().dirty_gen() != pre.dirty_gen {
        return;
    }
    let v = vim(ed);
    // The keystroke armed a chord (`d`, `f`, `"`) or started a count —
    // neither is a motion.
    if !matches!(v.pending, Pending::None) || v.count != 0 {
        return;
    }
    // The keystroke changed the mode, so it was not a motion: mode-entering
    // commands (`i`, `v`, `:`, `R`), `<Esc>`, and — the case that needs this
    // guard rather than the `dirty_gen` one — a Visual-mode operator that
    // makes no edit (`y`), which exits Visual and drops the cursor on the
    // selection start.
    if v.mode != pre.mode {
        return;
    }
    // A search prompt is still open: the cursor is on an incremental-search
    // preview, which is restored or committed when the prompt closes.
    if ed.search_prompt_state().is_some() {
        return;
    }
    // `sticky_col` was already stale when the key arrived — left behind by an
    // edit, an operator or an Insert-mode session, none of which maintain it
    // today. A motion that inherits that staleness is not the site to fix.
    if !pre.was_legal {
        return;
    }
    let (row, col) = ed.cursor();
    // Only a move can invalidate `curswant`; a keystroke that left the cursor
    // alone cannot have introduced a fresh violation.
    if (row, col) == pre.cursor {
        return;
    }
    if holds(ed) {
        return;
    }
    let want = ed.sticky_col().unwrap_or_default();
    let line_len = buf_line_chars(ed.buffer(), row);
    panic!(
        "curswant invariant violated after {input:?}: the cursor moved \
         {:?} -> ({row}, {col}) without editing the buffer, but sticky_col is \
         {want}. A non-vertical motion must set sticky_col to the column it \
         landed on (see docs/cursor-moves.md); row {row} has {line_len} \
         chars, so {want} is not a vertical clamp either.",
        pre.cursor
    );
}
