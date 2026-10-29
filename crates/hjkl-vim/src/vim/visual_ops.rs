//! Vim FSM: visual ops.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{InsertReason, LastChange, Mode, Motion, Operator, RangeKind, VisualExtent};

use hjkl_engine::rope_util::rope_line_to_str;

use super::*;
use crate::vim_state::{vim, vim_mut};
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{
    buf_cursor_pos, buf_line, buf_line_chars, buf_row_count, buf_set_cursor_rc,
};

pub fn apply_visual_operator<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    count: usize,
) {
    include_closed_folds_in_selection(ed);
    visual_operator_body(ed, op, count);
    remember_operator_column(ed);
}

/// vim's `do_pending_operator` in Visual mode: a selection end inside a
/// closed fold takes the whole fold. The earlier end moves to the fold's first
/// line, column 0, and the later one to its last line, past the last
/// character, so `zfjvd` deletes both folded lines with their line breaks.
fn include_closed_folds_in_selection<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) {
    let mode = vim(ed).mode;
    let anchor = match mode {
        Mode::Visual => vim(ed).visual_anchor,
        Mode::VisualLine => (vim(ed).visual_line_anchor, 0),
        Mode::VisualBlock => vim(ed).block_anchor,
        _ => return,
    };
    let cursor = ed.cursor();
    let fold_edge = |ed: &Editor<hjkl_buffer::View, H>, pos: (usize, usize), last: bool| {
        let (first, end) = expand_linewise_over_closed_folds(ed.buffer(), pos.0, pos.0);
        if first == end {
            pos
        } else if last {
            (end, buf_line_chars(ed.buffer(), end))
        } else {
            (first, 0)
        }
    };
    let anchor_first = anchor < cursor;
    let new_anchor = fold_edge(ed, anchor, !anchor_first);
    let new_cursor = fold_edge(ed, cursor, anchor_first);
    if (new_anchor, new_cursor) == (anchor, cursor) {
        return;
    }
    match mode {
        Mode::Visual => vim_mut(ed).visual_anchor = new_anchor,
        Mode::VisualLine => vim_mut(ed).visual_line_anchor = new_anchor.0,
        _ => {
            vim_mut(ed).block_anchor = new_anchor;
            vim_mut(ed).block_vcol = new_cursor.1;
        }
    }
    buf_set_cursor_rc(ed.buffer_mut(), new_cursor.0, new_cursor.1);
}

/// vim sets `'curswant'` from the cursor once an operator is done
/// (`w_set_curswant`): a following `j` / `k` aims for the column the
/// operator left the cursor in, not the one it had before. The sticky
/// column is a display column, so a tab before the cursor counts at its
/// width. An operator that entered Insert mode leaves that to the end of the
/// insert.
pub fn remember_operator_column<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) {
    if vim(ed).mode == Mode::Normal {
        remember_cursor_column(ed);
    }
}

/// Make the cursor's display column the sticky column, as vim does after
/// any command that sets `w_set_curswant`: a tab before the cursor counts
/// at its width, and a cursor on a tab in Normal mode at its last cell.
pub fn remember_cursor_column<H: hjkl_engine::types::Host>(ed: &mut Editor<hjkl_buffer::View, H>) {
    let (row, col) = ed.cursor();
    let want = ed.cursor_want(row, col);
    ed.set_sticky_col(Some(want));
}

/// Where a Visual operator starts: the selection's first character, the
/// Visual-line anchor's column 0 (or the cursor when it is above the
/// anchor), or a block's top-left corner. The undo step saves it, and a
/// refused operator leaves the cursor there.
pub fn visual_op_start<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
) -> (usize, usize) {
    let cursor = ed.cursor();
    match vim(ed).mode {
        Mode::Visual => order(vim(ed).visual_anchor, cursor).0,
        Mode::VisualLine if cursor.0 < vim(ed).visual_line_anchor => cursor,
        Mode::VisualLine => (vim(ed).visual_line_anchor, 0),
        Mode::VisualBlock => {
            let (top, _, left, _) = block_bounds(ed);
            (top, left)
        }
        _ => cursor,
    }
}

fn visual_operator_body<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    count: usize,
) {
    // `count` is the number of indent levels for `>` / `<` (vim `2>` = two
    // shiftwidths); other visual operators ignore it.
    let levels = count.max(1);
    // Read the explicit register before an operator consumes it, so the
    // dot-repeat entry can name it (`:h redo-register`).
    let register = vim(ed).pending_register;
    if matches!(op, Operator::Delete | Operator::Yank | Operator::Change)
        && refuse_read_only_register(ed)
    {
        let start = visual_op_start(ed);
        refused_operator(ed, op, start, RangeKind::Exclusive);
        return;
    }
    match vim(ed).mode {
        Mode::VisualLine => {
            let (cursor_row, cursor_col) = ed.cursor();
            let anchor_row = vim(ed).visual_line_anchor;
            let top = cursor_row.min(anchor_row);
            let bot = cursor_row.max(anchor_row);
            ed.set_yank_linewise(true);
            // vim takes the Visual-line anchor at column 0, and the earlier of
            // it and the cursor as the operator's start — the cursor the undo
            // step saves and where a yank leaves it: `V>u` returns to
            // (top, 0), `Vk>u` and `Vky` to the cursor. `gq` / `gw` keep the
            // cursor.
            let start_col = if cursor_row < anchor_row {
                cursor_col
            } else {
                0
            };
            if !matches!(op, Operator::Reflow | Operator::ReflowKeepCursor) {
                buf_set_cursor_rc(ed.buffer_mut(), top, start_col);
            }
            match op {
                Operator::Yank => {
                    let text = read_vim_range(ed, (top, 0), (bot, 0), RangeKind::Linewise);
                    if !text.is_empty() {
                        ed.record_yank_to_host(text.clone());
                        let target = vim_mut(ed).pending_register.take();
                        ed.record_yank(text, true, target);
                    }
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::Delete => {
                    ed.push_undo();
                    // Capture `total` (incl. ropey's phantom trailing row)
                    // before the cut so the phantom-row clamp below can decide
                    // where the cursor lands. Same reasoning as the `dd`
                    // (linewise.rs) and `run_operator_over_range` (op_motion.rs)
                    // delete paths — a linewise Visual delete reaching the true
                    // last row must not park the cursor on the phantom empty
                    // final line.
                    let total = buf_row_count(ed.buffer());
                    let deleted_through_last = bot + 1 >= total;
                    cut_vim_range(ed, (top, 0), (bot, 0), RangeKind::Linewise);
                    land_after_linewise_delete(ed, top, deleted_through_last);
                    record_visual_last_change(
                        ed,
                        op,
                        VisualExtent::Line {
                            lines: bot - top + 1,
                        },
                        register,
                        1,
                    );
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::Change => {
                    // Vim `Vc` / `Vjc`: same linewise-change semantics as
                    // `cc` — preserve first line's indent, enter insert.
                    // Record BEFORE entering insert: `AfterChange` patches
                    // `inserted` into this entry when the user hits Esc.
                    record_visual_last_change(
                        ed,
                        op,
                        VisualExtent::Line {
                            lines: bot - top + 1,
                        },
                        register,
                        1,
                    );
                    change_linewise_rows(ed, top, bot, ed.cursor().1);
                }
                Operator::Uppercase
                | Operator::Lowercase
                | Operator::ToggleCase
                | Operator::Rot13 => {
                    apply_case_op_to_selection(ed, op, (top, 0), (bot, 0), RangeKind::Linewise);
                    buf_set_cursor_rc(ed.buffer_mut(), top, 0);
                    record_visual_last_change(
                        ed,
                        op,
                        VisualExtent::Line {
                            lines: bot - top + 1,
                        },
                        register,
                        1,
                    );
                }
                Operator::Indent | Operator::Outdent => {
                    ed.push_undo();
                    if op == Operator::Indent {
                        indent_rows(ed, top, bot, levels);
                    } else {
                        outdent_rows(ed, top, bot, levels);
                    }
                    record_visual_last_change(
                        ed,
                        op,
                        VisualExtent::Line {
                            lines: bot - top + 1,
                        },
                        register,
                        levels,
                    );
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::Reflow => {
                    ed.push_undo();
                    let (cursor_row, _) = ed.cursor();
                    let bot = cursor_row.max(vim(ed).visual_line_anchor);
                    reflow_rows(ed, top, bot);
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::ReflowKeepCursor => {
                    let saved = ed.cursor();
                    ed.push_undo();
                    let (cursor_row, _) = ed.cursor();
                    let bot = cursor_row.max(vim(ed).visual_line_anchor);
                    let (before, after, opts) = reflow_rows_keep_cursor(ed, top, bot);
                    let (new_row, new_col) =
                        reflow_keep_cursor(top, saved.0, saved.1, &before, &after, opts);
                    buf_set_cursor_rc(ed.buffer_mut(), new_row, new_col);
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::AutoIndent => {
                    ed.push_undo();
                    auto_indent_rows(ed, top, bot);
                    vim_mut(ed).mode = Mode::Normal;
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
            ed.set_yank_linewise(false);
            let anchor = vim(ed).visual_anchor;
            let cursor = ed.cursor();
            let (top, bot) = order(anchor, cursor);
            // As in Visual-line: the selection's start is the operator's
            // start, the cursor the undo step saves (`vll~u` returns to the
            // first selected character). `gq` / `gw` keep the cursor.
            if !matches!(op, Operator::Reflow | Operator::ReflowKeepCursor) {
                buf_set_cursor_rc(ed.buffer_mut(), top.0, top.1);
            }
            match op {
                Operator::Yank => {
                    let (from, to, kind) = charwise_visual_range(ed, top, bot);
                    let text = read_vim_range(ed, from, to, kind);
                    if !text.is_empty() {
                        ed.record_yank_to_host(text.clone());
                        let target = vim_mut(ed).pending_register.take();
                        ed.record_yank(text, false, target);
                    }
                    buf_set_cursor_rc(ed.buffer_mut(), top.0, top.1);
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::Delete => {
                    ed.push_undo();
                    let (from, to, kind) = charwise_visual_range(ed, top, bot);
                    cut_vim_range(ed, from, to, kind);
                    // A cut through the end of the line leaves the cursor past
                    // the new end; Normal mode puts it on the last character.
                    clamp_cursor_to_normal_mode(ed);
                    record_visual_last_change(ed, op, charwise_extent(top, bot), register, 1);
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::Change => {
                    ed.push_undo();
                    // Record BEFORE entering insert: `AfterChange` patches
                    // `inserted` into this entry when the user hits Esc.
                    record_visual_last_change(ed, op, charwise_extent(top, bot), register, 1);
                    let (from, to, kind) = charwise_visual_range(ed, top, bot);
                    cut_vim_range(ed, from, to, kind);
                    begin_insert_noundo(ed, 1, InsertReason::AfterChange);
                }
                Operator::Uppercase
                | Operator::Lowercase
                | Operator::ToggleCase
                | Operator::Rot13 => {
                    let (from, to, kind) = charwise_visual_range(ed, top, bot);
                    apply_case_op_to_selection(ed, op, from, to, kind);
                    record_visual_last_change(ed, op, charwise_extent(top, bot), register, 1);
                }
                Operator::Indent | Operator::Outdent => {
                    ed.push_undo();
                    if op == Operator::Indent {
                        indent_rows(ed, top.0, bot.0, levels);
                    } else {
                        outdent_rows(ed, top.0, bot.0, levels);
                    }
                    // Indent/outdent always act linewise regardless of the
                    // originating charwise selection (`:h v_>` — columns are
                    // ignored), so the dot-repeat extent is Line, not Char.
                    record_visual_last_change(
                        ed,
                        op,
                        VisualExtent::Line {
                            lines: bot.0 - top.0 + 1,
                        },
                        register,
                        levels,
                    );
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::Reflow => {
                    ed.push_undo();
                    let anchor = vim(ed).visual_anchor;
                    let cursor = ed.cursor();
                    let (top, bot) = order(anchor, cursor);
                    reflow_rows(ed, top.0, bot.0);
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::ReflowKeepCursor => {
                    let saved = ed.cursor();
                    ed.push_undo();
                    let anchor = vim(ed).visual_anchor;
                    let cursor = ed.cursor();
                    let (top, bot) = order(anchor, cursor);
                    let (before, after, opts) = reflow_rows_keep_cursor(ed, top.0, bot.0);
                    let (new_row, new_col) =
                        reflow_keep_cursor(top.0, saved.0, saved.1, &before, &after, opts);
                    buf_set_cursor_rc(ed.buffer_mut(), new_row, new_col);
                    vim_mut(ed).mode = Mode::Normal;
                }
                Operator::AutoIndent => {
                    ed.push_undo();
                    let anchor = vim(ed).visual_anchor;
                    let cursor = ed.cursor();
                    let (top, bot) = order(anchor, cursor);
                    auto_indent_rows(ed, top.0, bot.0);
                    vim_mut(ed).mode = Mode::Normal;
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
/// B1: record `LastChange::VisualOp` for dot-repeat, unless this call is
/// itself a replay (`.` must not overwrite the entry it's replaying).
fn record_visual_last_change<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    extent: VisualExtent,
    register: Option<char>,
    levels: usize,
) {
    if !vim(ed).replaying {
        vim_mut(ed).last_change = Some(LastChange::VisualOp {
            op,
            extent,
            inserted: None,
            register,
            levels,
        });
    }
}
/// The range a charwise Visual selection `top..=bot` covers. On an empty
/// line the selection's one cell is the line break, which vim yanks and
/// changes with it (`vy` there yanks "\n"); as an exclusive range ending at
/// the next line's start, reading and cutting agree on that. The buffer's
/// last line has no break: ropey's phantom row after a trailing newline is
/// not a line.
pub fn charwise_visual_range<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    top: (usize, usize),
    bot: (usize, usize),
) -> ((usize, usize), (usize, usize), RangeKind) {
    let rows = buf_row_count(ed.buffer());
    let next_is_line =
        bot.0 + 1 < rows && !(bot.0 + 2 == rows && buf_line_chars(ed.buffer(), bot.0 + 1) == 0);
    if bot.1 >= buf_line_chars(ed.buffer(), bot.0) && next_is_line {
        (top, (bot.0 + 1, 0), RangeKind::Exclusive)
    } else {
        (top, bot, RangeKind::Inclusive)
    }
}
/// B1: derive the charwise dot-repeat extent from an ordered `(top, bot)`
/// INCLUSIVE range (`:h v_.`). Single-line selections store the raw
/// selected char count as `width` (replay selects that many chars starting
/// at the cursor); multi-line selections store the last line's char count
/// measured from column 0 (replay's last line always starts at column 0 —
/// only the first line tracks the cursor's column).
fn charwise_extent(top: (usize, usize), bot: (usize, usize)) -> VisualExtent {
    if top.0 == bot.0 {
        VisualExtent::Char {
            lines: 1,
            width: bot.1 - top.1 + 1,
        }
    } else {
        VisualExtent::Char {
            lines: bot.0 - top.0 + 1,
            width: bot.1 + 1,
        }
    }
}
/// B-block: derive the blockwise dot-repeat extent (`:h v_.` for blocks)
/// from an ordered inclusive rectangle. `rows`/`cols` are the height/width;
/// `to_eol` carries the `$`-ragged flag so replay re-runs to each row's own
/// EOL (`:h v_b_$`).
fn block_extent(top: usize, bot: usize, left: usize, right: usize, to_eol: bool) -> VisualExtent {
    VisualExtent::Block {
        rows: bot - top + 1,
        cols: right + 1 - left,
        to_eol,
    }
}
/// Compute `(top_row, bot_row, left_col, right_col)` for the current
/// VisualBlock selection. Columns are inclusive on both ends. Uses the
/// tracked virtual column (see [`update_block_vcol`] for when it moves)
/// rather than reading the cursor directly.
pub fn block_bounds<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
) -> (usize, usize, usize, usize) {
    let (ar, ac) = vim(ed).block_anchor;
    let (cr, _) = ed.cursor();
    let cc = vim(ed).block_vcol;
    let top = ar.min(cr);
    let bot = ar.max(cr);
    let left = ac.min(cc);
    let right = ac.max(cc);
    (top, bot, left, right)
}
/// Update the virtual column after a motion in VisualBlock mode.
/// Horizontal motions and `j` / `k` sync `block_vcol` to the new cursor
/// column; search and the other motions leave it alone.
///
/// `$` (`Motion::LineEnd`) additionally marks the block "ragged" (`:h
/// v_b_$`, [`crate::vim_state::VimState::block_to_eol`]) — every row then
/// resolves its own right edge to its own EOL until a DIFFERENT
/// horizontal motion re-establishes a fixed column, at which point the
/// flag clears. `j` / `k` preserve it, matching vim.
pub fn update_block_vcol<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    motion: &Motion,
) {
    match motion {
        Motion::LineEnd => {
            vim_mut(ed).block_vcol = ed.cursor().1;
            vim_mut(ed).block_to_eol = true;
        }
        Motion::Left
        | Motion::Right
        | Motion::SpaceFwd
        | Motion::BackspaceBack
        | Motion::WordFwd
        | Motion::BigWordFwd
        | Motion::WordBack
        | Motion::BigWordBack
        | Motion::WordEnd
        | Motion::BigWordEnd
        | Motion::WordEndBack
        | Motion::BigWordEndBack
        | Motion::LineStart
        | Motion::Home
        | Motion::FirstNonBlank
        | Motion::LastNonBlank
        | Motion::LineMiddle
        | Motion::ScreenLineMiddle
        | Motion::ScreenLineStart
        | Motion::ScreenLineFirstNonBlank
        | Motion::ScreenLineEnd
        | Motion::WordAtCursor { .. }
        | Motion::Find { .. }
        | Motion::FindRepeat { .. }
        | Motion::MatchBracket
        // `gg` / `G` reset vim's curswant to where they land, so the block's
        // column follows them: `<C-v>Gd` takes columns 0 to the anchor's.
        // `H` / `M` / `L` would too by the same measurement, but a corpus
        // case pins the old column for them (see `docs/backlog.md`).
        | Motion::FileTop
        | Motion::FileBottom
        // So do the other jumps that set curswant: `{` `}` `(` `)`, the
        // section and unmatched-bracket motions, `+` `-` `_`, `|`, `n` `N`.
        | Motion::ParagraphPrev
        | Motion::ParagraphNext
        | Motion::SentencePrev
        | Motion::SentenceNext
        | Motion::SectionBackward
        | Motion::SectionForward
        | Motion::SectionEndBackward
        | Motion::SectionEndForward
        | Motion::UnmatchedBracket { .. }
        | Motion::FirstNonBlankNextLine
        | Motion::FirstNonBlankPrevLine
        | Motion::FirstNonBlankLine
        | Motion::GotoColumn
        | Motion::SearchNext { .. } => {
            vim_mut(ed).block_vcol = ed.cursor().1;
            vim_mut(ed).block_to_eol = false;
        }
        // `j` / `k` — vim's `block_prep` reads the cursor's own column, which
        // the curswant puts back once the cursor reaches a long enough line,
        // so on a shorter line the block narrows to where the cursor landed
        // (the NUL, in Visual). The ragged flag survives.
        Motion::Up | Motion::Down => {
            vim_mut(ed).block_vcol = ed.cursor().1;
        }
        // `gj` / `gk` and `H` / `M` / `L` — preserve vcol AND the ragged flag.
        _ => {}
    }
}
pub fn visual_selection_for_search<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> ((usize, usize), String) {
    match vim(ed).mode {
        Mode::Visual => {
            let (top, bot) = order(vim(ed).visual_anchor, ed.cursor());
            (top, read_vim_range(ed, top, bot, RangeKind::Inclusive))
        }
        Mode::VisualLine => {
            let top = vim(ed).visual_line_anchor.min(ed.cursor().0);
            let bot = vim(ed).visual_line_anchor.max(ed.cursor().0);
            let text = read_vim_range(ed, (top, 0), (bot, 0), RangeKind::Linewise);
            (
                (top, 0),
                text.strip_suffix('\n').unwrap_or(&text).to_owned(),
            )
        }
        Mode::VisualBlock => {
            let (top, bot, cols) = block_rect(ed);
            (
                (top, block_start_col(ed, top, cols)),
                block_yank(ed, top, bot, cols),
            )
        }
        Mode::Normal | Mode::Insert => (ed.cursor(), String::new()),
    }
}

/// Yank / delete / change / replace a rectangular selection. Yanked text
/// is stored as one string per row joined with `\n`, but the register is
/// flagged BLOCKWISE (with the block's column width) so `p`/`P` re-insert
/// it as columns at the cursor — vim's true block-paste geometry — rather
/// than spilling each row onto its own new line (see the `blockwise` slot
/// flag and `do_block_paste`). The joined-`\n` string is also what the RPC
/// / charwise-fallback paths read.
pub fn apply_block_operator<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    count: usize,
) {
    // Same capture as `apply_visual_operator` — this is also reached
    // directly from `range_ops`, so it cannot rely on that one.
    let register = vim(ed).pending_register;
    let (top, bot, left, right) = block_bounds(ed);
    let (_, _, cols) = block_rect(ed);
    let left_col =
        block_start_col(ed, top, cols).min(buf_line_chars(ed.buffer(), top).saturating_sub(1));
    if matches!(op, Operator::Delete | Operator::Yank | Operator::Change)
        && refuse_read_only_register(ed)
    {
        refused_operator(ed, op, (top, left), RangeKind::Exclusive);
        return;
    }
    // `$` (`:h v_b_$`) makes the block ragged: every row resolves its own
    // right edge to its own EOL instead of the fixed `right` above. Read
    // once here and thread through — vim's `$`-block affects every
    // operator, not just `d`/`y`/`A`.
    let to_eol = vim(ed).block_to_eol;
    // Snapshot the block text for yank / clipboard.
    let yank = block_yank(ed, top, bot, cols);
    let block_width = block_register_width(&yank, cols);
    // vim's `do_pending_operator` puts the cursor on the operator's start
    // before the operator saves its undo step, so `u` returns to the block's
    // top-left corner, not to where the selection ended.
    ed.jump_cursor(top, left_col);

    match op {
        Operator::Yank => {
            if !yank.is_empty() {
                ed.record_yank_to_host(yank.clone());
                let target = vim_mut(ed).pending_register.take();
                ed.record_yank_block(yank, block_width, target);
            }
            vim_mut(ed).mode = Mode::Normal;
            ed.jump_cursor(top, left_col);
        }
        Operator::Delete => {
            ed.push_undo();
            delete_block_contents(ed, top, bot, cols);
            if !yank.is_empty() {
                ed.record_yank_to_host(yank.clone());
                let target = vim_mut(ed).pending_register.take();
                ed.record_delete_block(yank, block_width, target);
            }
            vim_mut(ed).mode = Mode::Normal;
            // Cursor lands on the block's left column, clamped to the row's
            // new last valid column — a ragged (`D` / `$`) delete can leave
            // the top row shorter than `left`, and nvim parks on its EOL
            // rather than past it (`<C-v>jjD` on "abcd…" → (0,0), not (0,1)).
            let line_len = buf_line_chars(ed.buffer(), top);
            ed.jump_cursor(top, left_col.min(line_len.saturating_sub(1)));
            record_visual_last_change(
                ed,
                op,
                block_extent(top, bot, left, right, to_eol),
                register,
                1,
            );
        }
        Operator::Change => {
            ed.push_undo();
            // Record BEFORE entering insert: the `BlockChange` finish site
            // (comment.rs) patches `inserted` into this entry on Esc, exactly
            // as `AfterChange` does for charwise/linewise `c`.
            record_visual_last_change(
                ed,
                op,
                block_extent(top, bot, left, right, to_eol),
                register,
                1,
            );
            // `c`'s replicated typed text always lands at the LEFT column
            // on every row (`:h v_b_c`) — unaffected by a ragged right
            // edge, so only the delete side needs `to_eol`.
            // The typed text goes where the block's left edge now falls on
            // the top row, past any spaces a cut tab left behind.
            let top_line = buf_line(ed.buffer(), top).unwrap_or_default();
            let top_span = block_row(&top_line, cols, ed.settings().tabstop, true);
            let insert_col = top_span.start + top_span.startspaces;
            delete_block_contents(ed, top, bot, cols);
            if !yank.is_empty() {
                ed.record_yank_to_host(yank.clone());
                let target = vim_mut(ed).pending_register.take();
                ed.record_delete_block(yank, block_width, target);
            }
            ed.jump_cursor(top, insert_col);
            begin_insert_noundo(
                ed,
                1,
                InsertReason::BlockChange {
                    top,
                    bot,
                    col: insert_col,
                },
            );
        }
        Operator::Uppercase | Operator::Lowercase | Operator::ToggleCase | Operator::Rot13 => {
            ed.push_undo();
            transform_block_case(ed, op, top, bot, cols);
            vim_mut(ed).mode = Mode::Normal;
            ed.jump_cursor(top, left_col);
            record_visual_last_change(
                ed,
                op,
                block_extent(top, bot, left, right, to_eol),
                register,
                1,
            );
        }
        Operator::Indent | Operator::Outdent => {
            // VisualBlock `>` / `<` shifts at the BLOCK's left column, not at
            // the start of the line. This used to fall through to the
            // linewise `indent_rows` / `outdent_rows` under a comment
            // claiming vim did the same; it does not — `<C-v>jl>` with the
            // block starting at column 2 turns "abcdef" into "ab    cdef",
            // where the linewise fallback produced "    abcdef".
            ed.push_undo();
            if op == Operator::Indent {
                indent_block(ed, top, bot, cols, count.max(1));
            } else {
                outdent_block(ed, top, bot, cols, count.max(1));
            }
            vim_mut(ed).mode = Mode::Normal;
            record_visual_last_change(
                ed,
                op,
                block_extent(top, bot, left, right, to_eol),
                register,
                count.max(1),
            );
        }
        Operator::Fold => unreachable!("Visual zf takes its own path"),
        Operator::Reflow => {
            // Reflow over the block falls back to linewise reflow over
            // the row range — column slicing for `gq` doesn't make
            // sense.
            ed.push_undo();
            reflow_rows(ed, top, bot);
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::ReflowKeepCursor => {
            // `gw` over a block: same fallback as `gq` but restore cursor.
            let saved = ed.cursor();
            ed.push_undo();
            let (before, after, opts) = reflow_rows_keep_cursor(ed, top, bot);
            let (new_row, new_col) =
                reflow_keep_cursor(top, saved.0, saved.1, &before, &after, opts);
            buf_set_cursor_rc(ed.buffer_mut(), new_row, new_col);
            vim_mut(ed).mode = Mode::Normal;
        }
        Operator::AutoIndent => {
            // AutoIndent over the block falls back to linewise
            // auto-indent over the row range.
            ed.push_undo();
            auto_indent_rows(ed, top, bot);
            vim_mut(ed).mode = Mode::Normal;
        }
        // Filter is dispatched through Editor::filter_range, not here.
        Operator::Filter => {}
        // Comment is dispatched through the app layer (engine_actions.rs), not here.
        Operator::Comment => {}
    }
}
/// In-place case transform over the block's rows, each over the characters
/// wholly inside the block's display columns (vim's `op_tilde` over
/// `block_prep`). Rows that end before the block are left alone.
pub fn transform_block_case<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    top: usize,
    bot: usize,
    cols: BlockCols,
) {
    use hjkl_buffer::{Edit, Position};
    let tabstop = ed.settings().tabstop;
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    for r in top..=bot.min(rope.len_lines().saturating_sub(1)) {
        let line = rope_line_to_str(&rope, r);
        let span = block_row(&line, cols, tabstop, false);
        if span.start >= span.end {
            continue;
        }
        let mid: String = line
            .chars()
            .skip(span.start)
            .take(span.end - span.start)
            .collect();
        let transformed = match op {
            Operator::Uppercase => mid.to_uppercase(),
            Operator::Lowercase => mid.to_lowercase(),
            Operator::ToggleCase => toggle_case_str(&mid),
            Operator::Rot13 => rot13_str(&mid),
            _ => mid,
        };
        ed.mutate_edit(Edit::Replace {
            start: Position::new(r, span.start),
            end: Position::new(r, span.end),
            with: transformed,
        });
    }
    let saved_yank = ed.yank();
    let saved_linewise = ed.yank_linewise();
    let start =
        block_start_col(ed, top, cols).min(buf_line_chars(ed.buffer(), top).saturating_sub(1));
    buf_set_cursor_rc(ed.buffer_mut(), top, start);
    ed.set_yank(saved_yank);
    ed.set_yank_linewise(saved_linewise);
}
/// The character the block starts at on `row`: the one holding its left
/// display column, or the row's end when the row stops short of it. Where
/// vim's `do_pending_operator` puts the cursor before a block operator.
pub fn block_start_col<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    row: usize,
    cols: BlockCols,
) -> usize {
    let line = buf_line(ed.buffer(), row).unwrap_or_default();
    block_row(&line, cols, ed.settings().tabstop, true).start
}
/// The block's display columns for a rectangle given in character columns
/// on its top row, as dot-repeat describes one.
pub fn block_cols_at<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    row: usize,
    left: usize,
    right: usize,
    to_eol: bool,
) -> BlockCols {
    let line = buf_line(ed.buffer(), row).unwrap_or_default();
    let tabstop = ed.settings().tabstop;
    let first = |col: usize| hjkl_buffer::char_col_to_visual_col(&line, col, tabstop);
    BlockCols {
        left: first(left),
        right: if to_eol {
            usize::MAX
        } else {
            first(right + 1).max(first(right) + 1) - 1
        },
    }
}
/// Yank the block: on each row the characters wholly inside its display
/// columns, with spaces for the cells of a character straddling an edge, and
/// a row that ends before the block padded to its width (vim's `op_yank` over
/// `block_prep`). A `$` block takes each row to its own end.
pub fn block_yank<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    top: usize,
    bot: usize,
    cols: BlockCols,
) -> String {
    let tabstop = ed.settings().tabstop;
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let n = rope.len_lines();
    let mut rows: Vec<String> = Vec::new();
    for r in top..=bot {
        if r >= n {
            break;
        }
        let line = rope_line_to_str(&rope, r);
        let span = block_row(&line, cols, tabstop, false);
        let mut text = " ".repeat(span.startspaces);
        text.extend(line.chars().skip(span.start).take(span.end - span.start));
        text.push_str(&" ".repeat(span.endspaces));
        rows.push(text);
    }
    rows.join("\n")
}
/// The width a block register pads each row to on paste: the block's
/// display width, or the widest row of a `$` block.
pub fn block_register_width(yank: &str, cols: BlockCols) -> usize {
    if cols.right == usize::MAX {
        yank.split('\n')
            .map(|s| s.chars().count())
            .max()
            .unwrap_or(0)
    } else {
        cols.right + 1 - cols.left
    }
}
/// Delete the block from each row: the characters it covers, a character
/// straddling an edge taken whole with the cells of it outside the block put
/// back as spaces (vim's `op_delete` over `block_prep`). Each row is its own
/// edit in the caller's undo group (no `push_undo` here).
pub fn delete_block_contents<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    top: usize,
    bot: usize,
    cols: BlockCols,
) {
    use hjkl_buffer::{Edit, Position};
    ed.sync_buffer_content_from_textarea();
    let tabstop = ed.settings().tabstop;
    let last_row = bot.min(buf_row_count(ed.buffer()).saturating_sub(1));
    for row in top..=last_row {
        let line = buf_line(ed.buffer(), row).unwrap_or_default();
        let span = block_row(&line, cols, tabstop, true);
        let spaces = span.startspaces + span.endspaces;
        if span.start == span.end && spaces == 0 {
            continue;
        }
        ed.mutate_edit(Edit::Replace {
            start: Position::new(row, span.start),
            end: Position::new(row, span.end),
            with: " ".repeat(spaces),
        });
    }
}
/// Replace each character cell in the block with `ch`. Ragged (`:h
/// v_b_$`) per row when `vim(ed).block_to_eol` is set.
pub fn block_replace<H: hjkl_engine::types::Host>(ed: &mut Editor<hjkl_buffer::View, H>, ch: char) {
    let (top, bot, cols) = block_rect(ed);
    let to_eol = vim(ed).block_to_eol;
    // `r` isn't an `Operator`, so record via its own dot-repeat variant.
    if !vim(ed).replaying {
        vim_mut(ed).last_change = Some(LastChange::VisualBlockReplace {
            ch,
            rows: bot - top + 1,
            cols: if to_eol {
                1
            } else {
                cols.right + 1 - cols.left
            },
            to_eol,
        });
    }
    block_replace_bounds(ed, ch, top, bot, cols);
}
/// Core of block `r`, vim's `op_replace` over `block_prep`: every display
/// cell of the block on each row becomes `ch`, so a tab inside it turns into
/// one `ch` per cell, and a tab cut by an edge keeps its outside cells as
/// spaces. A row that ends inside the block is replaced up to its end; one
/// that ends before it is left alone. Split out so dot-repeat can drive it
/// with a rectangle reconstructed at the cursor.
pub fn block_replace_bounds<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    ch: char,
    top: usize,
    bot: usize,
    cols: BlockCols,
) {
    ed.push_undo();
    ed.sync_buffer_content_from_textarea();
    use hjkl_buffer::{Edit, Position};
    let tabstop = ed.settings().tabstop;
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    for r in top..=bot.min(rope.len_lines().saturating_sub(1)) {
        let line = rope_line_to_str(&rope, r);
        let span = block_row(&line, cols, tabstop, true);
        let end_vcol = hjkl_buffer::char_col_to_visual_col(&line, span.end, tabstop);
        let short = cols.right == usize::MAX || end_vcol <= cols.right;
        let cells = if short {
            end_vcol.saturating_sub(cols.left)
        } else {
            cols.right + 1 - cols.left
        };
        if span.start == span.end || cells == 0 {
            continue;
        }
        let mut with = " ".repeat(span.startspaces);
        with.extend(std::iter::repeat_n(ch, cells));
        if !short {
            with.push_str(&" ".repeat(span.endspaces));
        }
        ed.mutate_edit(Edit::Replace {
            start: Position::new(r, span.start),
            end: Position::new(r, span.end),
            with,
        });
    }
    vim_mut(ed).mode = Mode::Normal;
    let start =
        block_start_col(ed, top, cols).min(buf_line_chars(ed.buffer(), top).saturating_sub(1));
    ed.jump_cursor(top, start);
}
/// Replicate `inserted` text across block rows `(top + 1)..=bot` at `col`.
/// `pad` distinguishes vim's two block-edge behaviours (`:h v_b_I` vs `:h
/// v_b_A`): when `pad` is true, rows shorter than `col` are padded with
/// spaces first so the text still lands at `col` (`A`); when false, rows
/// shorter than `col` are skipped entirely — no padding, no insert on that
/// row (`I`). `to_eol` (`:h v_b_$`, `A` only) overrides `col` per row with
/// that row's own current EOL — always exactly reachable, so it never pads
/// or skips. Leaves the cursor untouched — callers position it afterward.
///
/// Shared by the live `I`/`A`/`c` finish path (`finish_insert_session`) and
/// their dot-repeat replays.
pub fn replicate_block_text<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    inserted: &str,
    top: usize,
    bot: usize,
    col: usize,
    pad: bool,
    to_eol: bool,
) {
    use hjkl_buffer::{Edit, Position};
    // `col` is a character column on the top row; the other rows take the
    // same display column, which a tab before it puts at a different
    // character.
    let tabstop = ed.settings().tabstop;
    let top_line = buf_line(ed.buffer(), top).unwrap_or_default();
    let vcol = hjkl_buffer::char_col_to_visual_col(&top_line, col, tabstop);
    for r in (top + 1)..=bot {
        if !to_eol {
            let line = buf_line(ed.buffer(), r).unwrap_or_default();
            if let Some((tab, start, width)) = tab_cut_at(&line, vcol, tabstop) {
                ed.mutate_edit(Edit::Replace {
                    start: Position::new(r, tab),
                    end: Position::new(r, tab + 1),
                    with: " ".repeat(width),
                });
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(r, tab + vcol - start),
                    text: inserted.to_string(),
                });
                continue;
            }
        }
        let line_len = buf_line_chars(ed.buffer(), r);
        let line = buf_line(ed.buffer(), r).unwrap_or_default();
        let row_col = if to_eol {
            line_len
        } else {
            match char_at_vcol(&line, vcol, tabstop) {
                Ok(c) => c,
                Err(short) => {
                    if !pad {
                        // vim `v_b_I`: row doesn't reach the block's left
                        // column — skip it, no padding, no insert.
                        continue;
                    }
                    ed.mutate_edit(Edit::InsertStr {
                        at: Position::new(r, line_len),
                        text: " ".repeat(short),
                    });
                    line_len + short
                }
            }
        };
        ed.mutate_edit(Edit::InsertStr {
            at: Position::new(r, row_col),
            text: inserted.to_string(),
        });
    }
}
/// Dot-repeat replay of a block `d` / case op / `c` (`:h v_.` for blocks).
/// Reconstructs a `rows` × `cols` rectangle with its TOP-LEFT corner at the
/// current cursor (restoring the `$`-ragged `to_eol` flag) and re-runs `op`.
/// `inserted` carries the retyped `c` text (`None` for `d` / case ops).
pub fn replay_block_visual_op<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    rows: usize,
    cols: usize,
    to_eol: bool,
    inserted: Option<String>,
    levels: usize,
) {
    let (top, left) = ed.cursor();
    let bot = top + rows.saturating_sub(1);
    let right = left + cols.saturating_sub(1);
    // `d` / `c` yank the deleted rectangle into the unnamed register just
    // like the live block delete (`:h v_.` re-does the whole change,
    // register included) — verified against nvim (`.` after a block `d`
    // leaves the SECOND rectangle in `"`, not the first).
    let cols = block_cols_at(ed, top, left, right, to_eol);
    let record_delete = |ed: &mut Editor<hjkl_buffer::View, H>| {
        let yank = block_yank(ed, top, bot, cols);
        if !yank.is_empty() {
            let block_width = block_register_width(&yank, cols);
            ed.record_yank_to_host(yank.clone());
            // The replay's register comes from the change being repeated —
            // `replay_last_change` puts it back in `pending_register` before
            // calling here, so this consumes it exactly like the live path.
            let target = vim_mut(ed).pending_register.take();
            ed.record_delete_block(yank, block_width, target);
        }
    };
    match op {
        Operator::Delete => {
            ed.push_undo();
            record_delete(ed);
            delete_block_contents(ed, top, bot, cols);
            let line_len = buf_line_chars(ed.buffer(), top);
            ed.jump_cursor(top, left.min(line_len.saturating_sub(1)));
        }
        Operator::Uppercase | Operator::Lowercase | Operator::ToggleCase | Operator::Rot13 => {
            ed.push_undo();
            transform_block_case(ed, op, top, bot, cols);
            ed.jump_cursor(top, left);
        }
        Operator::Indent => {
            ed.push_undo();
            indent_block(ed, top, bot, cols, levels);
        }
        Operator::Outdent => {
            ed.push_undo();
            outdent_block(ed, top, bot, cols, levels);
        }
        Operator::Change => {
            ed.push_undo();
            record_delete(ed);
            delete_block_contents(ed, top, bot, cols);
            let text = inserted.unwrap_or_default();
            if text.is_empty() {
                ed.jump_cursor(top, left);
            } else {
                use hjkl_buffer::{Edit, Position};
                // Top row directly, remaining rows via `replicate_block_text`
                // (skips rows too short to reach `left`, `:h v_b_c`).
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(top, left),
                    text: text.clone(),
                });
                replicate_block_text(ed, &text, top, bot, left, false, false);
                let ins_chars = text.chars().count();
                let line_len = buf_line_chars(ed.buffer(), top);
                ed.jump_cursor(
                    top,
                    (left + ins_chars)
                        .saturating_sub(1)
                        .min(line_len.saturating_sub(1)),
                );
            }
        }
        _ => {}
    }
    vim_mut(ed).mode = Mode::Normal;
}
/// Dot-repeat replay of block `r{ch}` — reconstruct the rectangle at the
/// cursor and re-replace.
pub fn replay_block_replace<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    ch: char,
    rows: usize,
    cols: usize,
    to_eol: bool,
) {
    let (top, left) = ed.cursor();
    let bot = top + rows.saturating_sub(1);
    let right = left + cols.saturating_sub(1);
    let cols = block_cols_at(ed, top, left, right, to_eol);
    block_replace_bounds(ed, ch, top, bot, cols);
}
/// Dot-repeat replay of block `I` / `A` — re-insert `text` at the block's
/// left (`append == false`) or right (`append == true`) edge over a
/// rectangle reconstructed at the cursor. Ragged `A` (`to_eol`) appends at
/// each row's own EOL. Cursor lands on the block's LEFT edge, matching the
/// live finish path.
pub fn replay_block_insert<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    text: &str,
    rows: usize,
    cols: usize,
    to_eol: bool,
    append: bool,
) {
    use hjkl_buffer::{Edit, Position};
    let (top, left) = ed.cursor();
    let bot = top + rows.saturating_sub(1);
    ed.push_undo();
    ed.sync_buffer_content_from_textarea();
    // `A` appends `cols` columns past the left edge (or each row's own EOL
    // when ragged); `I` inserts at the left edge.
    let top_col = if append {
        if to_eol {
            buf_line_chars(ed.buffer(), top)
        } else {
            left + cols
        }
    } else {
        left
    };
    let line_len = buf_line_chars(ed.buffer(), top);
    if append && top_col > line_len {
        let pad_str: String = std::iter::repeat_n(' ', top_col - line_len).collect();
        ed.mutate_edit(Edit::InsertStr {
            at: Position::new(top, line_len),
            text: pad_str,
        });
    }
    // Top row (cursor row) always reaches `top_col` — `left` for `I`, or a
    // just-padded column for `A`.
    ed.mutate_edit(Edit::InsertStr {
        at: Position::new(top, top_col),
        text: text.to_string(),
    });
    replicate_block_text(ed, text, top, bot, top_col, append, to_eol);
    ed.jump_cursor(top, left);
    vim_mut(ed).mode = Mode::Normal;
}
/// B2: `r{ch}` in charwise (`v`) or linewise (`V`) Visual mode — replace
/// every character in the selection with `ch`; newlines are NOT replaced
/// (a linewise selection keeps its original line count). Registers are
/// left untouched (`r` doesn't yank/delete). Cursor lands at the start of
/// the selection — verified against real nvim: charwise `llvjrX` on
/// `"aaaa\nbbbb\n..."` lands on `(0, 2)` (the selection's top), linewise
/// `VjrZ` lands on `(top, 0)`.
pub fn visual_replace_char<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    ch: char,
) {
    let linewise = matches!(vim(ed).mode, Mode::VisualLine);
    let (top, bot) = if linewise {
        let cursor_row = buf_cursor_pos(ed.buffer()).row;
        let anchor_row = vim(ed).visual_line_anchor;
        (
            (cursor_row.min(anchor_row), 0),
            (cursor_row.max(anchor_row), 0),
        )
    } else {
        let anchor = vim(ed).visual_anchor;
        let cursor = ed.cursor();
        order(anchor, cursor)
    };
    // `r` isn't an `Operator`, so record via its own dot-repeat variant
    // (mirrors block `r` / `VisualBlockReplace`). Charwise stores the same
    // `charwise_extent` the visual OPERATORS use; linewise stores the row
    // count — `.` then re-replaces a same-size region at the cursor.
    if !vim(ed).replaying {
        let extent = if linewise {
            VisualExtent::Line {
                lines: bot.0 - top.0 + 1,
            }
        } else {
            charwise_extent(top, bot)
        };
        vim_mut(ed).last_change = Some(LastChange::VisualReplace { ch, extent });
    }
    // The undo step saves the selection's start, as for the operators: in
    // Visual-line that is column 0 unless the cursor is above the anchor.
    let start = if !linewise {
        top
    } else if ed.cursor().0 < vim(ed).visual_line_anchor {
        ed.cursor()
    } else {
        top
    };
    buf_set_cursor_rc(ed.buffer_mut(), start.0, start.1);
    replace_range_with_char(ed, linewise, top, bot, ch);
}
/// Core of charwise/linewise Visual `r`: replace every character in
/// `(top..=bot)` with `ch`, preserving newlines. Split out so dot-repeat can
/// drive it with a region reconstructed at the cursor (`replay_visual_replace`)
/// rather than the live selection. Cursor lands at the selection start
/// (`top.1` charwise, column 0 linewise).
fn replace_range_with_char<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    linewise: bool,
    top: (usize, usize),
    bot: (usize, usize),
    ch: char,
) {
    ed.push_undo();
    ed.sync_buffer_content_from_textarea();
    use hjkl_buffer::{Edit, Position};
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let last_row = bot.0.min(rope.len_lines().saturating_sub(1));
    for row in top.0..=last_row {
        let chars: Vec<char> = rope_line_to_str(&rope, row).chars().collect();
        let lo = if !linewise && row == top.0 { top.1 } else { 0 };
        let hi = if !linewise && row == bot.0 {
            (bot.1 + 1).min(chars.len())
        } else {
            chars.len()
        };
        if lo >= hi {
            continue;
        }
        // Replace only the selection's span — the row's head/tail stay put.
        let middle: String = std::iter::repeat_n(ch, hi - lo).collect();
        ed.mutate_edit(Edit::Replace {
            start: Position::new(row, lo),
            end: Position::new(row, hi),
            with: middle,
        });
    }
    vim_mut(ed).mode = Mode::Normal;
    ed.jump_cursor(top.0, if linewise { 0 } else { top.1 });
}
/// Dot-repeat replay of charwise/linewise Visual `r{ch}` — reconstruct a
/// same-size region anchored at the cursor from the stored `extent` (mirrors
/// the `VisualExtent::Char`/`Line` shape the visual operators replay, `:h
/// v_.`), then re-replace. `VisualExtent::Block` never reaches here (block `r`
/// rides `VisualBlockReplace`).
pub fn replay_visual_replace<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    ch: char,
    extent: VisualExtent,
) {
    let (r, c) = ed.cursor();
    match extent {
        VisualExtent::Line { lines } => {
            let top = (r, 0);
            let bot = (r + lines.saturating_sub(1), 0);
            replace_range_with_char(ed, true, top, bot, ch);
        }
        VisualExtent::Char { lines, width } => {
            let top = (r, c);
            // Same reconstruction the charwise operator replay uses: single
            // line keeps its raw `width` from the cursor; multi-line runs the
            // first line cursor-to-EOL, middle lines whole, last line's first
            // `width` chars (`bot.1` measured from column 0).
            let bot = if lines <= 1 {
                (r, c + width.saturating_sub(1))
            } else {
                (r + lines - 1, width.saturating_sub(1))
            };
            replace_range_with_char(ed, false, top, bot, ch);
        }
        VisualExtent::Block { .. } => {}
    }
}
