//! Phase 6.6e: normal-mode FSM body relocated from `hjkl-engine::vim`.
//!
//! Dispatched by [`crate::dispatch_input`] for all non-insert,
//! non-search-prompt modes (Normal, Visual, VisualLine, VisualBlock).
//!
//! The engine keeps in-engine duplicate bodies (`step_normal` +
//! `handle_normal_only`) in `vim::step` for back-compat with the deprecated
//! `Editor::step_input` / `Editor::step_input_raw` shim path until Phase 6.6h.
use hjkl_engine::{
    FsmMode, Host, Input, Key, LastChange, Motion, Operator, Pending, ScrollDir, VimMode,
    op_is_change, parse_motion,
};

// ─── Public entry point ────────────────────────────────────────────────────

/// Drive the normal / visual / operator-pending FSM for one keystroke.
///
/// Returns `true` when the input was consumed. Every key is consumed in
/// these modes (unknown keys swallow silently to avoid TUI bubbling).
pub fn step_normal<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
        Pending::None => {}
    }

    let count = ed.take_count();

    // Common normal / visual keys.
    match input.key {
        Key::Esc => {
            ed.force_normal();
            return true;
        }
        Key::Char('v') if !input.ctrl && ed.fsm_mode() == FsmMode::Normal => {
            ed.set_visual_anchor(ed.cursor());
            ed.set_mode(VimMode::Visual);
            return true;
        }
        Key::Char('V') if !input.ctrl && ed.fsm_mode() == FsmMode::Normal => {
            let (row, _) = ed.cursor();
            ed.set_visual_line_anchor(row);
            ed.set_mode(VimMode::VisualLine);
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

    // Visual mode: operators act on the current selection.
    if ed.is_visual()
        && let Some(op) = visual_operator(&input)
    {
        ed.apply_visual_operator(op);
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
                let (top, bot, _left, right) = ed.visual_block_bounds();
                let line_len = ed.line_char_count(top);
                let col = (right + 1).min(line_len);
                ed.visual_block_append_at_right(top, bot, col);
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
                ed.redo();
                return true;
            }
            'a' if ed.fsm_mode() == FsmMode::Normal => {
                ed.adjust_number(count.max(1) as i64);
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

    // `g` prefix.
    if !input.ctrl && input.key == Key::Char('g') && ed.fsm_mode() == FsmMode::Normal {
        ed.set_count(count);
        ed.set_pending(Pending::G);
        return true;
    }

    // `z` prefix (zz / zt / zb — cursor-relative viewport scrolls).
    if !input.ctrl
        && input.key == Key::Char('z')
        && matches!(
            ed.fsm_mode(),
            FsmMode::Normal | FsmMode::Visual | FsmMode::VisualLine | FsmMode::VisualBlock
        )
    {
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
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
            ed.substitute_char(count);
            true
        }
        Key::Char('S') => {
            ed.substitute_line(count);
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
        Key::Char('u') => {
            ed.undo();
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
        Key::Char('.') => {
            ed.replay_last_change(count);
            true
        }
        _ => false,
    }
}

// ─── Pending chord handlers ────────────────────────────────────────────────

fn handle_set_mark<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: Input,
) -> bool {
    if let Key::Char(c) = input.key {
        ed.set_mark_at_cursor(c);
    }
    true
}

fn handle_select_register<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: Input,
) -> bool {
    if let Key::Char(c) = input.key {
        ed.set_pending_register(c);
    }
    true
}

fn handle_record_macro_target<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
                .registers()
                .read(lower)
                .map(|s| s.text.clone())
                .unwrap_or_default();
            ed.set_recording_keys(hjkl_engine::decode_macro(&text));
        } else {
            ed.set_recording_keys(vec![]);
        }
    }
    true
}

fn handle_play_macro_target<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    let text = match ed.registers().read(reg) {
        Some(slot) if !slot.text.is_empty() => slot.text.clone(),
        _ => return true,
    };
    let keys = hjkl_engine::decode_macro(&text);
    ed.set_last_macro(Some(reg));
    let times = count.max(1);
    let was_replaying = ed.is_replaying_macro_raw();
    ed.set_replaying_macro_raw(true);
    for _ in 0..times {
        for k in keys.iter().copied() {
            crate::dispatch_input(ed, k);
        }
    }
    ed.set_replaying_macro_raw(was_replaying);
    true
}

fn handle_goto_mark<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
        // `==` auto-indents the current line.
        Operator::AutoIndent => Some('='),
        // `!!` filters the current line — vim's doubled form.
        Operator::Filter => Some('!'),
        // `gcc` toggles comment on the current line — doubled 'c' after `gc`.
        Operator::Comment => Some('c'),
    };
    if let Key::Char(c) = input.key
        && !input.ctrl
        && Some(c) == double_ch
    {
        let count2 = ed.take_count();
        let total = count1.max(1) * count2.max(1);
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

    // Motion.
    let count2 = ed.take_count();
    let total = count1.max(1) * count2.max(1);
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
            // Vim quirk: `cw` / `cW` are `ce` / `cE` — don't include
            // trailing whitespace so the user's replacement text lands
            // before the following word's leading space.
            Motion::WordFwd if op == Operator::Change => Motion::WordEnd,
            Motion::BigWordFwd if op == Operator::Change => Motion::BigWordEnd,
            m => m,
        };
        ed.apply_op_with_motion_direct(op, &motion, total);
        if let Motion::Find { ch, forward, till } = &motion {
            ed.set_last_find(Some((*ch, *forward, *till)));
        }
        if !ed.is_replaying() && op_is_change(op) {
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
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: Input,
    op: Operator,
    count1: usize,
) -> bool {
    if input.ctrl {
        return true;
    }
    let count2 = ed.take_count();
    let total = count1.max(1) * count2.max(1);
    if let Key::Char(ch) = input.key {
        ed.apply_op_g(op, ch, total);
    }
    true
}

fn handle_after_g<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: Input,
) -> bool {
    let count = ed.take_count();
    // Extract the char and delegate to the shared apply_after_g body.
    // Non-char keys (ctrl sequences etc.) are silently ignored.
    if let Key::Char(ch) = input.key {
        ed.after_g(ch, count);
    }
    true
}

fn handle_after_z<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: Input,
) -> bool {
    if let Key::Char(ch) = input.key {
        if ed.fsm_mode() == FsmMode::VisualBlock {
            ed.replace_block_char(ch);
            return true;
        }
        let count = ed.take_count();
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
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: Input,
    forward: bool,
    till: bool,
) -> bool {
    let Key::Char(ch) = input.key else {
        return true;
    };
    let count = ed.take_count();
    ed.find_char(ch, forward, till, count.max(1));
    true
}

fn handle_op_find_target<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: Input,
    op: Operator,
    count1: usize,
    forward: bool,
    till: bool,
) -> bool {
    let Key::Char(ch) = input.key else {
        return true;
    };
    let count2 = ed.take_count();
    let total = count1.max(1) * count2.max(1);
    ed.apply_op_find(op, ch, forward, till, total);
    true
}

fn handle_text_object<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: Input,
    op: Operator,
    _count1: usize,
    inner: bool,
) -> bool {
    let Key::Char(ch) = input.key else {
        return true;
    };
    // Delegate to shared implementation; unknown chars are a no-op (return true
    // to consume the key from the FSM regardless).
    ed.apply_op_text_obj(op, ch, inner, 1);
    true
}

fn handle_visual_text_obj<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: Input,
    count: usize,
) -> bool {
    let motion = match input.key {
        Key::Char('[') => Motion::SectionBackward,
        Key::Char(']') => Motion::SectionEndBackward,
        _ => return true, // unknown second key — cancel silently
    };
    ed.execute_motion(motion, count);
    true
}

/// `]]` — forward to next `{` at col 0; `][` — forward to `}` at col 0.
fn handle_after_square_bracket_close<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: Input,
    count: usize,
) -> bool {
    let motion = match input.key {
        Key::Char(']') => Motion::SectionForward,
        Key::Char('[') => Motion::SectionEndForward,
        _ => return true,
    };
    ed.execute_motion(motion, count);
    true
}

/// Operator + `[[` / `[]`.
fn handle_op_after_square_bracket_open<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: Input,
    op: Operator,
    count1: usize,
) -> bool {
    let motion = match input.key {
        Key::Char('[') => Motion::SectionBackward,
        Key::Char(']') => Motion::SectionEndBackward,
        _ => return true,
    };
    let count2 = ed.take_count();
    let total = count1.max(1) * count2.max(1);
    ed.apply_op_with_motion_direct(op, &motion, total);
    true
}

/// Operator + `]]` / `][`.
fn handle_op_after_square_bracket_close<H: Host>(
    ed: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: Input,
    op: Operator,
    count1: usize,
) -> bool {
    let motion = match input.key {
        Key::Char(']') => Motion::SectionForward,
        Key::Char('[') => Motion::SectionEndForward,
        _ => return true,
    };
    let count2 = ed.take_count();
    let total = count1.max(1) * count2.max(1);
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
