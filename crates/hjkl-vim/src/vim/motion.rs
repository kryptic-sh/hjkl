//! Vim FSM: motion.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{LastHorizontalMotion, Mode, Motion};

use hjkl_engine::input::{Input, Key};

use super::*;
use crate::vim_state::{vim, vim_mut};
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{
    buf_cursor_pos, buf_line, buf_line_chars, buf_row_count, buf_set_cursor_rc,
};

/// Parse the first key of a normal/visual-mode motion. Returns `None` for
/// keys that don't start a motion (operator keys, command keys, etc.).
/// Promoted to `pub` in Phase 6.6e so `hjkl-vim::normal` can call it.
pub fn parse_motion(input: &Input) -> Option<Motion> {
    if input.ctrl {
        // `<C-h>` is vim's `<BS>` — a wrapping left motion. (The hjkl app
        // rebinds `<C-h>` to window-focus-left before it reaches the engine;
        // this keeps it correct for engine consumers that don't override it.)
        if input.key == Key::Char('h') {
            return Some(Motion::BackspaceBack);
        }
        return None;
    }
    match input.key {
        Key::Char('h') | Key::Left => Some(Motion::Left),
        Key::Char('l') | Key::Right => Some(Motion::Right),
        // `<Space>`/`<BS>` are vim's right/left motions that WRAP at line ends
        // (default `whichwrap=b,s`), unlike `l`/`h`/arrows which never wrap.
        // Operators (`d<Space>`/`d<BS>`) act on one char mid-line like `dl`/`dh`.
        Key::Char(' ') => Some(Motion::SpaceFwd),
        Key::Backspace => Some(Motion::BackspaceBack),
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
pub(crate) fn execute_motion<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    motion: Motion,
    count: usize,
) {
    let count = count.clamp(1, MAX_COUNT);
    // `;`/`,` smart fallback: if the last horizontal motion was a sneak
    // digraph, repeat via apply_sneak instead of find-char.
    if let Motion::FindRepeat { reverse } = motion
        && vim(ed).last_horizontal_motion == LastHorizontalMotion::Sneak
    {
        if let Some(((c1, c2), fwd)) = vim(ed).last_sneak {
            let effective_fwd = if reverse { !fwd } else { fwd };
            apply_sneak(ed, c1, c2, effective_fwd, count);
        }
        return;
    }
    // FindRepeat needs the stored direction. A `;`/`,` repeat of a `t`/`T`
    // find must skip an immediately-adjacent match (vim's repeat quirk); flag
    // it so the `Motion::Find` dispatch below passes `skip_adjacent`.
    let motion = match motion {
        Motion::FindRepeat { reverse } => match vim(ed).last_find {
            Some((ch, forward, till)) => {
                vim_mut(ed).find_repeat_skip = true;
                Motion::Find {
                    ch,
                    forward: if reverse { !forward } else { forward },
                    till,
                }
            }
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
/// Wrapper around `execute_motion` that also syncs `block_vcol` when in
/// VisualBlock mode. The engine FSM's `step()` already does this (line ~2001);
/// the keymap path (`apply_motion_kind`) must do the same so VisualBlock h/l
/// extend the highlighted region correctly.
///
/// `update_block_vcol` is only a no-op for vertical / non-horizontal motions
/// (Up, Down, FileTop, FileBottom, Search), so passing every motion through is
/// safe — the function's own match arm handles the no-op case.
pub(crate) fn execute_motion_with_block_vcol<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    motion: Motion,
    count: usize,
) {
    let motion_copy = motion.clone();
    execute_motion(ed, motion, count);
    if vim(ed).mode == Mode::VisualBlock {
        update_block_vcol(ed, &motion_copy);
    }
}
/// Execute a `hjkl_engine::MotionKind` cursor motion. Called by the host's
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
pub(crate) fn apply_motion_kind<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    kind: hjkl_engine::MotionKind,
    count: usize,
) {
    let count = count.max(1);
    match kind {
        hjkl_engine::MotionKind::CharLeft => {
            execute_motion_with_block_vcol(ed, Motion::Left, count);
        }
        hjkl_engine::MotionKind::CharRight => {
            execute_motion_with_block_vcol(ed, Motion::Right, count);
        }
        hjkl_engine::MotionKind::LineDown => {
            execute_motion_with_block_vcol(ed, Motion::Down, count);
        }
        hjkl_engine::MotionKind::LineUp => {
            execute_motion_with_block_vcol(ed, Motion::Up, count);
        }
        hjkl_engine::MotionKind::FirstNonBlankDown => {
            // `+`: move down `count` lines then land on first non-blank.
            // Not a big-jump (no jump-list entry), sticky col set to the
            // landed column (first non-blank). Mirrors scroll_cursor_rows
            // semantics but goes through the fold-aware buffer motion path.
            let folds = hjkl_engine::SnapshotFoldProvider::from_buffer(ed.buffer());
            let mut sticky = ed.sticky_col();
            hjkl_engine::motions::move_down(ed.buffer_mut(), &folds, count, &mut sticky);
            ed.set_sticky_col(sticky);
            hjkl_engine::motions::move_first_non_blank(ed.buffer_mut());
            ed.push_buffer_cursor_to_textarea();
            ed.set_sticky_col(Some(buf_cursor_pos(ed.buffer()).col));
            ed.sync_buffer_from_textarea();
        }
        hjkl_engine::MotionKind::FirstNonBlankUp => {
            // `-`: move up `count` lines then land on first non-blank.
            // Same pattern as FirstNonBlankDown, direction reversed.
            let folds = hjkl_engine::SnapshotFoldProvider::from_buffer(ed.buffer());
            let mut sticky = ed.sticky_col();
            hjkl_engine::motions::move_up(ed.buffer_mut(), &folds, count, &mut sticky);
            ed.set_sticky_col(sticky);
            hjkl_engine::motions::move_first_non_blank(ed.buffer_mut());
            ed.push_buffer_cursor_to_textarea();
            ed.set_sticky_col(Some(buf_cursor_pos(ed.buffer()).col));
            ed.sync_buffer_from_textarea();
        }
        hjkl_engine::MotionKind::WordForward => {
            execute_motion_with_block_vcol(ed, Motion::WordFwd, count);
        }
        hjkl_engine::MotionKind::BigWordForward => {
            execute_motion_with_block_vcol(ed, Motion::BigWordFwd, count);
        }
        hjkl_engine::MotionKind::WordBackward => {
            execute_motion_with_block_vcol(ed, Motion::WordBack, count);
        }
        hjkl_engine::MotionKind::BigWordBackward => {
            execute_motion_with_block_vcol(ed, Motion::BigWordBack, count);
        }
        hjkl_engine::MotionKind::WordEnd => {
            execute_motion_with_block_vcol(ed, Motion::WordEnd, count);
        }
        hjkl_engine::MotionKind::BigWordEnd => {
            execute_motion_with_block_vcol(ed, Motion::BigWordEnd, count);
        }
        hjkl_engine::MotionKind::LineStart => {
            // `0` / `<Home>`: first column of the current line.
            // count is ignored — matches vim `0` semantics.
            execute_motion_with_block_vcol(ed, Motion::LineStart, 1);
        }
        hjkl_engine::MotionKind::FirstNonBlank => {
            // `^`: first non-blank column on the current line.
            // count is ignored — matches vim `^` semantics.
            execute_motion_with_block_vcol(ed, Motion::FirstNonBlank, 1);
        }
        hjkl_engine::MotionKind::GotoLine => {
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
        hjkl_engine::MotionKind::LineEnd => {
            // `$` / `<End>`: last character on the current line.
            // count is ignored at the keymap-path level (vim `N$` moves
            // down N-1 lines then lands at line-end; not yet wired).
            execute_motion_with_block_vcol(ed, Motion::LineEnd, 1);
        }
        hjkl_engine::MotionKind::FindRepeat => {
            // `;` — repeat last f/F/t/T in the same direction.
            // execute_motion resolves FindRepeat via vim(ed).last_find;
            // no-op if no prior find exists (None arm returns early).
            execute_motion_with_block_vcol(ed, Motion::FindRepeat { reverse: false }, count);
        }
        hjkl_engine::MotionKind::FindRepeatReverse => {
            // `,` — repeat last f/F/t/T in the reverse direction.
            // execute_motion resolves FindRepeat via vim(ed).last_find;
            // no-op if no prior find exists (None arm returns early).
            execute_motion_with_block_vcol(ed, Motion::FindRepeat { reverse: true }, count);
        }
        hjkl_engine::MotionKind::BracketMatch => {
            // `%` — jump to the matching bracket.
            // count is passed through; engine-side matching_bracket handles
            // the no-match case as a no-op (cursor stays). Engine FSM arm
            // for `%` in parse_motion is kept intact for macro-replay.
            execute_motion_with_block_vcol(ed, Motion::MatchBracket, count);
        }
        hjkl_engine::MotionKind::ViewportTop => {
            // `H` — cursor to top of visible viewport, then count-1 rows down.
            // Engine FSM arm for `H` in parse_motion is kept intact for macro-replay.
            execute_motion_with_block_vcol(ed, Motion::ViewportTop, count);
        }
        hjkl_engine::MotionKind::ViewportMiddle => {
            // `M` — cursor to middle of visible viewport; count ignored.
            // Engine FSM arm for `M` in parse_motion is kept intact for macro-replay.
            execute_motion_with_block_vcol(ed, Motion::ViewportMiddle, count);
        }
        hjkl_engine::MotionKind::ViewportBottom => {
            // `L` — cursor to bottom of visible viewport, then count-1 rows up.
            // Engine FSM arm for `L` in parse_motion is kept intact for macro-replay.
            execute_motion_with_block_vcol(ed, Motion::ViewportBottom, count);
        }
        hjkl_engine::MotionKind::HalfPageDown => {
            // `<C-d>` — half page down, count multiplies the distance.
            // Calls scroll_cursor_rows directly rather than adding a Motion enum
            // variant, keeping engine Motion churn minimal.
            {
                let d = ed.viewport_half_rows(count) as isize;
                ed.scroll_cursor_rows(d);
            }
        }
        hjkl_engine::MotionKind::HalfPageUp => {
            // `<C-u>` — half page up, count multiplies the distance.
            // Direct call mirrors the FSM Ctrl-u arm. No new Motion variant.
            {
                let d = -(ed.viewport_half_rows(count) as isize);
                ed.scroll_cursor_rows(d);
            }
        }
        hjkl_engine::MotionKind::FullPageDown => {
            // `<C-f>` — full page down (2-line overlap), count multiplies.
            // Direct call mirrors the FSM Ctrl-f arm. No new Motion variant.
            {
                let d = ed.viewport_full_rows(count) as isize;
                ed.scroll_cursor_rows(d);
            }
        }
        hjkl_engine::MotionKind::FullPageUp => {
            // `<C-b>` — full page up (2-line overlap), count multiplies.
            // Direct call mirrors the FSM Ctrl-b arm. No new Motion variant.
            {
                let d = -(ed.viewport_full_rows(count) as isize);
                ed.scroll_cursor_rows(d);
            }
        }
        hjkl_engine::MotionKind::FirstNonBlankLine => {
            execute_motion_with_block_vcol(ed, Motion::FirstNonBlankLine, count);
        }
        hjkl_engine::MotionKind::SectionBackward => {
            execute_motion_with_block_vcol(ed, Motion::SectionBackward, count);
        }
        hjkl_engine::MotionKind::SectionForward => {
            execute_motion_with_block_vcol(ed, Motion::SectionForward, count);
        }
        hjkl_engine::MotionKind::SectionEndBackward => {
            execute_motion_with_block_vcol(ed, Motion::SectionEndBackward, count);
        }
        hjkl_engine::MotionKind::SectionEndForward => {
            execute_motion_with_block_vcol(ed, Motion::SectionEndForward, count);
        }
        // `MotionKind` is `#[non_exhaustive]` and now lives in another crate, so
        // the engine can add a motion this FSM has never heard of. Ignoring it
        // is the honest response: a discipline cannot execute a motion it has
        // no binding for. Every variant that exists today is handled above.
        _ => {}
    }
}
/// Restore the cursor to the sticky column after vertical motions and
/// sync the sticky column to the current column after horizontal ones.
/// `pre_col` is the cursor column captured *before* the motion — used
/// to bootstrap the sticky value on the very first motion.
pub(crate) fn apply_sticky_col<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    motion: &Motion,
    pre_col: usize,
) {
    if is_vertical_motion(motion) {
        let want = ed.sticky_col().unwrap_or(pre_col);
        // Record the desired column so the next vertical motion sees
        // it even if we currently clamped to a shorter row.
        ed.set_sticky_col(Some(want));
        let (row, _) = ed.cursor();
        let line_len = buf_line_chars(ed.buffer(), row);
        // Clamp to the last char on non-empty lines (vim normal-mode
        // never parks the cursor one past end of line). Empty lines
        // collapse to col 0.
        let max_col = line_len.saturating_sub(1);
        let target = want.min(max_col);
        // raw primitive: this function MUST preserve the un-clamped `want`
        // already stored in `ed.sticky_col()`; `jump_cursor` would overwrite
        // it with the clamped `target`.
        buf_set_cursor_rc(ed.buffer_mut(), row, target);
    } else {
        // Horizontal motion or non-motion: sticky column tracks the
        // new cursor column so the *next* vertical motion aims there.
        ed.set_sticky_col(Some(ed.cursor().1));
    }
}
pub(crate) fn is_vertical_motion(motion: &Motion) -> bool {
    // Only j / k preserve the sticky column. Everything else (search,
    // gg / G, word jumps, etc.) lands at the match's own column so the
    // sticky value should sync to the new cursor column.
    matches!(
        motion,
        Motion::Up | Motion::Down | Motion::ScreenUp | Motion::ScreenDown
    )
}
pub(crate) fn apply_motion_cursor<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    motion: &Motion,
    count: usize,
) {
    apply_motion_cursor_ctx(ed, motion, count, false)
}
pub(crate) fn apply_motion_cursor_ctx<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    motion: &Motion,
    count: usize,
    as_operator: bool,
) {
    // Clamp the count where it fans out into the per-motion `0..count` walk.
    // Two bounds:
    //  - vim's documented ceiling (`:h count`) for folded counts; and
    //  - the buffer's character count, since a motion can never make progress
    //    past the end of the buffer — without this a pathological prefix
    //    (`999999999w`, `<big>dw`) would spin the walk up to ~1e9 times,
    //    freezing the UI, even though the result is identical to stopping at
    //    the buffer edge.
    let count = count
        .min(MAX_COUNT)
        .min(ed.buffer().rope().len_chars().saturating_add(1));
    match motion {
        Motion::Left => {
            // `h` — View clamps at col 0 (no wrap), matching vim.
            hjkl_engine::motions::move_left(ed.buffer_mut(), count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::Right => {
            // `l` — operator-motion context (`dl`/`cl`/`yl`) is allowed
            // one past the last char so the range includes it; cursor
            // context clamps at the last char.
            if as_operator {
                hjkl_engine::motions::move_right_to_end(ed.buffer_mut(), count);
            } else {
                hjkl_engine::motions::move_right_in_line(ed.buffer_mut(), count);
            }
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::SpaceFwd => {
            // `<Space>` — wraps to next line at EOL in cursor context; mid-line
            // char delete like `l` under an operator (`d<Space>`).
            if as_operator {
                hjkl_engine::motions::move_right_to_end(ed.buffer_mut(), count);
            } else {
                hjkl_engine::motions::move_space_fwd(ed.buffer_mut(), count);
            }
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::BackspaceBack => {
            // `<BS>` — wraps to prev line's last char at BOL in cursor context;
            // mid-line char move like `h` under an operator (`d<BS>`).
            if as_operator {
                hjkl_engine::motions::move_left(ed.buffer_mut(), count);
            } else {
                hjkl_engine::motions::move_backspace_back(ed.buffer_mut(), count);
            }
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::Up => {
            // Final col is set by `apply_sticky_col` below — push the
            // post-move row to the textarea and let sticky tracking
            // finish the work.
            let folds = hjkl_engine::SnapshotFoldProvider::from_buffer(ed.buffer());
            let mut sticky = ed.sticky_col();
            hjkl_engine::motions::move_up(ed.buffer_mut(), &folds, count, &mut sticky);
            ed.set_sticky_col(sticky);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::Down => {
            let folds = hjkl_engine::SnapshotFoldProvider::from_buffer(ed.buffer());
            let mut sticky = ed.sticky_col();
            hjkl_engine::motions::move_down(ed.buffer_mut(), &folds, count, &mut sticky);
            ed.set_sticky_col(sticky);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::ScreenUp => {
            let v = *ed.host().viewport();
            let folds = hjkl_engine::SnapshotFoldProvider::from_buffer(ed.buffer());
            let mut sticky = ed.sticky_col();
            hjkl_engine::motions::move_screen_up(ed.buffer_mut(), &folds, &v, count, &mut sticky);
            ed.set_sticky_col(sticky);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::ScreenDown => {
            let v = *ed.host().viewport();
            let folds = hjkl_engine::SnapshotFoldProvider::from_buffer(ed.buffer());
            let mut sticky = ed.sticky_col();
            hjkl_engine::motions::move_screen_down(ed.buffer_mut(), &folds, &v, count, &mut sticky);
            ed.set_sticky_col(sticky);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::WordFwd => {
            let iskeyword = ed.settings().iskeyword.clone();
            hjkl_engine::motions::move_word_fwd(ed.buffer_mut(), false, count, &iskeyword);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::WordBack => {
            let iskeyword = ed.settings().iskeyword.clone();
            hjkl_engine::motions::move_word_back(ed.buffer_mut(), false, count, &iskeyword);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::WordEnd => {
            let iskeyword = ed.settings().iskeyword.clone();
            hjkl_engine::motions::move_word_end(ed.buffer_mut(), false, count, &iskeyword);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::BigWordFwd => {
            let iskeyword = ed.settings().iskeyword.clone();
            hjkl_engine::motions::move_word_fwd(ed.buffer_mut(), true, count, &iskeyword);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::BigWordBack => {
            let iskeyword = ed.settings().iskeyword.clone();
            hjkl_engine::motions::move_word_back(ed.buffer_mut(), true, count, &iskeyword);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::BigWordEnd => {
            let iskeyword = ed.settings().iskeyword.clone();
            hjkl_engine::motions::move_word_end(ed.buffer_mut(), true, count, &iskeyword);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::WordEndBack => {
            let iskeyword = ed.settings().iskeyword.clone();
            hjkl_engine::motions::move_word_end_back(ed.buffer_mut(), false, count, &iskeyword);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::BigWordEndBack => {
            let iskeyword = ed.settings().iskeyword.clone();
            hjkl_engine::motions::move_word_end_back(ed.buffer_mut(), true, count, &iskeyword);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::LineStart => {
            hjkl_engine::motions::move_line_start(ed.buffer_mut());
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::FirstNonBlank => {
            hjkl_engine::motions::move_first_non_blank(ed.buffer_mut());
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::LineEnd => {
            // Vim normal-mode `$` lands on the last char, not one past it.
            hjkl_engine::motions::move_line_end(ed.buffer_mut());
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::FileTop => {
            // `count gg` jumps to line `count` (first non-blank);
            // bare `gg` lands at the top.
            if count > 1 {
                hjkl_engine::motions::move_bottom(ed.buffer_mut(), count);
            } else {
                hjkl_engine::motions::move_top(ed.buffer_mut());
            }
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::FileBottom => {
            // `count G` jumps to line `count`; bare `G` lands at
            // the buffer bottom (`View::move_bottom(0)`).
            if count > 1 {
                hjkl_engine::motions::move_bottom(ed.buffer_mut(), count);
            } else {
                hjkl_engine::motions::move_bottom(ed.buffer_mut(), 0);
            }
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::Find { ch, forward, till } => {
            // Skip an adjacent target when this is a `;`/`,` repeat, and on the
            // 2nd..Nth step of a counted `t`/`T` (the cursor lands one cell
            // short each time, so a naive repeat would stick).
            let repeat = std::mem::take(&mut vim_mut(ed).find_repeat_skip);
            for i in 0..count {
                let skip_adjacent = repeat || i > 0;
                if !find_char_on_line(ed, *ch, *forward, *till, skip_adjacent) {
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
            if let Some(pattern) = ed.last_search_pattern() {
                ed.push_search_pattern(&pattern);
            }
            if ed.search_state().pattern.is_none() {
                return;
            }
            // `n` repeats the last search in its committed direction;
            // `N` inverts. So a `?` search makes `n` walk backward and
            // `N` walk forward.
            let forward = ed.last_search_forward() != *reverse;
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
            hjkl_engine::motions::move_viewport_top(ed.buffer_mut(), &v, count.saturating_sub(1));
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::ViewportMiddle => {
            let v = *ed.host().viewport();
            hjkl_engine::motions::move_viewport_middle(ed.buffer_mut(), &v);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::ViewportBottom => {
            let v = *ed.host().viewport();
            hjkl_engine::motions::move_viewport_bottom(
                ed.buffer_mut(),
                &v,
                count.saturating_sub(1),
            );
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::LastNonBlank => {
            hjkl_engine::motions::move_last_non_blank(ed.buffer_mut());
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::LineMiddle => {
            let row = ed.cursor().0;
            let line_chars = buf_line_chars(ed.buffer(), row);
            // Vim's `gM`: column = floor(chars / 2). Empty / single-char
            // lines stay at col 0.
            let target = line_chars / 2;
            ed.jump_cursor(row, target);
        }
        Motion::ScreenLineMiddle => {
            // Vim's `gm`: middle of the *screen* line = column
            // `viewport_width / 2`, clamped to the last char of the line.
            let row = ed.cursor().0;
            let width = ed.host().viewport().width as usize;
            let last = buf_line_chars(ed.buffer(), row).saturating_sub(1);
            let target = (width / 2).min(last);
            ed.jump_cursor(row, target);
        }
        Motion::ParagraphPrev => {
            hjkl_engine::motions::move_paragraph_prev(ed.buffer_mut(), count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::ParagraphNext => {
            hjkl_engine::motions::move_paragraph_next(ed.buffer_mut(), count);
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
            hjkl_engine::motions::move_section_backward(ed.buffer_mut(), count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::SectionForward => {
            hjkl_engine::motions::move_section_forward(ed.buffer_mut(), count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::SectionEndBackward => {
            hjkl_engine::motions::move_section_end_backward(ed.buffer_mut(), count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::SectionEndForward => {
            hjkl_engine::motions::move_section_end_forward(ed.buffer_mut(), count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::FirstNonBlankNextLine => {
            hjkl_engine::motions::move_first_non_blank_next_line(ed.buffer_mut(), count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::FirstNonBlankPrevLine => {
            hjkl_engine::motions::move_first_non_blank_prev_line(ed.buffer_mut(), count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::FirstNonBlankLine => {
            hjkl_engine::motions::move_first_non_blank_line(ed.buffer_mut(), count);
            ed.push_buffer_cursor_to_textarea();
        }
        Motion::GotoColumn => {
            hjkl_engine::motions::move_goto_column(ed.buffer_mut(), count);
            ed.push_buffer_cursor_to_textarea();
        }
    }
}
pub(crate) fn move_first_non_whitespace<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) {
    // Some call sites invoke this right after `dd` / `<<` / `>>` etc
    // mutates the textarea content, so the migration buffer hasn't
    // seen the new lines OR new cursor yet. Mirror the full content
    // across before delegating, then push the result back so the
    // textarea reflects the resolved column too.
    ed.sync_buffer_content_from_textarea();
    hjkl_engine::motions::move_first_non_blank(ed.buffer_mut());
    ed.push_buffer_cursor_to_textarea();
}
pub(crate) fn find_char_on_line<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    ch: char,
    forward: bool,
    till: bool,
    skip_adjacent: bool,
) -> bool {
    let moved =
        hjkl_engine::motions::find_char_on_line(ed.buffer_mut(), ch, forward, till, skip_adjacent);
    if moved {
        ed.push_buffer_cursor_to_textarea();
    }
    moved
}
pub(crate) fn matching_bracket<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    let moved = hjkl_engine::motions::match_bracket(ed.buffer_mut());
    if moved {
        ed.push_buffer_cursor_to_textarea();
    }
    moved
}
/// `[(` / `])` / `[{` / `]}` — move to the `count`-th previous (`forward =
/// false`) / next (`forward = true`) unmatched bracket of the kind given by
/// `open` (`(` or `{`). Balanced inner pairs are skipped via a depth counter.
pub(crate) fn goto_unmatched_bracket<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    forward: bool,
    open: char,
    count: usize,
) {
    let close = match open {
        '(' => ')',
        '{' => '}',
        _ => return,
    };
    let cursor = buf_cursor_pos(ed.buffer());
    let rows = buf_row_count(ed.buffer());
    let target = count.max(1);
    let mut found = 0usize;
    let mut depth = 0i32;

    if forward {
        let mut r = cursor.row;
        let mut from_col = cursor.col + 1;
        while r < rows {
            let line: Vec<char> = buf_line(ed.buffer(), r)
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
                            buf_set_cursor_rc(ed.buffer_mut(), r, ci);
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
            let line: Vec<char> = buf_line(ed.buffer(), r as usize)
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
                            buf_set_cursor_rc(ed.buffer_mut(), r as usize, ci as usize);
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
pub(crate) fn word_at_cursor_search<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    forward: bool,
    whole_word: bool,
    count: usize,
) {
    let (row, col) = ed.cursor();
    let line: String = buf_line(ed.buffer(), row).unwrap_or_default();
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
    ed.set_last_search_pattern_only(Some(pattern));
    ed.set_last_search_forward_only(forward);
    for _ in 0..count.max(1) {
        if forward {
            ed.search_advance_forward(true);
        } else {
            ed.search_advance_backward(true);
        }
    }
    ed.push_buffer_cursor_to_textarea();
}
pub(crate) fn regex_escape(s: &str) -> String {
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
