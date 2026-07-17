//! Phase 6.6e: normal-mode FSM body relocated from `hjkl-engine::vim`.
//!
//! Dispatched by [`crate::dispatch_input`] for all non-insert,
//! non-search-prompt modes (Normal, Visual, VisualLine, VisualBlock).
//!
//! The engine keeps in-engine duplicate bodies (`step_normal` +
//! `handle_normal_only`) in `vim::step` for back-compat with the deprecated
//! `Editor::step_input` / `Editor::step_input_raw` shim path until Phase 6.6h.
use crate::vim::{op_is_change, parse_motion};
use hjkl_engine::{
    FsmMode, Host, Input, Key, LastChange, Motion, Operator, Pending, ScrollDir, VimMode,
};

// Re-export sneak variants for shorter usage in this module.
use hjkl_engine::Pending::{OpSneakFirst, OpSneakSecond, SneakFirst, SneakSecond};

use crate::VimEditorExt;

// ─── Public entry point ────────────────────────────────────────────────────

/// Drive the normal / visual / operator-pending FSM for one keystroke.
///
/// Returns `true` when the input was consumed. Every key is consumed in
/// these modes (unknown keys swallow silently to avoid TUI bubbling).
pub fn step_normal<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
) -> bool {
    // Consume digits first — except '0' at start of count (that's LineStart).
    if let Key::Char(d @ '0'..='9') = input.key
        && !input.ctrl
        && !input.alt
        && !matches!(
            ed.pending(),
            Pending::Replace
                | Pending::Find { .. }
                | Pending::OpFind { .. }
                | Pending::VisualTextObj { .. }
                // Pendings whose next key is a literal NAME, not a count, so a
                // digit selects e.g. `"1` (numbered register), `` `1 `` /
                // `'1` (numbered mark), `q1` (macro register) — not a count.
                | Pending::SelectRegister
                | Pending::SetMark
                | Pending::GotoMarkLine
                | Pending::GotoMarkChar
                | Pending::RecordMacroTarget
                | Pending::PlayMacroTarget { .. }
                | SneakFirst { .. }
                | SneakSecond { .. }
                | OpSneakFirst { .. }
                | OpSneakSecond { .. }
        )
        && (d != '0' || ed.count() > 0)
    {
        ed.accumulate_count_digit(d as usize - '0' as usize);
        return true;
    }

    // Handle pending two-key sequences first.
    match ed.take_pending() {
        Pending::Replace => return handle_replace(ed, input),
        Pending::Find { forward, till } => return handle_find_target(ed, input, forward, till),
        Pending::OpFind {
            op,
            count1,
            forward,
            till,
        } => return handle_op_find_target(ed, input, op, count1, forward, till),
        Pending::G => return handle_after_g(ed, input),
        Pending::OpG { op, count1 } => return handle_op_after_g(ed, input, op, count1),
        Pending::Op { op, count1 } => return handle_after_op(ed, input, op, count1),
        Pending::OpTextObj { op, count1, inner } => {
            return handle_text_object(ed, input, op, count1, inner);
        }
        Pending::VisualTextObj { inner } => {
            return handle_visual_text_obj(ed, input, inner);
        }
        Pending::Z => return handle_after_z(ed, input),
        Pending::SetMark => return handle_set_mark(ed, input),
        Pending::GotoMarkLine => return handle_goto_mark(ed, input, true),
        Pending::GotoMarkChar => return handle_goto_mark(ed, input, false),
        Pending::SelectRegister => return handle_select_register(ed, input),
        Pending::RecordMacroTarget => return handle_record_macro_target(ed, input),
        Pending::PlayMacroTarget { count } => return handle_play_macro_target(ed, input, count),
        Pending::SquareBracketOpen => {
            let cnt = ed.take_count();
            return handle_after_square_bracket_open(ed, input, cnt);
        }
        Pending::SquareBracketClose => {
            let cnt = ed.take_count();
            return handle_after_square_bracket_close(ed, input, cnt);
        }
        Pending::OpSquareBracketOpen { op, count1 } => {
            return handle_op_after_square_bracket_open(ed, input, op, count1);
        }
        Pending::OpSquareBracketClose { op, count1 } => {
            return handle_op_after_square_bracket_close(ed, input, op, count1);
        }
        SneakFirst { forward, count } => {
            return handle_sneak_first(ed, input, forward, count);
        }
        SneakSecond { c1, forward, count } => {
            return handle_sneak_second(ed, input, c1, forward, count);
        }
        OpSneakFirst {
            op,
            count1,
            forward,
        } => {
            return handle_op_sneak_first(ed, input, op, count1, forward);
        }
        OpSneakSecond {
            op,
            count1,
            c1,
            forward,
        } => {
            return handle_op_sneak_second(ed, input, op, count1, c1, forward);
        }
        Pending::None => {}
    }

    // Whether the user typed an explicit count before this key (`take_count`
    // defaults to 1, erasing the distinction — capture it first).
    let had_explicit_count = ed.count() > 0;
    let count = ed.take_count();

    // Common normal / visual keys.
    match input.key {
        Key::Esc => {
            // BLAME is a Normal-only read-only view; Esc leaves it (returning
            // to a plain Normal view) as well as clearing any pending state.
            ed.exit_blame();
            ed.force_normal();
            return true;
        }
        Key::Char('v') if !input.ctrl && ed.fsm_mode() == FsmMode::Normal => {
            ed.set_visual_anchor(ed.cursor());
            ed.set_mode(VimMode::Visual);
            // B5: `[count]v` extends the initial selection to `count`
            // chars from the anchor — verified against real nvim: the
            // selection stays within the CURRENT LINE, clamped at
            // one-past-the-last-char (absorbing the trailing newline into
            // an operator's range, same as `v$`) once `count` reaches or
            // exceeds the remaining line length; it never wraps onto the
            // next line's real characters no matter how large `count` is
            // (`3v` through `8v` on a 2-char line all select identically).
            if had_explicit_count && count > 1 {
                let (row, col) = ed.cursor();
                let line_chars = hjkl_engine::buf_helpers::buf_line_chars(ed.buffer(), row);
                let target_col = (col + count - 1).min(line_chars);
                ed.jump_cursor(row, target_col);
            }
            return true;
        }
        Key::Char('V') if !input.ctrl && ed.fsm_mode() == FsmMode::Normal => {
            let (row, _) = ed.cursor();
            ed.set_visual_line_anchor(row);
            ed.set_mode(VimMode::VisualLine);
            // B5: `[count]V` extends the initial selection to `count`
            // lines from the anchor, clamped to the buffer's LAST CONTENT
            // row — `buf_row_count` counts ropey's phantom trailing row
            // (the empty remainder after a buffer's own trailing `\n`), so
            // clamping to `total - 1` directly would land the cursor one
            // row past the real content (`:h phantom row`-style bug, same
            // class as H2's linewise-delete clamp).
            if had_explicit_count && count > 1 {
                let total = hjkl_engine::buf_helpers::buf_row_count(ed.buffer());
                let last_content_row = if total >= 2
                    && hjkl_engine::buf_helpers::buf_line(ed.buffer(), total - 1)
                        .map(|s| s.is_empty())
                        .unwrap_or(false)
                {
                    total - 2
                } else {
                    total.saturating_sub(1)
                };
                let target_row = (row + count - 1).min(last_content_row);
                ed.jump_cursor(target_row, 0);
            }
            return true;
        }
        Key::Char('v') if !input.ctrl && ed.fsm_mode() == FsmMode::VisualLine => {
            ed.set_visual_anchor(ed.cursor());
            ed.set_mode(VimMode::Visual);
            return true;
        }
        Key::Char('V') if !input.ctrl && ed.fsm_mode() == FsmMode::Visual => {
            let (row, _) = ed.cursor();
            ed.set_visual_line_anchor(row);
            ed.set_mode(VimMode::VisualLine);
            return true;
        }
        Key::Char('v') if input.ctrl && ed.fsm_mode() == FsmMode::Normal => {
            let cur = ed.cursor();
            ed.set_block_anchor(cur);
            ed.set_block_vcol(cur.1);
            ed.set_block_to_eol(false);
            ed.set_mode(VimMode::VisualBlock);
            return true;
        }
        Key::Char('v') if input.ctrl && ed.fsm_mode() == FsmMode::VisualBlock => {
            // Second Ctrl-v exits block mode back to Normal.
            ed.set_mode(VimMode::Normal);
            return true;
        }
        // `o` in visual modes — swap anchor and cursor so the user
        // can extend the other end of the selection.
        Key::Char('o') if !input.ctrl => match ed.fsm_mode() {
            FsmMode::Visual => {
                let cur = ed.cursor();
                let anchor = ed.visual_anchor();
                ed.set_visual_anchor(cur);
                ed.jump_cursor(anchor.0, anchor.1);
                return true;
            }
            FsmMode::VisualLine => {
                let cur_row = ed.cursor().0;
                let anchor_row = ed.visual_line_anchor();
                ed.set_visual_line_anchor(cur_row);
                ed.jump_cursor(anchor_row, 0);
                return true;
            }
            FsmMode::VisualBlock => {
                let cur = ed.cursor();
                let anchor = ed.block_anchor();
                ed.set_block_anchor(cur);
                ed.set_block_vcol(anchor.1);
                ed.jump_cursor(anchor.0, anchor.1);
                return true;
            }
            _ => {}
        },
        _ => {}
    }

    // Visual mode: `p` / `P` replace the selection with the register.
    if ed.is_visual() && !input.ctrl && matches!(input.key, Key::Char('p') | Key::Char('P')) {
        ed.visual_paste(matches!(input.key, Key::Char('P')));
        return true;
    }

    // Visual mode: `J` joins the selected lines (with a space).
    if ed.is_visual() && !input.ctrl && input.key == Key::Char('J') {
        ed.visual_join(true);
        return true;
    }

    // Visual mode: operators act on the current selection. The leading count
    // (drained into `count` above) multiplies indent levels (`2>` = two
    // shiftwidths); other visual operators ignore it.
    if ed.is_visual()
        && let Some(op) = visual_operator(&input)
    {
        ed.apply_visual_operator(op, count.max(1));
        return true;
    }

    // B2: charwise (`v`) / linewise (`V`) Visual mode `r<ch>` — replace
    // every character in the selection with `ch`. `visual_operator()`
    // deliberately has no `r` arm (it isn't an operator: it takes its own
    // pending char, like normal-mode `r`), so without this arm bare `r`
    // fell through every branch to the unknown-key no-op — leaving Visual
    // mode active — and the NEXT key got redispatched as a fresh visual
    // command (e.g. `vllrx` silently swallowed `r`, then `x` deleted the
    // still-active selection: real data loss, not a replace).
    if matches!(ed.fsm_mode(), FsmMode::Visual | FsmMode::VisualLine)
        && !input.ctrl
        && input.key == Key::Char('r')
    {
        ed.set_pending(Pending::Replace);
        return true;
    }

    // VisualBlock: extra commands beyond the standard y/d/c/x — `r`
    // replaces the block with a single char, `I` / `A` enter insert
    // mode at the block's left / right edge and repeat on every row.
    if ed.fsm_mode() == FsmMode::VisualBlock && !input.ctrl {
        match input.key {
            Key::Char('r') => {
                ed.set_pending(Pending::Replace);
                return true;
            }
            Key::Char('I') => {
                let (top, bot, left, _right) = ed.visual_block_bounds();
                ed.visual_block_insert_at_left(top, bot, left);
                return true;
            }
            Key::Char('A') => {
                // vim `v_b_A`: append one past the block's right column on
                // EVERY row, padding short rows to reach it first (`:h
                // v_b_A`). The old `.min(line_char_count(top))` clamp here
                // capped the column by the TOP row's length alone, so on
                // longer rows the typed text landed inside the block
                // instead of past its right edge — `visual_block_append_
                // at_right` now does the per-row padding itself.
                //
                // Ragged (`$` was pressed — `:h v_b_$`): append at EACH
                // row's own EOL instead, so the top row's insertion column
                // is that row's own current length rather than a fixed
                // `right + 1`.
                let (top, bot, left, right) = ed.visual_block_bounds();
                let col = if ed.block_to_eol() {
                    ed.line_char_count(top)
                } else {
                    right + 1
                };
                ed.visual_block_append_at_right(top, bot, col, left);
                return true;
            }
            _ => {}
        }
    }

    // Visual mode: `i` / `a` start a text-object extension.
    if matches!(ed.fsm_mode(), FsmMode::Visual | FsmMode::VisualLine)
        && !input.ctrl
        && matches!(input.key, Key::Char('i') | Key::Char('a'))
    {
        let inner = matches!(input.key, Key::Char('i'));
        ed.set_pending(Pending::VisualTextObj { inner });
        return true;
    }

    // Ctrl-prefixed scrolling + misc. Vim semantics: Ctrl-d / Ctrl-u
    // move the cursor by half a window, Ctrl-f / Ctrl-b by a full
    // window. Viewport follows the cursor. Cursor lands on the first
    // non-blank of the target row (matches vim).
    if input.ctrl
        && let Key::Char(c) = input.key
    {
        match c {
            'd' => {
                ed.scroll_half_page(ScrollDir::Down, count);
                return true;
            }
            'u' => {
                ed.scroll_half_page(ScrollDir::Up, count);
                return true;
            }
            'f' => {
                ed.scroll_full_page(ScrollDir::Down, count);
                return true;
            }
            'b' => {
                ed.scroll_full_page(ScrollDir::Up, count);
                return true;
            }
            'e' if ed.fsm_mode() == FsmMode::Normal => {
                ed.scroll_line(ScrollDir::Down, count);
                return true;
            }
            'y' if ed.fsm_mode() == FsmMode::Normal => {
                ed.scroll_line(ScrollDir::Up, count);
                return true;
            }
            'r' => {
                ed.later_by_steps(count.max(1));
                return true;
            }
            'a' if ed.fsm_mode() == FsmMode::Normal => {
                ed.adjust_number(count.max(1) as i64);
                return true;
            }
            // Visual `<C-a>` — add the same amount to each selected line's
            // first number (uniform). `g<C-a>` (sequential) takes the g path.
            'a' if ed.is_visual() => {
                ed.adjust_number_visual(count.max(1) as i64, false);
                return true;
            }
            'x' if ed.is_visual() => {
                ed.adjust_number_visual(-(count.max(1) as i64), false);
                return true;
            }
            'x' if ed.fsm_mode() == FsmMode::Normal => {
                ed.adjust_number(-(count.max(1) as i64));
                return true;
            }
            'o' if ed.fsm_mode() == FsmMode::Normal => {
                ed.jump_back(count);
                return true;
            }
            'i' if ed.fsm_mode() == FsmMode::Normal => {
                ed.jump_forward(count);
                return true;
            }
            _ => {}
        }
    }

    // `Tab` in normal mode is also `Ctrl-i` — vim aliases them.
    if !input.ctrl && input.key == Key::Tab && ed.fsm_mode() == FsmMode::Normal {
        ed.jump_forward(count);
        return true;
    }

    // `[count]%` — go to the line at `count` percent of the file. With no
    // count, `%` is the match-pair motion (handled by `parse_motion` below).
    if !input.ctrl && input.key == Key::Char('%') && had_explicit_count {
        ed.goto_percent(count);
        return true;
    }

    // Motion-only commands.
    if let Some(motion) = parse_motion(&input) {
        ed.execute_motion(motion.clone(), count);
        // Block mode: maintain the virtual column across j/k clamps.
        if ed.fsm_mode() == FsmMode::VisualBlock {
            ed.update_block_vcol(&motion);
        }
        if let Motion::Find { ch, forward, till } = motion {
            ed.set_last_find(Some((ch, forward, till)));
        }
        return true;
    }

    // `.` dot-repeat: vim *replaces* the stored count with an explicit
    // `[count].` (`:h .`) — `3x` then `2.` deletes 2, not 6. Pass 0 when the
    // user typed no count so the engine reuses the change's original count.
    // Handled here (not in `handle_normal_only`) because that helper only
    // sees the count-defaulted-to-1 value and loses `had_explicit_count`.
    if ed.fsm_mode() == FsmMode::Normal && !input.ctrl && input.key == Key::Char('.') {
        ed.replay_last_change(if had_explicit_count { count } else { 0 });
        return true;
    }

    // Mode transitions + pure normal-mode commands (not applicable in visual).
    if ed.fsm_mode() == FsmMode::Normal && handle_normal_only(ed, &input, count) {
        return true;
    }

    // Operator triggers in normal mode.
    if ed.fsm_mode() == FsmMode::Normal
        && let Key::Char(op_ch) = input.key
        && !input.ctrl
        && let Some(op) = char_to_operator(op_ch)
    {
        ed.set_pending(Pending::Op { op, count1: count });
        return true;
    }

    // `f`/`F`/`t`/`T` entry.
    if ed.fsm_mode() == FsmMode::Normal
        && let Some((forward, till)) = find_entry(&input)
    {
        ed.set_count(count);
        ed.set_pending(Pending::Find { forward, till });
        return true;
    }

    // `g` prefix. Available in Normal and the Visual modes (visual `gu`/`gU`/
    // `g~`, `gq`/`gw`, `g<C-a>`/`g<C-x>`, and the `gg`/`ge` extend motions).
    if !input.ctrl
        && input.key == Key::Char('g')
        && matches!(
            ed.fsm_mode(),
            FsmMode::Normal | FsmMode::Visual | FsmMode::VisualLine | FsmMode::VisualBlock
        )
    {
        ed.set_count(count);
        ed.set_pending(Pending::G);
        return true;
    }

    // `z` prefix (zz / zt / zb / zh / zl / zH / zL — cursor-relative
    // viewport scrolls). B13: re-arm the count the digit-accumulation
    // above already consumed via `take_count()` (line ~126) so
    // `handle_after_z`'s own `take_count()` sees `[count]` instead of the
    // default 1 — mirrors the sibling `g` prefix handler just above.
    if !input.ctrl
        && input.key == Key::Char('z')
        && matches!(
            ed.fsm_mode(),
            FsmMode::Normal | FsmMode::Visual | FsmMode::VisualLine | FsmMode::VisualBlock
        )
    {
        ed.set_count(count);
        ed.set_pending(Pending::Z);
        return true;
    }

    // `[` prefix (section motions `[[` / `[]`). Available in Normal and Visual modes.
    if !input.ctrl
        && input.key == Key::Char('[')
        && matches!(
            ed.fsm_mode(),
            FsmMode::Normal | FsmMode::Visual | FsmMode::VisualLine | FsmMode::VisualBlock
        )
    {
        ed.set_count(count);
        ed.set_pending(Pending::SquareBracketOpen);
        return true;
    }

    // `]` prefix (section motions `]]` / `][`). Available in Normal and Visual modes.
    if !input.ctrl
        && input.key == Key::Char(']')
        && matches!(
            ed.fsm_mode(),
            FsmMode::Normal | FsmMode::Visual | FsmMode::VisualLine | FsmMode::VisualBlock
        )
    {
        ed.set_count(count);
        ed.set_pending(Pending::SquareBracketClose);
        return true;
    }

    // Mark set / jump entries. `m` arms the set-mark pending state;
    // `'` and `` ` `` arm the goto states (linewise vs charwise). The
    // mark letter is consumed on the next keystroke.
    // In visual modes, `` ` `` also arms GotoMarkChar so the cursor can
    // extend the selection to a mark position (e.g. `` `[v`] `` idiom).
    if !input.ctrl
        && matches!(
            ed.fsm_mode(),
            FsmMode::Normal | FsmMode::Visual | FsmMode::VisualLine | FsmMode::VisualBlock
        )
        && input.key == Key::Char('`')
    {
        ed.set_pending(Pending::GotoMarkChar);
        return true;
    }
    if !input.ctrl && ed.fsm_mode() == FsmMode::Normal {
        match input.key {
            Key::Char('m') => {
                ed.set_pending(Pending::SetMark);
                return true;
            }
            Key::Char('\'') => {
                ed.set_pending(Pending::GotoMarkLine);
                return true;
            }
            Key::Char('`') => {
                // Already handled above for all visual modes + normal.
                ed.set_pending(Pending::GotoMarkChar);
                return true;
            }
            Key::Char('"') => {
                // Open the register-selector chord. The next char picks
                // a register that the next y/d/c/p uses.
                ed.set_pending(Pending::SelectRegister);
                return true;
            }
            Key::Char('@') => {
                // Open the macro-play chord. Next char names the
                // register; `@@` re-plays the last-played macro.
                // Stash any count so the chord can multiply replays.
                ed.set_pending(Pending::PlayMacroTarget { count });
                return true;
            }
            Key::Char('q') if ed.recording_macro().is_none() => {
                // Open the macro-record chord. The bare-q stop is
                // handled at the top of `step` so it's not consumed
                // as another open. Recording-in-progress falls through
                // here and is treated as a no-op (matches vim).
                ed.set_pending(Pending::RecordMacroTarget);
                return true;
            }
            _ => {}
        }
    }

    // Unknown key — swallow so it doesn't bubble into the TUI layer.
    true
}

// ─── Phase 6.6a thin dispatcher ───────────────────────────────────────────

/// Normal-only commands (not motion, not operator, not applicable in visual).
fn handle_normal_only<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: &Input,
    count: usize,
) -> bool {
    if input.ctrl {
        return false;
    }
    match input.key {
        Key::Char('i') => {
            ed.enter_insert_i(count);
            true
        }
        Key::Char('I') => {
            ed.enter_insert_shift_i(count);
            true
        }
        Key::Char('a') => {
            ed.enter_insert_a(count);
            true
        }
        Key::Char('A') => {
            ed.enter_insert_shift_a(count);
            true
        }
        Key::Char('R') => {
            ed.enter_replace_mode(count);
            true
        }
        Key::Char('o') => {
            ed.open_line_below(count);
            true
        }
        Key::Char('O') => {
            ed.open_line_above(count);
            true
        }
        Key::Char('x') => {
            ed.delete_char_forward(count);
            true
        }
        Key::Char('X') => {
            ed.delete_char_backward(count);
            true
        }
        Key::Char('~') => {
            ed.toggle_case_at_cursor(count);
            true
        }
        Key::Char('J') => {
            ed.join_line(count);
            true
        }
        Key::Char('D') => {
            ed.delete_to_eol();
            true
        }
        Key::Char('Y') => {
            ed.yank_to_eol(count);
            true
        }
        Key::Char('C') => {
            ed.change_to_eol();
            true
        }
        Key::Char('s') => {
            if ed.settings().motion_sneak {
                // vim-sneak: `s` enters SneakFirst (forward). The count is
                // threaded through the pending payload only — stashing it in
                // the editor accumulator too would leak it into the command
                // that follows the sneak (nothing on this path takes it back).
                ed.set_pending(SneakFirst {
                    forward: true,
                    count,
                });
            } else {
                ed.substitute_char(count);
            }
            true
        }
        Key::Char('S') => {
            if ed.settings().motion_sneak {
                // vim-sneak: `S` enters SneakFirst (backward). Count threads
                // through the pending payload only (see `s` above).
                ed.set_pending(SneakFirst {
                    forward: false,
                    count,
                });
            } else {
                ed.substitute_line(count);
            }
            true
        }
        Key::Char('p') => {
            ed.paste_after(count);
            true
        }
        Key::Char('P') => {
            ed.paste_before(count);
            true
        }
        Key::Char('&') => {
            // `&` — repeat last `:s` on the current line (no flags).
            ed.ampersand_repeat();
            true
        }
        Key::Char('u') => {
            ed.earlier_by_steps(count.max(1));
            true
        }
        Key::Char('U') => {
            // `U` — restore the last-changed line (`:h U`). Vim ignores
            // any count. `undo_line` handles the "nothing to restore"
            // no-op and the undo/toggle semantics itself.
            ed.undo_line();
            true
        }
        Key::Char('r') => {
            ed.set_count(count);
            ed.set_pending(Pending::Replace);
            true
        }
        Key::Char('/') => {
            ed.enter_search(true);
            true
        }
        Key::Char('?') => {
            ed.enter_search(false);
            true
        }
        _ => false,
    }
}

// ─── Pending chord handlers ────────────────────────────────────────────────

fn handle_set_mark<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
) -> bool {
    if let Key::Char(c) = input.key {
        ed.set_mark_at_cursor(c);
    }
    true
}

fn handle_select_register<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
) -> bool {
    if let Key::Char(c) = input.key {
        ed.set_pending_register(c);
    }
    true
}

fn handle_record_macro_target<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
) -> bool {
    if let Key::Char(c) = input.key
        && (c.is_ascii_alphabetic() || c.is_ascii_digit())
    {
        ed.set_recording_macro(Some(c));
        // For `qA` (capital), seed the buffer with the existing
        // lowercase recording so the new keystrokes append.
        if c.is_ascii_uppercase() {
            let lower = c.to_ascii_lowercase();
            // Seed `recording_keys` with the existing register's text
            // decoded back to inputs, so capital-register append
            // continues from where the previous recording left off.
            let text = ed
                .with_registers(|r| r.read(lower).map(|s| s.text.clone()))
                .unwrap_or_default();
            ed.set_recording_keys(hjkl_engine::decode_macro(&text));
        } else {
            ed.set_recording_keys(vec![]);
        }
    }
    true
}

fn handle_play_macro_target<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    count: usize,
) -> bool {
    let reg = match input.key {
        Key::Char('@') => ed.last_macro(),
        Key::Char(c) if c.is_ascii_alphabetic() || c.is_ascii_digit() => {
            Some(c.to_ascii_lowercase())
        }
        _ => None,
    };
    let Some(reg) = reg else {
        return true;
    };
    // Read the macro text from the named register and decode back to
    // an Input stream. Empty / unset registers replay nothing.
    let text = match ed.with_registers(|r| r.read(reg).cloned()) {
        Some(slot) if !slot.text.is_empty() => slot.text.clone(),
        _ => return true,
    };
    let keys = hjkl_engine::decode_macro(&text);
    ed.set_last_macro(Some(reg));
    // Replay-recursion guard: a register whose text plays itself (e.g.
    // register `a` holding "@a") would otherwise recurse through
    // `dispatch_input` until the stack overflows. Vim bounds nested
    // replay via 'maxmapdepth'; we cap the same way and silently stop
    // at the limit.
    const MAX_REPLAY_DEPTH: usize = 100;
    thread_local! {
        static REPLAY_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    let depth = REPLAY_DEPTH.with(std::cell::Cell::get);
    if depth >= MAX_REPLAY_DEPTH {
        return true;
    }
    REPLAY_DEPTH.with(|d| d.set(depth + 1));
    let times = count.max(1);
    let was_replaying = ed.is_replaying_macro_raw();
    ed.set_replaying_macro_raw(true);
    for _ in 0..times {
        for k in keys.iter().copied() {
            crate::dispatch_input(ed, k);
        }
    }
    ed.set_replaying_macro_raw(was_replaying);
    REPLAY_DEPTH.with(|d| d.set(depth));
    true
}

fn handle_goto_mark<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    linewise: bool,
) -> bool {
    let Key::Char(c) = input.key else {
        return true;
    };
    // CrossBuffer results are silently ignored here — the FSM has no
    // mechanism to switch buffers. The app layer handles uppercase marks
    // through chord_routing + apply_mark_jump. Lowercase/special marks
    // always resolve in the same buffer. Uppercase marks that are in the
    // same buffer (current_buffer_id matches) execute the jump normally.
    if linewise {
        let _ = ed.try_goto_mark_line(c);
    } else {
        let _ = ed.try_goto_mark_char(c);
    }
    true
}

fn handle_after_op<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    op: Operator,
    count1: usize,
) -> bool {
    // Inner count after operator (e.g. d3w): accumulate in state.count.
    if let Key::Char(d @ '0'..='9') = input.key
        && !input.ctrl
        && (d != '0' || ed.count() > 0)
    {
        ed.accumulate_count_digit(d as usize - '0' as usize);
        ed.set_pending(Pending::Op { op, count1 });
        return true;
    }

    // Esc cancels.
    if input.key == Key::Esc {
        ed.reset_count();
        return true;
    }

    // Same-letter: dd / cc / yy / gUU / guu / g~~ / >> / <<. Fold has
    // no doubled form in vim — `zfzf` is two `zf` chords, not a line
    // op — so skip the branch entirely.
    let double_ch = match op {
        Operator::Delete => Some('d'),
        Operator::Change => Some('c'),
        Operator::Yank => Some('y'),
        Operator::Indent => Some('>'),
        Operator::Outdent => Some('<'),
        Operator::Uppercase => Some('U'),
        Operator::Lowercase => Some('u'),
        Operator::ToggleCase => Some('~'),
        Operator::Fold => None,
        // `gqq` reflows the current line — vim's doubled form for the
        // reflow operator is the second `q` after `gq`.
        Operator::Reflow => Some('q'),
        // `gww` reflows the current line keeping the cursor — second `w` after `gw`.
        Operator::ReflowKeepCursor => Some('w'),
        // `==` auto-indents the current line.
        Operator::AutoIndent => Some('='),
        // `!!` filters the current line — vim's doubled form.
        Operator::Filter => Some('!'),
        // `gcc` toggles comment on the current line — doubled 'c' after `gc`.
        Operator::Comment => Some('c'),
        // `g??` rot13s the current line — doubled '?' after `g?`.
        Operator::Rot13 => Some('?'),
    };
    if let Key::Char(c) = input.key
        && !input.ctrl
        && Some(c) == double_ch
    {
        let count2 = ed.take_count();
        let total = count1.max(1).saturating_mul(count2.max(1));
        ed.apply_op_double(op, total);
        return true;
    }

    // Text object: `i` or `a`.
    if let Key::Char('i') | Key::Char('a') = input.key
        && !input.ctrl
    {
        let inner = matches!(input.key, Key::Char('i'));
        ed.set_pending(Pending::OpTextObj { op, count1, inner });
        return true;
    }

    // `g` — awaiting `g` for `gg`.
    if input.key == Key::Char('g') && !input.ctrl {
        ed.set_pending(Pending::OpG { op, count1 });
        return true;
    }

    // `[` / `]` — section-motion prefix in operator-pending context (d[[ etc).
    if !input.ctrl && input.key == Key::Char('[') {
        ed.set_pending(Pending::OpSquareBracketOpen { op, count1 });
        return true;
    }
    if !input.ctrl && input.key == Key::Char(']') {
        ed.set_pending(Pending::OpSquareBracketClose { op, count1 });
        return true;
    }

    // `f`/`F`/`t`/`T` with pending target.
    if let Some((forward, till)) = find_entry(&input) {
        ed.set_pending(Pending::OpFind {
            op,
            count1,
            forward,
            till,
        });
        return true;
    }

    // `s`/`S` sneak with operator pending (e.g. `dsab`).
    if ed.settings().motion_sneak
        && let Key::Char(sc) = input.key
        && !input.ctrl
        && matches!(sc, 's' | 'S')
    {
        let forward = sc == 's';
        ed.set_pending(OpSneakFirst {
            op,
            count1,
            forward,
        });
        return true;
    }

    // `/` / `?` — operator + search motion (`d/pat`, `c/pat`, `y/pat`). Opens
    // the search prompt in operator-pending mode; the operator runs over the
    // range to the match on commit.
    if !input.ctrl && matches!(input.key, Key::Char('/') | Key::Char('?')) {
        let forward = input.key == Key::Char('/');
        ed.enter_search_op(forward, op, count1);
        return true;
    }

    // Motion.
    let count2 = ed.take_count();
    let total = count1.max(1).saturating_mul(count2.max(1));
    if let Some(motion) = parse_motion(&input) {
        let motion = match motion {
            Motion::FindRepeat { reverse } => match ed.last_find() {
                Some((ch, forward, till)) => Motion::Find {
                    ch,
                    forward: if reverse { !forward } else { forward },
                    till,
                },
                None => return true,
            },
            // Vim quirk (`:h cw`): `cw`/`cW` act like `ce`/`cE` — but ONLY when
            // the cursor is on a non-blank. On whitespace, `cw` behaves like
            // `dw` (changes just the whitespace up to the next word), so the
            // conversion is skipped.
            Motion::WordFwd
                if op == Operator::Change
                    && ed.char_at_cursor().is_some_and(|c| !c.is_whitespace()) =>
            {
                Motion::WordEnd
            }
            Motion::BigWordFwd
                if op == Operator::Change
                    && ed.char_at_cursor().is_some_and(|c| !c.is_whitespace()) =>
            {
                Motion::BigWordEnd
            }
            m => m,
        };
        ed.apply_op_with_motion_direct(op, &motion, total);
        if let Motion::Find { ch, forward, till } = &motion {
            ed.set_last_find(Some((*ch, *forward, *till)));
        }
        // Record for dot-repeat: change ops (d/c) plus the buffer-mutating
        // indent ops (`>j` / `<j` etc.).
        if !ed.is_replaying()
            && (op_is_change(op) || matches!(op, Operator::Indent | Operator::Outdent))
        {
            ed.set_last_change(Some(LastChange::OpMotion {
                op,
                motion,
                count: total,
                inserted: None,
            }));
        }
        return true;
    }

    // Unknown — cancel the operator.
    true
}

fn handle_op_after_g<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    op: Operator,
    count1: usize,
) -> bool {
    // Consume the inner count first so a cancelled chord (ctrl-key /
    // non-char) doesn't leak it into the next command.
    let count2 = ed.take_count();
    if input.ctrl {
        return true;
    }
    let total = count1.max(1).saturating_mul(count2.max(1));
    if let Key::Char(ch) = input.key {
        ed.apply_op_g(op, ch, total);
    }
    true
}

fn handle_after_g<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
) -> bool {
    let count = ed.take_count();
    // Visual-mode `g`-commands apply to the active selection rather than
    // entering operator-pending the way the Normal-mode forms do.
    if ed.is_visual() {
        if input.ctrl {
            // `g<C-a>` / `g<C-x>` — sequential increment over the selection.
            if let Key::Char(c) = input.key {
                match c {
                    'a' => ed.adjust_number_visual(count.max(1) as i64, true),
                    'x' => ed.adjust_number_visual(-(count.max(1) as i64), true),
                    _ => {}
                }
            }
            return true;
        }
        if let Key::Char(c) = input.key {
            match c {
                'u' => ed.apply_visual_operator(Operator::Lowercase, count.max(1)),
                'U' => ed.apply_visual_operator(Operator::Uppercase, count.max(1)),
                '~' => ed.apply_visual_operator(Operator::ToggleCase, count.max(1)),
                '?' => ed.apply_visual_operator(Operator::Rot13, count.max(1)),
                'q' => ed.apply_visual_operator(Operator::Reflow, count.max(1)),
                'w' => ed.apply_visual_operator(Operator::ReflowKeepCursor, count.max(1)),
                // `gJ` — join the selected lines without a space.
                'J' => ed.visual_join(false),
                // Extend-the-selection motions go through the shared body.
                'g' | 'e' | 'E' | '_' | 'j' | 'k' | 'M' | 'm' | '*' | '#' => ed.after_g(c, count),
                // Other g-commands have no visual meaning here — swallow.
                _ => {}
            }
        }
        return true;
    }
    // Extract the char and delegate to the shared apply_after_g body.
    // Non-char keys (ctrl sequences etc.) are silently ignored.
    if let Key::Char(ch) = input.key {
        ed.after_g(ch, count);
    }
    true
}

fn handle_after_z<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
) -> bool {
    let count = ed.take_count();
    // Extract the char and delegate to the shared apply_after_z body.
    // Non-char keys (ctrl sequences etc.) are silently ignored.
    if let Key::Char(ch) = input.key {
        ed.after_z(ch, count);
    }
    true
}

fn handle_replace<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
) -> bool {
    // Consume the stashed count up front so a cancelled chord (Esc or any
    // non-char key) doesn't leak it into the next command.
    let count = ed.take_count();
    if let Key::Char(ch) = input.key {
        if ed.fsm_mode() == FsmMode::VisualBlock {
            ed.replace_block_char(ch);
            return true;
        }
        // B2: charwise / linewise Visual `r<ch>` — replace the whole
        // selection, not a single char at the cursor (that's the
        // normal-mode-only `replace_char_at` path below).
        if matches!(ed.fsm_mode(), FsmMode::Visual | FsmMode::VisualLine) {
            ed.visual_replace_char(ch);
            return true;
        }
        ed.replace_char_at(ch, count.max(1));
        if !ed.is_replaying() {
            ed.set_last_change(Some(LastChange::ReplaceChar {
                ch,
                count: count.max(1),
            }));
        }
    }
    true
}

fn handle_find_target<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    forward: bool,
    till: bool,
) -> bool {
    // Consume the count first: a cancelled chord (Esc / non-char) must not
    // leak the stashed count into the next command.
    let count = ed.take_count();
    let Key::Char(ch) = input.key else {
        return true;
    };
    ed.find_char(ch, forward, till, count.max(1));
    true
}

fn handle_op_find_target<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    op: Operator,
    count1: usize,
    forward: bool,
    till: bool,
) -> bool {
    // Consume the inner count first so a cancelled chord doesn't leak it.
    let count2 = ed.take_count();
    let Key::Char(ch) = input.key else {
        return true;
    };
    let total = count1.max(1).saturating_mul(count2.max(1));
    ed.apply_op_find(op, ch, forward, till, total);
    true
}

fn handle_text_object<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    op: Operator,
    count1: usize,
    inner: bool,
) -> bool {
    // Counts multiply across the operator and the text object: both `2di{` and
    // `d2i{` target the 2nd enclosing pair. For bracket objects this selects
    // the Nth enclosing pair; non-bracket objects ignore the count (as in vim).
    // Consumed before the char check so a cancelled chord doesn't leak it.
    let count2 = ed.take_count();
    let Key::Char(ch) = input.key else {
        return true;
    };
    let total = count1.max(1).saturating_mul(count2.max(1));
    // Delegate to shared implementation; unknown chars are a no-op (return true
    // to consume the key from the FSM regardless).
    ed.apply_op_text_obj(op, ch, inner, total);
    true
}

fn handle_visual_text_obj<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    inner: bool,
) -> bool {
    let Key::Char(ch) = input.key else {
        return true;
    };
    ed.visual_text_obj_extend(ch, inner);
    true
}

// ─── Section-motion chord handlers ────────────────────────────────────────

/// `[[` — backward to previous `{` at col 0; `[]` — backward to `}` at col 0.
fn handle_after_square_bracket_open<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    count: usize,
) -> bool {
    // `[p` / `[P` — indent-adjusted paste ABOVE the current line.
    if let Key::Char('p' | 'P') = input.key {
        ed.paste_reindent(true, count.max(1));
        return true;
    }
    let motion = match input.key {
        Key::Char('[') => Motion::SectionBackward,
        Key::Char(']') => Motion::SectionEndBackward,
        // `[(` / `[{` — previous unmatched open bracket.
        Key::Char('(') => Motion::UnmatchedBracket {
            forward: false,
            open: '(',
        },
        Key::Char('{') => Motion::UnmatchedBracket {
            forward: false,
            open: '{',
        },
        _ => return true, // unknown second key — cancel silently
    };
    ed.execute_motion(motion, count);
    true
}

/// `]]` — forward to next `{` at col 0; `][` — forward to `}` at col 0.
fn handle_after_square_bracket_close<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    count: usize,
) -> bool {
    // `]p` — indent-adjusted paste BELOW; `]P` — indent-adjusted paste ABOVE.
    match input.key {
        Key::Char('p') => {
            ed.paste_reindent(false, count.max(1));
            return true;
        }
        Key::Char('P') => {
            ed.paste_reindent(true, count.max(1));
            return true;
        }
        _ => {}
    }
    let motion = match input.key {
        Key::Char(']') => Motion::SectionForward,
        Key::Char('[') => Motion::SectionEndForward,
        // `])` / `]}` — next unmatched close bracket.
        Key::Char(')') => Motion::UnmatchedBracket {
            forward: true,
            open: '(',
        },
        Key::Char('}') => Motion::UnmatchedBracket {
            forward: true,
            open: '{',
        },
        _ => return true,
    };
    ed.execute_motion(motion, count);
    true
}

/// Operator + `[[` / `[]`.
fn handle_op_after_square_bracket_open<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    op: Operator,
    count1: usize,
) -> bool {
    // Consume the inner count first so an unknown second key (cancel path)
    // doesn't leak it into the next command.
    let count2 = ed.take_count();
    let motion = match input.key {
        Key::Char('[') => Motion::SectionBackward,
        Key::Char(']') => Motion::SectionEndBackward,
        Key::Char('(') => Motion::UnmatchedBracket {
            forward: false,
            open: '(',
        },
        Key::Char('{') => Motion::UnmatchedBracket {
            forward: false,
            open: '{',
        },
        _ => return true,
    };
    let total = count1.max(1).saturating_mul(count2.max(1));
    ed.apply_op_with_motion_direct(op, &motion, total);
    true
}

/// Operator + `]]` / `][`.
fn handle_op_after_square_bracket_close<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    op: Operator,
    count1: usize,
) -> bool {
    // Consume the inner count first (mirrors the `[`-prefix handler).
    let count2 = ed.take_count();
    let motion = match input.key {
        Key::Char(']') => Motion::SectionForward,
        Key::Char('[') => Motion::SectionEndForward,
        Key::Char(')') => Motion::UnmatchedBracket {
            forward: true,
            open: '(',
        },
        Key::Char('}') => Motion::UnmatchedBracket {
            forward: true,
            open: '{',
        },
        _ => return true,
    };
    let total = count1.max(1).saturating_mul(count2.max(1));
    ed.apply_op_with_motion_direct(op, &motion, total);
    true
}

// ─── Pure utility helpers (no Editor mutation) ─────────────────────────────

fn char_to_operator(c: char) -> Option<Operator> {
    match c {
        'd' => Some(Operator::Delete),
        'c' => Some(Operator::Change),
        'y' => Some(Operator::Yank),
        '>' => Some(Operator::Indent),
        '<' => Some(Operator::Outdent),
        '=' => Some(Operator::AutoIndent),
        _ => None,
    }
}

fn visual_operator(input: &Input) -> Option<Operator> {
    if input.ctrl {
        return None;
    }
    match input.key {
        Key::Char('y') => Some(Operator::Yank),
        Key::Char('d') | Key::Char('x') => Some(Operator::Delete),
        Key::Char('c') | Key::Char('s') => Some(Operator::Change),
        // Case operators — shift forms apply to the active selection.
        Key::Char('U') => Some(Operator::Uppercase),
        Key::Char('u') => Some(Operator::Lowercase),
        Key::Char('~') => Some(Operator::ToggleCase),
        // Indent operators on selection.
        Key::Char('>') => Some(Operator::Indent),
        Key::Char('<') => Some(Operator::Outdent),
        // Auto-indent selection.
        Key::Char('=') => Some(Operator::AutoIndent),
        _ => None,
    }
}

fn find_entry(input: &Input) -> Option<(bool, bool)> {
    if input.ctrl {
        return None;
    }
    match input.key {
        Key::Char('f') => Some((true, false)),
        Key::Char('F') => Some((false, false)),
        Key::Char('t') => Some((true, true)),
        Key::Char('T') => Some((false, true)),
        _ => None,
    }
}

// ─── Sneak chord handlers ──────────────────────────────────────────────────

/// Handle the first char of a bare sneak (no operator).
/// Transitions to `SneakSecond` so the second char can be captured.
///
/// State machine: `SneakFirst` → char1 → `SneakSecond { c1 }`
///                `SneakSecond` → char2 → `apply_sneak(c1, c2)`
///                Either state + Esc/non-char → cancel.
fn handle_sneak_first<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    forward: bool,
    count: usize,
) -> bool {
    match input.key {
        Key::Esc => {
            // Cancel silently.
            true
        }
        Key::Char(c1) => {
            // Store char1, wait for char2 via SneakSecond.
            ed.set_pending(hjkl_engine::Pending::SneakSecond { c1, forward, count });
            true
        }
        _ => {
            // Non-char key (other than Esc) cancels.
            true
        }
    }
}

/// Handle the second char of a bare sneak: we have char1, this is char2.
/// Execute the jump.
fn handle_sneak_second<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    c1: char,
    forward: bool,
    count: usize,
) -> bool {
    match input.key {
        Key::Esc => true, // Cancel.
        Key::Char(c2) => {
            ed.sneak(c1, c2, forward, count.max(1));
            true
        }
        _ => true, // Cancel on non-char.
    }
}

/// Handle the first char of an op+sneak (`dsXY` — this is `X`).
fn handle_op_sneak_first<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    op: Operator,
    count1: usize,
    forward: bool,
) -> bool {
    match input.key {
        Key::Esc => {
            // Cancel — drop any inner count so it doesn't leak.
            ed.reset_count();
            true
        }
        Key::Char(c1) => {
            ed.set_pending(hjkl_engine::Pending::OpSneakSecond {
                op,
                count1,
                c1,
                forward,
            });
            true
        }
        _ => {
            ed.reset_count();
            true
        }
    }
}

/// Handle the second char of an op+sneak (`dsXY` — this is `Y`).
fn handle_op_sneak_second<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: Input,
    op: Operator,
    count1: usize,
    c1: char,
    forward: bool,
) -> bool {
    // Consume the inner count first so a cancelled chord doesn't leak it.
    let count2 = ed.take_count();
    match input.key {
        Key::Esc => true,
        Key::Char(c2) => {
            let total = count1.max(1).saturating_mul(count2.max(1));
            ed.apply_op_sneak(op, c1, c2, forward, total);
            true
        }
        _ => true,
    }
}
