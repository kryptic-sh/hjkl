//! Vim FSM: visual.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{LastVisual, Mode, Operator, Pending};

use super::*;
use crate::vim_state::{vim, vim_mut};
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::buf_set_cursor_rc;

/// Drop the `Blame` view overlay whenever the input mode is no longer
/// `Normal`. BLAME is a Normal-only read-only view; entering Insert/Visual/etc.
/// (by keyboard, mouse drag, or programmatic transition) implicitly leaves it.
/// Called from every mode-transition funnel so the FSM is the single source of
/// truth — the host never has to police this.
#[inline]
pub fn drop_blame_if_left_normal<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    if vim(ed).current_mode != hjkl_engine::VimMode::Normal {
        ed.set_view_mode(hjkl_engine::ViewMode::Normal);
    }
}
/// Helper — set both the FSM-internal `mode` and the stable `current_mode`
/// field in one call. Every Phase 6.3 bridge that changes mode calls this so
/// `vim_mode()` stays correct without going through the FSM's `step()` loop.
#[inline]
pub(crate) fn set_vim_mode_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    mode: Mode,
) {
    vim_mut(ed).mode = mode;
    vim_mut(ed).current_mode = vim_mut(ed).public_mode();
    drop_blame_if_left_normal(ed);
}
/// `v` from Normal — enter charwise Visual mode. Anchors at the current
/// cursor position; the cursor IS the live end of the selection.
pub(crate) fn enter_visual_char_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    let cur = ed.cursor();
    vim_mut(ed).visual_anchor = cur;
    set_vim_mode_bridge(ed, Mode::Visual);
}
/// `V` from Normal — enter linewise Visual mode. Anchors the whole line
/// containing the current cursor; `o` still swaps the anchor row.
pub(crate) fn enter_visual_line_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    let (row, _) = ed.cursor();
    vim_mut(ed).visual_line_anchor = row;
    set_vim_mode_bridge(ed, Mode::VisualLine);
}
/// `<C-v>` from Normal — enter Visual-block mode. Anchors at the current
/// cursor; `block_vcol` is seeded from the cursor column so h/l navigation
/// preserves the desired virtual column.
pub(crate) fn enter_visual_block_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    let cur = ed.cursor();
    vim_mut(ed).block_anchor = cur;
    vim_mut(ed).block_vcol = cur.1;
    set_vim_mode_bridge(ed, Mode::VisualBlock);
}
/// Esc from any visual mode — set `<` / `>` marks (per `:h v_:`), stash the
/// selection for `gv` re-entry, and return to Normal. Replicates the
/// `pre_visual_snapshot` logic in `step()` so callers outside the FSM get
/// identical behaviour.
pub(crate) fn exit_visual_to_normal_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    // Build the same snapshot that `step()` captures at pre-step time.
    let snap: Option<LastVisual> = match vim(ed).mode {
        Mode::Visual => Some(LastVisual {
            mode: Mode::Visual,
            anchor: vim(ed).visual_anchor,
            cursor: ed.cursor(),
            block_vcol: 0,
        }),
        Mode::VisualLine => Some(LastVisual {
            mode: Mode::VisualLine,
            anchor: (vim(ed).visual_line_anchor, 0),
            cursor: ed.cursor(),
            block_vcol: 0,
        }),
        Mode::VisualBlock => Some(LastVisual {
            mode: Mode::VisualBlock,
            anchor: vim(ed).block_anchor,
            cursor: ed.cursor(),
            block_vcol: vim(ed).block_vcol,
        }),
        _ => None,
    };
    // Transition to Normal first (matches FSM order).
    vim_mut(ed).pending = Pending::None;
    vim_mut(ed).count = 0;
    vim_mut(ed).insert_session = None;
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
        vim_mut(ed).last_visual = Some(snap);
    }
}
/// `o` in Visual / VisualLine / VisualBlock — swap the cursor and anchor
/// without mutating the selection range. In charwise mode the cursor jumps
/// to the old anchor and the anchor takes the old cursor. In linewise mode
/// the anchor *row* swaps with the current cursor row. In block mode the
/// block corners swap.
pub(crate) fn visual_o_toggle_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    match vim(ed).mode {
        Mode::Visual => {
            let cur = ed.cursor();
            let anchor = vim(ed).visual_anchor;
            vim_mut(ed).visual_anchor = cur;
            ed.jump_cursor(anchor.0, anchor.1);
        }
        Mode::VisualLine => {
            let cur_row = ed.cursor().0;
            let anchor_row = vim(ed).visual_line_anchor;
            vim_mut(ed).visual_line_anchor = cur_row;
            ed.jump_cursor(anchor_row, 0);
        }
        Mode::VisualBlock => {
            let cur = ed.cursor();
            let anchor = vim(ed).block_anchor;
            vim_mut(ed).block_anchor = cur;
            vim_mut(ed).block_vcol = anchor.1;
            ed.jump_cursor(anchor.0, anchor.1);
        }
        _ => {}
    }
}
/// `gv` — restore the last visual selection (mode + anchor + cursor).
/// No-op if no selection was ever stored. Mirrors the `gv` arm in
/// `handle_normal_g`.
pub(crate) fn reenter_last_visual_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    if let Some(snap) = vim(ed).last_visual {
        match snap.mode {
            Mode::Visual => {
                vim_mut(ed).visual_anchor = snap.anchor;
                set_vim_mode_bridge(ed, Mode::Visual);
            }
            Mode::VisualLine => {
                vim_mut(ed).visual_line_anchor = snap.anchor.0;
                set_vim_mode_bridge(ed, Mode::VisualLine);
            }
            Mode::VisualBlock => {
                vim_mut(ed).block_anchor = snap.anchor;
                vim_mut(ed).block_vcol = snap.block_vcol;
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
pub(crate) fn set_mode_bridge<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    mode: hjkl_engine::VimMode,
) {
    let internal = match mode {
        hjkl_engine::VimMode::Normal => Mode::Normal,
        hjkl_engine::VimMode::Insert => Mode::Insert,
        hjkl_engine::VimMode::Visual => Mode::Visual,
        hjkl_engine::VimMode::VisualLine => Mode::VisualLine,
        hjkl_engine::VimMode::VisualBlock => Mode::VisualBlock,
    };
    vim_mut(ed).mode = internal;
    vim_mut(ed).current_mode = mode;
    drop_blame_if_left_normal(ed);
}
/// `m{ch}` — public controller entry point. Validates `ch` (must be
/// alphanumeric to match vim's mark-name rules) and records the current
/// cursor position under that name. Promoted to the public surface in 0.6.7
/// so the hjkl-vim `PendingState::SetMark` reducer can dispatch
/// `EngineCmd::SetMark` without re-entering the engine FSM.
/// `handle_set_mark` delegates here to avoid logic duplication.
pub(crate) fn set_mark_at_cursor<H: hjkl_engine::types::Host>(
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
pub(crate) fn goto_mark<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    ch: char,
    linewise: bool,
) {
    let target = match ch {
        'a'..='z' => ed.mark(ch),
        '\'' | '`' => ed.jump_back_list().last().copied(),
        '.' => ed.last_edit_pos(),
        '[' | ']' | '<' | '>' => ed.mark(ch),
        _ => None,
    };
    let Some((row, col)) = target else {
        return;
    };
    let pre = ed.cursor();
    let (r, c_clamped) = clamp_pos(ed, (row, col));
    if linewise {
        buf_set_cursor_rc(ed.buffer_mut(), r, 0);
        ed.push_buffer_cursor_to_textarea();
        move_first_non_whitespace(ed);
    } else {
        buf_set_cursor_rc(ed.buffer_mut(), r, c_clamped);
        ed.push_buffer_cursor_to_textarea();
    }
    if ed.cursor() != pre {
        ed.push_jump(pre);
    }
    ed.set_sticky_col(Some(ed.cursor().1));
}
/// Unified mark-jump entry point that returns a [`hjkl_engine::MarkJump`]
/// so the app layer can decide whether to switch buffers.
///
/// - Uppercase marks (`'A'`–`'Z'`) look in `global_marks`. If the stored
///   `buffer_id` differs from `ed.current_buffer_id()`, returns
///   `CrossBuffer`. Same-buffer uppercase marks execute the jump normally.
/// - All other legal mark chars delegate to [`goto_mark`] and return
///   `SameBuffer`.
pub(crate) fn try_goto_mark<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    ch: char,
    linewise: bool,
) -> hjkl_engine::MarkJump {
    use hjkl_engine::MarkJump;
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
                buf_set_cursor_rc(ed.buffer_mut(), r, 0);
                ed.push_buffer_cursor_to_textarea();
                move_first_non_whitespace(ed);
            } else {
                buf_set_cursor_rc(ed.buffer_mut(), r, c_clamped);
                ed.push_buffer_cursor_to_textarea();
            }
            if ed.cursor() != pre {
                ed.push_jump(pre);
            }
            ed.set_sticky_col(Some(ed.cursor().1));
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
