//! Vim FSM: operator.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{
    InsertEntry, InsertReason, LastChange, LastHorizontalMotion, Mode, Motion, Operator, Pending,
    RangeKind,
};

use hjkl_engine::input::{Input, Key};

use super::*;
use crate::vim_state::{vim, vim_mut};
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_cursor_pos, buf_line, buf_row_count, buf_set_cursor_rc};

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
pub(crate) fn apply_op_motion_key<H: hjkl_engine::types::Host>(
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
    // Vim quirk (`:h cw`): `cw`/`cW` act like `ce`/`cE` — but ONLY when the
    // cursor is on a non-blank. On whitespace, `cw` behaves like `dw` (changes
    // just the whitespace up to the next word), so the conversion is skipped.
    let cursor_on_nonblank = {
        let (r, c) = ed.cursor();
        buf_line(ed.buffer(), r)
            .and_then(|l| l.chars().nth(c))
            .map(|ch| !ch.is_whitespace())
            .unwrap_or(false)
    };
    let motion = match motion {
        Motion::FindRepeat { reverse } => match vim(ed).last_find {
            Some((ch, forward, till)) => Motion::Find {
                ch,
                forward: if reverse { !forward } else { forward },
                till,
            },
            None => return,
        },
        Motion::WordFwd if op == Operator::Change && cursor_on_nonblank => Motion::WordEnd,
        Motion::BigWordFwd if op == Operator::Change && cursor_on_nonblank => Motion::BigWordEnd,
        m => m,
    };
    apply_op_with_motion(ed, op, &motion, total_count);
    if let Motion::Find { ch, forward, till } = &motion {
        vim_mut(ed).last_find = Some((*ch, *forward, *till));
    }
    if !vim(ed).replaying && op_is_change(op) {
        vim_mut(ed).last_change = Some(LastChange::OpMotion {
            op,
            motion,
            count: total_count,
            inserted: None,
        });
    }
}
/// Public(crate) entry: apply doubled-letter line op (`dd`/`yy`/`cc`/`>>`/`<<`/`gcc`).
/// Called by `Editor::apply_op_double` (the public controller API).
pub(crate) fn apply_op_double<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    total_count: usize,
) {
    if op == Operator::Comment {
        // `gcc` / `{N}gcc` — toggle comment on `total_count` lines starting at cursor.
        let row = buf_cursor_pos(ed.buffer()).row;
        let end_row = (row + total_count.max(1) - 1).min(ed.buffer().row_count().saturating_sub(1));
        ed.toggle_comment_range(row, end_row);
        vim_mut(ed).mode = Mode::Normal;
        if !vim(ed).replaying {
            vim_mut(ed).last_change = Some(LastChange::LineOp {
                op,
                count: total_count,
                inserted: None,
            });
        }
        return;
    }
    execute_line_op(ed, op, total_count);
    if !vim(ed).replaying {
        vim_mut(ed).last_change = Some(LastChange::LineOp {
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
pub(crate) fn gn_find_range<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    re: &regex::Regex,
    forward: bool,
) -> Option<(hjkl_engine::types::Pos, hjkl_engine::types::Pos)> {
    use hjkl_engine::types::{Cursor, Pos, Search};
    let cursor = Cursor::cursor(ed.buffer());
    let contains =
        Search::find_prev(ed.buffer(), cursor, re).filter(|m| m.start <= cursor && cursor < m.end);
    let range = if let Some(m) = contains {
        m
    } else if forward {
        Search::find_next(ed.buffer(), cursor, re)?
    } else {
        Search::find_prev(ed.buffer(), cursor, re)?
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
pub(crate) fn gn_operate<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Option<Operator>,
    forward: bool,
    count: usize,
) {
    use hjkl_engine::types::{Cursor, Pos};
    // Make sure the compiled pattern reflects the last `/` or `*` search.
    if let Some(p) = ed.last_search_pattern().map(str::to_string) {
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
        Cursor::set_cursor(ed.buffer_mut(), past);
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
            vim_mut(ed).visual_anchor = start_t;
            buf_set_cursor_rc(ed.buffer_mut(), end_t.0, end_t.1);
            vim_mut(ed).mode = Mode::Visual;
            vim_mut(ed).current_mode = hjkl_engine::VimMode::Visual;
            ed.push_buffer_cursor_to_textarea();
        }
        Some(Operator::Delete) => {
            ed.push_undo();
            cut_vim_range(ed, start_t, end_t, RangeKind::Inclusive);
            // Deleting at the line end can leave the cursor one past the last
            // char; vim clamps it back onto the line.
            clamp_cursor_to_normal_mode(ed);
            ed.push_buffer_cursor_to_textarea();
            if !vim(ed).replaying {
                vim_mut(ed).last_change = Some(LastChange::GnOp {
                    op: Operator::Delete,
                    forward,
                    inserted: None,
                });
            }
        }
        Some(Operator::Change) => {
            ed.push_undo();
            vim_mut(ed).change_mark_start = Some(start_t);
            cut_vim_range(ed, start_t, end_t, RangeKind::Inclusive);
            if !vim(ed).replaying {
                vim_mut(ed).last_change = Some(LastChange::GnOp {
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
                let target = vim_mut(ed).pending_register.take();
                ed.record_yank(text, false, target);
            }
            buf_set_cursor_rc(ed.buffer_mut(), start_t.0, start_t.1);
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
pub(crate) fn apply_op_g_inner<H: hjkl_engine::types::Host>(
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
            if !vim(ed).replaying {
                vim_mut(ed).last_change = Some(LastChange::LineOp {
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
    if !vim(ed).replaying && op_is_change(op) {
        vim_mut(ed).last_change = Some(LastChange::OpMotion {
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
pub(crate) fn apply_after_g<H: hjkl_engine::types::Host>(
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
            ed.set_sticky_col(Some(ed.cursor().1));
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
        // `gm` — middle of the screen line (viewport_width/2, clamped to EOL).
        'm' => execute_motion(ed, Motion::ScreenLineMiddle, count),
        // `gv` — re-enter the last visual selection. Calls the bridge in this
        // module directly; routing back out through the `Editor` wrapper was
        // pure indirection, and that wrapper now lives on
        // `hjkl_vim::VimEditorExt` (#267).
        'v' => reenter_last_visual_bridge(ed),
        // `gj` / `gk` — display-line down / up. Walks one screen
        // segment at a time under `:set wrap`; falls back to `j`/`k`
        // when wrap is off (Buffer::move_screen_* handles the branch).
        'j' => execute_motion(ed, Motion::ScreenDown, count),
        'k' => execute_motion(ed, Motion::ScreenUp, count),
        // Case operators: `gU` / `gu` / `g~`. Enter operator-pending
        // so the next input is treated as the motion / text object /
        // shorthand double (`gUU`, `guu`, `g~~`).
        'U' => {
            vim_mut(ed).pending = Pending::Op {
                op: Operator::Uppercase,
                count1: count,
            };
        }
        'u' => {
            vim_mut(ed).pending = Pending::Op {
                op: Operator::Lowercase,
                count1: count,
            };
        }
        '~' => {
            vim_mut(ed).pending = Pending::Op {
                op: Operator::ToggleCase,
                count1: count,
            };
        }
        '?' => {
            // `g?{motion}` — ROT13 operator (`g??` / `g?g?` doubled).
            vim_mut(ed).pending = Pending::Op {
                op: Operator::Rot13,
                count1: count,
            };
        }
        'q' => {
            // `gq{motion}` — text reflow operator. Subsequent motion
            // / textobj rides the same operator pipeline.
            vim_mut(ed).pending = Pending::Op {
                op: Operator::Reflow,
                count1: count,
            };
        }
        'w' => {
            // `gw{motion}` — same reflow as `gq` but cursor stays at
            // its pre-reflow position (clamped to new EOL if shorter).
            vim_mut(ed).pending = Pending::Op {
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
                if !join_line_raw(ed) {
                    break;
                }
            }
            if !vim(ed).replaying {
                vim_mut(ed).last_change = Some(LastChange::JoinLine { count: joins });
            }
        }
        'd' => {
            // `gd` — goto definition. hjkl-engine doesn't run an LSP
            // itself; raise an intent the host drains and routes to
            // `sqls`. The cursor stays put here — the host moves it
            // once it has the target location.
            ed.set_pending_lsp(Some(hjkl_engine::LspIntent::GotoDefinition));
        }
        // `gi` — go to last-insert position and re-enter insert mode.
        // Matches vim's `:h gi`: moves to the `'^` mark position (the
        // cursor where insert mode was last active, before Esc step-back)
        // and enters insert mode there.
        'i' => {
            if let Some((row, col)) = vim(ed).last_insert_pos {
                ed.jump_cursor(row, col);
            }
            begin_insert(ed, count.max(1), InsertReason::Enter(InsertEntry::I));
        }
        // `gc` — enter operator-pending for the comment-toggle operator.
        // `gcc` (doubled 'c') is the line-wise form; `gc{motion}` is the
        // motion form. The operator is Comment — the app layer (or the
        // doubled-char path in handle_after_op) calls toggle_comment_range.
        'c' => {
            vim_mut(ed).pending = Pending::Op {
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
            let cmd = match ed.last_substitute().cloned() {
                Some(c) => c,
                None => {
                    // No prior substitute — mirror the `:&` error path; do
                    // nothing to the buffer (the host's status line will show
                    // the pending error if wired; for headless / test hosts
                    // we simply return silently).
                    return;
                }
            };
            let last_row = buf_row_count(ed.buffer()).saturating_sub(1) as u32;
            let r = 0u32..=last_row;
            // apply_substitute moves cursor to last changed line and pushes
            // one undo snapshot — same semantics as `:&&` / `:%s//~/&`.
            let _ = hjkl_engine::substitute::apply_substitute(ed, &cmd, r);
            // Update stored substitute so subsequent `g&` sees the same cmd.
            // (apply_substitute doesn't call set_last_substitute itself.)
            ed.set_last_substitute(cmd);
        }
        _ => {}
    }
}
/// Normal-mode `&` — repeat the last `:s` on the current line, dropping the
/// previous flags (vim: `&` ≡ `:s` with no flags). `g&` keeps flags + whole
/// buffer; this is the single-line, flag-less form.
pub(crate) fn ampersand_repeat<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    let Some(mut cmd) = ed.last_substitute().cloned() else {
        return;
    };
    cmd.flags = hjkl_engine::substitute::SubstFlags::default();
    let row = buf_cursor_pos(ed.buffer()).row as u32;
    let _ = hjkl_engine::substitute::apply_substitute(ed, &cmd, row..=row);
}
/// Public(crate) entry point for bare `z<x>`. Applies the z-chord effect
/// given the char `ch` and pre-captured `count`. Called by `Editor::after_z`
/// (the public controller API) so the hjkl-vim pending-state reducer can
/// dispatch `AfterZChord` without re-entering the engine FSM.
pub(crate) fn apply_after_z<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    ch: char,
    count: usize,
) {
    use hjkl_engine::CursorScrollTarget;
    let row = ed.cursor().0;
    match ch {
        'z' => {
            ed.scroll_cursor_to(CursorScrollTarget::Center);
            ed.set_viewport_pinned(true);
            ed.set_scroll_anim_hint(true);
        }
        't' => {
            ed.scroll_cursor_to(CursorScrollTarget::Top);
            ed.set_viewport_pinned(true);
            ed.set_scroll_anim_hint(true);
        }
        'b' => {
            ed.scroll_cursor_to(CursorScrollTarget::Bottom);
            ed.set_viewport_pinned(true);
            ed.set_scroll_anim_hint(true);
        }
        // Folds — operate on the fold under the cursor (or the
        // whole buffer for `R` / `M`). Routed through
        // [`Editor::apply_fold_op`] (0.0.38 Patch C-δ.4) so the host
        // can observe / veto each op via [`Editor::take_fold_ops`].
        'o' => {
            ed.apply_fold_op(hjkl_engine::types::FoldOp::OpenAt(row));
        }
        'c' => {
            ed.apply_fold_op(hjkl_engine::types::FoldOp::CloseAt(row));
        }
        'a' => {
            ed.apply_fold_op(hjkl_engine::types::FoldOp::ToggleAt(row));
        }
        'R' => {
            ed.apply_fold_op(hjkl_engine::types::FoldOp::OpenAll);
        }
        'M' => {
            ed.apply_fold_op(hjkl_engine::types::FoldOp::CloseAll);
        }
        'E' => {
            ed.apply_fold_op(hjkl_engine::types::FoldOp::ClearAll);
        }
        'd' => {
            ed.apply_fold_op(hjkl_engine::types::FoldOp::RemoveAt(row));
        }
        'f' => {
            if matches!(
                vim(ed).mode,
                Mode::Visual | Mode::VisualLine | Mode::VisualBlock
            ) {
                // `zf` over a Visual selection creates a fold spanning
                // anchor → cursor.
                let anchor_row = match vim(ed).mode {
                    Mode::VisualLine => vim(ed).visual_line_anchor,
                    Mode::VisualBlock => vim(ed).block_anchor.0,
                    _ => vim(ed).visual_anchor.0,
                };
                let cur = ed.cursor().0;
                let top = anchor_row.min(cur);
                let bot = anchor_row.max(cur);
                ed.apply_fold_op(hjkl_engine::types::FoldOp::Add {
                    start_row: top,
                    end_row: bot,
                    closed: true,
                });
                vim_mut(ed).mode = Mode::Normal;
            } else {
                // `zf{motion}` / `zf{textobj}` — route through the
                // operator pipeline. `Operator::Fold` reuses every
                // motion / text-object / `g`-prefix branch the other
                // operators get.
                vim_mut(ed).pending = Pending::Op {
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
pub(crate) fn apply_find_char<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    ch: char,
    forward: bool,
    till: bool,
    count: usize,
) {
    execute_motion(ed, Motion::Find { ch, forward, till }, count.max(1));
    vim_mut(ed).last_find = Some((ch, forward, till));
    vim_mut(ed).last_horizontal_motion = LastHorizontalMotion::FindChar;
}
#[cfg(test)]
mod indent_count_tests {
    use super::*;
    use hjkl_buffer::Buffer;
    use hjkl_engine::{DefaultHost, Editor, Options};

    fn make_editor(content: &str) -> Editor<Buffer, DefaultHost> {
        let buf = Buffer::from_str(content);
        let mut ed = crate::vim::vim_editor(buf, DefaultHost::new(), Options::default());
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

    // #263: `>>` under `noexpandtab` must insert a hard tab, not spaces.
    #[test]
    fn indent_noexpandtab_inserts_tab() {
        let mut ed = make_editor("hello\n");
        ed.settings_mut().expandtab = false;
        ed.settings_mut().shiftwidth = 4;
        ed.settings_mut().tabstop = 4;
        ed.jump_cursor(0, 0);
        execute_line_op(&mut ed, Operator::Indent, 1);
        assert_eq!(content(&ed), "\thello\n");
    }

    // #263: a sub-tabstop shiftwidth under `noexpandtab` pads with spaces for
    // the remainder (shiftwidth=2 < tabstop=4 → two spaces, no tab).
    #[test]
    fn indent_noexpandtab_subtab_remainder_is_spaces() {
        let mut ed = make_editor("hello\n");
        ed.settings_mut().expandtab = false;
        ed.settings_mut().shiftwidth = 2;
        ed.settings_mut().tabstop = 4;
        ed.jump_cursor(0, 0);
        execute_line_op(&mut ed, Operator::Indent, 1);
        assert_eq!(content(&ed), "  hello\n");
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
