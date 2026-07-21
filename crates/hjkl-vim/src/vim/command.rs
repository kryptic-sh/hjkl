//! Vim FSM: command.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{LastChange, Mode, RangeKind};

use hjkl_engine::rope_util::{rope_line_to_str, rope_row_range_str};

use super::*;
use crate::vim_state::{vim, vim_mut};
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{
    buf_cursor_pos, buf_line, buf_line_chars, buf_row_count, buf_set_cursor_pos, buf_set_cursor_rc,
};

/// Read the text in a vim-shaped range without mutating. Used by
/// `Operator::Yank` so we can pipe the same range translation as
/// [`cut_vim_range`] but skip the delete + inverse extraction.
pub(crate) fn read_vim_range<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    start: (usize, usize),
    end: (usize, usize),
    kind: RangeKind,
) -> String {
    let (top, bot) = order(start, end);
    ed.sync_buffer_content_from_textarea();
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
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
/// Cut a vim-shaped range through the View edit funnel and return
/// the deleted text. Translates vim's `RangeKind`
/// (Linewise/Inclusive/Exclusive) into the buffer's
/// `hjkl_buffer::MotionKind` (Line/Char) and applies the right end-
/// position adjustment so inclusive motions actually include the bot
/// cell. Pushes the cut text into the clipboard via `record_yank_to_host`
/// and the textarea yank buffer (still observed by `p`/`P` until the paste
/// path is ported), and updates `yank_linewise` for linewise cuts.
pub(crate) fn cut_vim_range<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
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
            let line_chars = buf_line_chars(ed.buffer(), bot.0);
            // Advance one cell past `bot` so the buffer's exclusive
            // `cut_chars` actually drops the inclusive endpoint. Wrap
            // to the next row when bot already sits on the last char.
            let next = if bot.1 < line_chars {
                Position::new(bot.0, bot.1 + 1)
            } else if bot.0 + 1 < buf_row_count(ed.buffer()) {
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
        let target = vim_mut(ed).pending_register.take();
        ed.record_delete(text.clone(), matches!(kind, RangeKind::Linewise), target);
    }
    ed.push_buffer_cursor_to_textarea();
    text
}
/// `D` / `C` — delete from cursor to end of line through the edit
/// funnel. Pushes the deleted text to the clipboard via `record_yank_to_host`
/// and the textarea's yank buffer (still observed by `p`/`P` until the paste
/// path is ported). Cursor lands at the deletion start so the caller
/// can decide whether to step it left (`D`) or open insert mode (`C`).
pub(crate) fn delete_to_eol<H: hjkl_engine::types::Host>(ed: &mut Editor<hjkl_buffer::View, H>) {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(ed.buffer());
    let line_chars = buf_line_chars(ed.buffer(), cursor.row);
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
        ed.set_yank_linewise(false);
        ed.set_yank(text);
    }
    buf_set_cursor_pos(ed.buffer_mut(), cursor);
    ed.push_buffer_cursor_to_textarea();
}
pub(crate) fn do_char_delete<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
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
        let cursor = buf_cursor_pos(ed.buffer());
        let line_chars = buf_line_chars(ed.buffer(), cursor.row);
        if forward {
            // `x` — delete the char under the cursor. Vim no-ops on
            // an empty line; the buffer would drop a row otherwise.
            // `break`, not `continue`: the cursor can't regress below
            // EOL, so remaining iterations would spin uselessly (a
            // saturated count prefix would hang the editor).
            if cursor.col >= line_chars {
                break;
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
            // `X` — delete the char before the cursor. `break` for the
            // same no-further-progress reason as the `x` arm above.
            if cursor.col == 0 {
                break;
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
        let target = vim_mut(ed).pending_register.take();
        ed.record_delete(deleted, false, target);
    }
    // B11: `x` deleting the last char(s) of a line can leave the cursor one
    // past the new end — vim clamps to the new last column in Normal mode.
    let cursor = buf_cursor_pos(ed.buffer());
    let line_chars = buf_line_chars(ed.buffer(), cursor.row);
    if line_chars > 0 && cursor.col >= line_chars {
        buf_set_cursor_pos(ed.buffer_mut(), Position::new(cursor.row, line_chars - 1));
    }
    ed.push_buffer_cursor_to_textarea();
}
/// Vim `Ctrl-a` / `Ctrl-x` — find the next number at or after the cursor on the
/// current line, add `delta`, leave the cursor on the last digit of the result.
/// Recognises `0x`/`0X` hex literals (incremented in hex, width preserved) as
/// well as signed decimals. No-op if the line has no number to the right.
pub(crate) fn adjust_number<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    delta: i64,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(ed.buffer());
    let row = cursor.row;
    let chars: Vec<char> = match buf_line(ed.buffer(), row) {
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
        let new_val = n as i128 + delta as i128;
        // Vim zero-pads the result back to the original digit width, but
        // only when the original number actually had a leading zero
        // (`:h CTRL-A`): "10" <C-x> -> "9", not "09"; "007" <C-x> -> "006".
        // The `-` sign is never part of the padded width — "-007" <C-a> ->
        // "-006", and crossing zero into negative still pads the digits
        // ("009" 20<C-x> -> "-011").
        let digits: String = chars[digit_start..span_end].iter().collect();
        let width = digits.len();
        let new_s = if width > 1 && digits.starts_with('0') {
            if new_val < 0 {
                let mag = new_val.unsigned_abs();
                format!("-{mag:0width$}")
            } else {
                format!("{new_val:0width$}")
            }
        } else {
            new_val.to_string()
        };
        (span_start, span_end, new_s)
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
    buf_set_cursor_rc(ed.buffer_mut(), row, span_start + new_len.saturating_sub(1));
    ed.push_buffer_cursor_to_textarea();
    true
}
pub(crate) fn replace_char<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    ch: char,
    count: usize,
) {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    // Vim aborts `r{count}{char}` entirely — replacing nothing — when fewer
    // than `count` characters remain from the cursor to end-of-line, rather
    // than replacing a partial run. Check before touching undo/the buffer.
    let start = buf_cursor_pos(ed.buffer());
    let start_line_chars = buf_line_chars(ed.buffer(), start.row);
    if count == 0 || start.col + count > start_line_chars {
        return;
    }
    ed.push_undo();
    for _ in 0..count {
        let cursor = buf_cursor_pos(ed.buffer());
        let line_chars = buf_line_chars(ed.buffer(), cursor.row);
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
    hjkl_engine::motions::move_left(ed.buffer_mut(), 1);
    ed.push_buffer_cursor_to_textarea();
}
/// Returns `false` when there is no char under the cursor to toggle
/// (end of line / empty line) so counted loops can stop instead of
/// spinning through a saturated count prefix.
pub(crate) fn toggle_case_at_cursor<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(ed.buffer());
    let Some(c) = buf_line(ed.buffer(), cursor.row).and_then(|l| l.chars().nth(cursor.col)) else {
        return false;
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
    true
}
/// Returns `false` when the cursor is on the last line (nothing to
/// join) so counted loops can stop instead of spinning.
pub(crate) fn join_line<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    use hjkl_buffer::{Edit, Position};
    ed.sync_buffer_content_from_textarea();
    let row = buf_cursor_pos(ed.buffer()).row;
    if row + 1 >= buf_row_count(ed.buffer()) {
        return false;
    }
    let cur_line = buf_line(ed.buffer(), row).unwrap_or_default();
    let next_raw = buf_line(ed.buffer(), row + 1).unwrap_or_default();
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
    buf_set_cursor_rc(ed.buffer_mut(), row, cur_chars);
    ed.push_buffer_cursor_to_textarea();
    true
}
/// `gJ` — join the next line onto the current one without inserting a
/// separating space or stripping leading whitespace.
/// Returns `false` when the cursor is on the last line. See [`join_line`].
pub(crate) fn join_line_raw<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) -> bool {
    use hjkl_buffer::Edit;
    ed.sync_buffer_content_from_textarea();
    let row = buf_cursor_pos(ed.buffer()).row;
    if row + 1 >= buf_row_count(ed.buffer()) {
        return false;
    }
    let join_col = buf_line_chars(ed.buffer(), row);
    ed.mutate_edit(Edit::JoinLines {
        row,
        count: 1,
        with_space: false,
    });
    // Vim leaves the cursor at the join point (end of original line).
    buf_set_cursor_rc(ed.buffer_mut(), row, join_col);
    ed.push_buffer_cursor_to_textarea();
    true
}
/// Visual-mode `J` (`with_space = true`) / `gJ` (`with_space = false`) — join
/// every line spanned by the selection into one. A single-line selection joins
/// the current line with the one below (matching normal-mode `J`).
pub(crate) fn visual_join<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    with_space: bool,
) {
    let cursor_row = buf_cursor_pos(ed.buffer()).row;
    let (top, bot) = match vim(ed).mode {
        Mode::VisualLine => (
            cursor_row.min(vim(ed).visual_line_anchor),
            cursor_row.max(vim(ed).visual_line_anchor),
        ),
        Mode::VisualBlock => {
            let a = vim(ed).block_anchor.0;
            (a.min(cursor_row), a.max(cursor_row))
        }
        Mode::Visual => {
            let a = vim(ed).visual_anchor.0;
            (a.min(cursor_row), a.max(cursor_row))
        }
        _ => return,
    };
    // N selected lines → N-1 joins; a single line still does one join (with the
    // line below) like normal-mode `J`.
    let joins = (bot - top).max(1);
    ed.push_undo();
    buf_set_cursor_rc(ed.buffer_mut(), top, 0);
    ed.push_buffer_cursor_to_textarea();
    for _ in 0..joins {
        let joined = if with_space {
            join_line(ed)
        } else {
            join_line_raw(ed)
        };
        if !joined {
            break;
        }
    }
    // B1: visual `J`/`gJ` is exactly `[joins + 1]J`/`gJ` from the top row —
    // reuse the existing `JoinLine` dot-repeat entry (`:h v_J`) rather than
    // adding a bespoke visual variant; replay already starts at whatever
    // row the cursor is on, same as this function does via the
    // `buf_set_cursor_rc` above.
    if !vim(ed).replaying {
        vim_mut(ed).last_change = Some(LastChange::JoinLine { count: joins });
    }
    vim_mut(ed).mode = Mode::Normal;
    ed.set_sticky_col(Some(buf_cursor_pos(ed.buffer()).col));
}
/// `[count]%` — go to the line at `count` percent of the file (vim: line
/// `(count * line_count + 99) / 100`), cursor on the first non-blank.
pub(crate) fn goto_percent<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
) {
    let rows = buf_row_count(ed.buffer());
    if rows == 0 {
        return;
    }
    // Exclude the phantom trailing empty line (a file ending in `\n` is N lines
    // in vim, not N+1) so the percentage matches nvim.
    let total = if rows >= 2
        && buf_line(ed.buffer(), rows - 1)
            .map(|s| s.is_empty())
            .unwrap_or(false)
    {
        rows - 1
    } else {
        rows
    };
    // 1-based target line, clamped to the buffer (vim: ceil(count*lines/100)).
    // Saturating: a pathological count prefix (e.g. 20 typed digits) must not
    // overflow the multiply; the clamp below caps the result at `total` anyway.
    let line = count.saturating_mul(total).div_ceil(100).clamp(1, total);
    let pre = ed.cursor();
    ed.jump_cursor(line - 1, 0);
    move_first_non_whitespace(ed);
    ed.set_sticky_col(Some(ed.cursor().1));
    if ed.cursor() != pre {
        ed.push_jump(pre);
    }
}
/// Indent width of a leading-whitespace prefix, counting a `\t` as advancing
/// to the next `tabstop` boundary and a space as one column.
pub(crate) fn indent_width(s: &str, tabstop: usize) -> usize {
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
pub(crate) fn build_indent(width: usize, settings: &hjkl_engine::Settings) -> String {
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
pub(crate) fn reindent_block(
    text: &str,
    target_width: usize,
    settings: &hjkl_engine::Settings,
) -> String {
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
pub(crate) fn do_paste<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
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
    let selector = vim_mut(ed).pending_register.take();
    // `"+p`/`"*p`: refresh the register slot from the live OS clipboard
    // before reading it below (audit-r2 fix 4) — otherwise this reads
    // whatever the internal slot last had from an in-editor `"+y`.
    sync_clipboard_register_for(ed, selector);
    let (yank, linewise, blockwise, block_width) =
        ed.with_registers(|regs| match selector.and_then(|c| regs.read(c)) {
            Some(slot) => (
                slot.text.clone(),
                slot.linewise,
                slot.blockwise,
                slot.block_width,
            ),
            // Read both fields from the unnamed slot rather than mixing the
            // slot's text with `vim.yank_linewise`. The cached vim flag is
            // per-editor, so a register imported from another editor (e.g.
            // cross-buffer yank/paste) carried the wrong linewise without
            // this — pasting a linewise yank inserted at the char cursor.
            None => (
                regs.unnamed.text.clone(),
                regs.unnamed.linewise,
                regs.unnamed.blockwise,
                regs.unnamed.block_width,
            ),
        });
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
        let r = buf_cursor_pos(ed.buffer()).row;
        let (_, fold_end) = expand_linewise_over_closed_folds(ed.buffer(), r, r);
        Some(fold_end)
    } else {
        None
    };
    // Empty register: nothing to paste on any iteration — bail before the
    // loop instead of `continue`-spinning through a huge count prefix.
    if yank.is_empty() {
        return;
    }
    // Blockwise register (`<C-v>` yank/delete): re-insert the row segments
    // as columns at the cursor. Handles its own cursor / marks / sticky
    // column, so return before the charwise/linewise loop below.
    if blockwise {
        do_block_paste(ed, before, count, block_width, &yank);
        return;
    }
    // Charwise pastes insert the register text repeated `count` times as a
    // single block (see the `i == 0` branch below), so they only need one
    // pass through the loop; linewise pastes still iterate `count` times
    // (each pass opens its own fresh row(s)).
    let iterations = if linewise { count } else { count.min(1) };
    for _ in 0..iterations {
        ed.sync_buffer_content_from_textarea();
        let yank = yank.clone();
        if linewise {
            // Linewise paste: insert payload as fresh row(s) above
            // (`P`) or below (`p`) the cursor's row. Cursor lands on
            // the first non-blank of the first pasted line.
            let mut text = yank.trim_matches('\n').to_string();
            let row = buf_cursor_pos(ed.buffer()).row;
            // `]p` / `[p` — reindent the pasted block to the current line.
            if reindent {
                let cur_line = buf_line(ed.buffer(), row).unwrap_or_default();
                let target_w = indent_width(&cur_line, ed.settings().tabstop.max(1));
                text = reindent_block(&text, target_w, ed.settings());
            }
            // Fold-aware: linewise paste lands relative to the whole CLOSED
            // fold, not just the cursor line — `p` after the fold's last row,
            // `P` before its first row (vim behaviour). No fold → unchanged.
            let (fold_start, fold_end) = expand_linewise_over_closed_folds(ed.buffer(), row, row);
            let target_row = if before {
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(fold_start, 0),
                    text: format!("{text}\n"),
                });
                fold_start
            } else {
                let line_chars = buf_line_chars(ed.buffer(), fold_end);
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(fold_end, line_chars),
                    text: format!("\n{text}"),
                });
                fold_end + 1
            };
            buf_set_cursor_rc(ed.buffer_mut(), target_row, 0);
            hjkl_engine::motions::move_first_non_blank(ed.buffer_mut());
            ed.push_buffer_cursor_to_textarea();
            // Linewise: `[` = (target_row, 0), `]` = (bot_row, last_col).
            let payload_lines = text.lines().count().max(1);
            let bot_row = target_row + payload_lines - 1;
            let bot_last_col = buf_line_chars(ed.buffer(), bot_row).saturating_sub(1);
            paste_mark = Some(((target_row, 0), (bot_row, bot_last_col)));
        } else {
            // Charwise paste. `P` inserts at cursor (shifting cell
            // right); `p` inserts after cursor (advance one cell
            // first, clamped to the end of the line).
            //
            // B20: `[count]p`/`[count]P` insert the register text
            // repeated `count` times as a SINGLE block (vim semantics),
            // not `count` separate paste operations — a per-iteration
            // loop is only correct for `p` (each pass's cursor lands
            // right after the previous insert, so it happens to
            // append); for `P` every pass re-anchors on the *original*
            // cursor, re-inserting at the same point and leaving the
            // cursor on the wrong char once the loop ends. Building the
            // repeated text once and inserting it in one shot gives the
            // right buffer AND the right final cursor for both.
            let cursor = buf_cursor_pos(ed.buffer());
            let at = if before {
                cursor
            } else {
                let line_chars = buf_line_chars(ed.buffer(), cursor.row);
                Position::new(cursor.row, (cursor.col + 1).min(line_chars))
            };
            let repeated = yank.repeat(count);
            ed.mutate_edit(Edit::InsertStr { at, text: repeated });
            // Vim parks the cursor on the last char of the pasted text
            // (do_insert_str leaves it one past the end). `gp` instead
            // leaves the cursor just AFTER the pasted text, so skip the
            // step-back there.
            if !cursor_after && ed.cursor().1 > 0 {
                hjkl_engine::motions::move_left(ed.buffer_mut(), 1);
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
            let last_row = buf_row_count(ed.buffer()).saturating_sub(1);
            let target = (bot_row + 1).min(last_row);
            buf_set_cursor_rc(ed.buffer_mut(), target, 0);
            ed.push_buffer_cursor_to_textarea();
        }
    } else if let Some(orig_row) = original_row_for_linewise_after {
        // Linewise `p` (after) with count: cursor lands on the FIRST pasted
        // line (original_row + 1) — vim parity. The per-iteration loop
        // moves cursor to each paste's target_row, so without this reset
        // `5p` would land at original_row + 5 instead of original_row + 1.
        let first_target = orig_row.saturating_add(1);
        buf_set_cursor_rc(ed.buffer_mut(), first_target, 0);
        hjkl_engine::motions::move_first_non_blank(ed.buffer_mut());
        ed.push_buffer_cursor_to_textarea();
    }
    // Any paste re-anchors the sticky column to the new cursor position.
    ed.set_sticky_col(Some(buf_cursor_pos(ed.buffer()).col));
}
/// Blockwise paste (`p`/`P` with a visual-block register). Re-inserts the
/// register's row segments as COLUMNS at the cursor — vim's true
/// block-paste geometry — rather than spilling each segment onto its own
/// new line. `count` repeats each segment horizontally.
///
/// Geometry (verified against nvim v0.12.4):
/// - `p` inserts starting at the column AFTER the cursor, `P` AT the
///   cursor column; segment row `i` lands on buffer row `cursor.row + i`.
/// - Rows past the buffer end are created; a target row shorter than the
///   insert column is padded with spaces up to it.
/// - Each segment is padded with trailing spaces to the register's block
///   `width` and then repeated `count` times — but ONLY when there is text
///   after the insert column on that row; at end-of-line no trailing
///   padding is added (matches nvim).
/// - The cursor lands on the top-left cell of the pasted block.
///
/// The caller (`do_paste`) has already pushed the undo checkpoint.
fn do_block_paste<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    before: bool,
    count: usize,
    width: usize,
    yank: &str,
) {
    ed.sync_buffer_content_from_textarea();
    let mut lines: Vec<String> =
        hjkl_engine::rope_util::rope_to_lines_vec(&hjkl_engine::types::Query::rope(ed.buffer()));
    // Detach ropey's phantom trailing empty line (present iff the buffer
    // ends with `\n`) so appending block rows past EOF doesn't consume the
    // trailing newline. Re-added after the join below.
    let trailing_nl = lines.len() > 1 && lines.last().is_some_and(String::is_empty);
    if trailing_nl {
        lines.pop();
    }
    let cursor = buf_cursor_pos(ed.buffer());
    let start_row = cursor.row;
    // Insert column (char index). `P` inserts at the cursor; `p` after it.
    // On an empty line `p` has no char to sit after, so it inserts at col 0.
    let cur_len = lines.get(start_row).map(|l| l.chars().count()).unwrap_or(0);
    let insert_col = if before {
        cursor.col
    } else if cur_len == 0 {
        0
    } else {
        cursor.col + 1
    };
    let segments: Vec<&str> = yank.split('\n').collect();
    for (i, seg) in segments.iter().enumerate() {
        let row = start_row + i;
        // Rows past the buffer end are created as empty lines.
        while row >= lines.len() {
            lines.push(String::new());
        }
        let mut chars: Vec<char> = lines[row].chars().collect();
        // Pad the target row up to the insert column with spaces.
        if chars.len() < insert_col {
            chars.resize(insert_col, ' ');
        }
        let head: String = chars[..insert_col].iter().collect();
        let tail: String = chars[insert_col..].iter().collect();
        // Pad the segment to the block width only when it is followed by
        // text on this row — at EOL vim adds no trailing spaces.
        let piece = if tail.is_empty() {
            seg.repeat(count)
        } else {
            let seg_len = seg.chars().count();
            let mut padded = seg.to_string();
            if seg_len < width {
                padded.extend(std::iter::repeat_n(' ', width - seg_len));
            }
            padded.repeat(count)
        };
        lines[row] = format!("{head}{piece}{tail}");
    }
    let mut joined = lines.join("\n");
    if trailing_nl {
        joined.push('\n');
    }
    hjkl_engine::types::BufferEdit::replace_all(ed.buffer_mut(), &joined);
    ed.mark_content_dirty();
    // Cursor lands on the top-left cell of the pasted block.
    ed.jump_cursor(start_row, insert_col);
    // `[` / `]` span the pasted block's top-left .. bottom-left column.
    let bot_row = start_row + segments.len().saturating_sub(1);
    ed.set_mark('[', (start_row, insert_col));
    ed.set_mark(']', (bot_row, insert_col));
    ed.set_sticky_col(Some(insert_col));
}
/// Visual-mode `p` / `P` — replace the active selection with the register.
/// With `p` the deleted selection lands in the unnamed register (vim's swap);
/// with `P` (`before = true`) the source register is preserved so it can be
/// pasted over multiple selections in turn.
pub(crate) fn visual_paste<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    before: bool,
) {
    use hjkl_buffer::{Edit, Position};
    ed.sync_buffer_content_from_textarea();

    // Resolve the source register (selector or unnamed) BEFORE the delete
    // overwrites the unnamed register with the cut selection.
    let selector = vim_mut(ed).pending_register.take();
    // `"+p`/`"*p` in visual mode: same live-clipboard refresh as normal-mode
    // paste (audit-r2 fix 4).
    sync_clipboard_register_for(ed, selector);
    let (reg_text, reg_linewise, reg_blockwise, reg_block_width) =
        ed.with_registers(|regs| match selector.and_then(|c| regs.read(c)) {
            Some(slot) => (
                slot.text.clone(),
                slot.linewise,
                slot.blockwise,
                slot.block_width,
            ),
            None => (
                regs.unnamed.text.clone(),
                regs.unnamed.linewise,
                regs.unnamed.blockwise,
                regs.unnamed.block_width,
            ),
        });
    // For `P`, snapshot the unnamed register so we can restore it afterwards.
    let saved_unnamed = before.then(|| ed.with_registers(|regs| regs.unnamed.clone()));

    let mode = vim(ed).mode;
    ed.push_undo();

    match mode {
        Mode::VisualLine => {
            let cursor_row = buf_cursor_pos(ed.buffer()).row;
            let top = cursor_row.min(vim(ed).visual_line_anchor);
            let bot = cursor_row.max(vim(ed).visual_line_anchor);
            // Delete the selected lines into the unnamed register.
            cut_vim_range(ed, (top, 0), (bot, 0), RangeKind::Linewise);
            // Insert the register as fresh line(s) where the selection was.
            let text = reg_text.trim_matches('\n').to_string();
            let line_count = buf_row_count(ed.buffer());
            if top >= line_count {
                // Selection reached the end of the buffer: append below the
                // (new) last line.
                let last = line_count.saturating_sub(1);
                let lc = buf_line_chars(ed.buffer(), last);
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(last, lc),
                    text: format!("\n{text}"),
                });
                buf_set_cursor_rc(ed.buffer_mut(), last + 1, 0);
            } else {
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(top, 0),
                    text: format!("{text}\n"),
                });
                buf_set_cursor_rc(ed.buffer_mut(), top, 0);
            }
            hjkl_engine::motions::move_first_non_blank(ed.buffer_mut());
            ed.push_buffer_cursor_to_textarea();
        }
        Mode::Visual => {
            let anchor = vim(ed).visual_anchor;
            let cursor = ed.cursor();
            let (top, bot) = order(anchor, cursor);
            // Delete the selection into the unnamed register.
            cut_vim_range(ed, top, bot, RangeKind::Inclusive);
            // Insert the register text where the selection started.
            if reg_linewise {
                // Linewise register into a charwise hole: open a line below.
                let text = reg_text.trim_matches('\n').to_string();
                let lc = buf_line_chars(ed.buffer(), top.0);
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(top.0, lc),
                    text: format!("\n{text}"),
                });
                buf_set_cursor_rc(ed.buffer_mut(), top.0 + 1, 0);
                hjkl_engine::motions::move_first_non_blank(ed.buffer_mut());
            } else {
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(top.0, top.1),
                    text: reg_text.clone(),
                });
                // Park the cursor on the last char of the inserted text.
                let inserted_len = reg_text.chars().count();
                let last_col = top.1 + inserted_len.saturating_sub(1);
                buf_set_cursor_rc(ed.buffer_mut(), top.0, last_col);
            }
            ed.push_buffer_cursor_to_textarea();
        }
        Mode::VisualBlock => {
            // `p`/`P` over a VISUAL-BLOCK selection: delete the rectangle,
            // then put the source register according to its kind. Verified
            // against nvim v0.12.4 — each register type places differently:
            //   - blockwise reg → re-inserted as columns at the block's
            //     top-left, exactly like a normal-mode block paste.
            //   - linewise reg  → opened as fresh line(s) BELOW the block's
            //     bottom row (not inline).
            //   - single-line charwise reg → replicated at the block's LEFT
            //     column on EVERY row of the (now-deleted) block.
            //   - multi-line charwise reg → a plain inline charwise paste at
            //     the block's top-left (cursor parks at the paste start).
            let (top, bot, left, right) = block_bounds(ed);
            let to_eol = vim(ed).block_to_eol;
            // Snapshot the rectangle for the `p` swap register.
            let deleted = block_yank(ed, top, bot, left, right, to_eol);
            let del_width = if to_eol {
                deleted
                    .split('\n')
                    .map(|s| s.chars().count())
                    .max()
                    .unwrap_or(0)
            } else {
                right + 1 - left
            };
            delete_block_contents(ed, top, bot, left, right, to_eol);
            // `p` swaps the deleted block into the unnamed register (blockwise);
            // `P` preserves the source register (restored below via
            // `saved_unnamed`).
            if !before && !deleted.is_empty() {
                ed.record_yank_to_host(deleted.clone());
                ed.record_delete_block(deleted, del_width, None);
            }
            if reg_blockwise {
                ed.jump_cursor(top, left);
                // `before = true` makes `do_block_paste` insert AT `left`
                // (the block's now-vacated left column) rather than after it.
                do_block_paste(ed, true, 1, reg_block_width, &reg_text);
            } else if reg_linewise {
                let text = reg_text.trim_matches('\n').to_string();
                let lc = buf_line_chars(ed.buffer(), bot);
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(bot, lc),
                    text: format!("\n{text}"),
                });
                buf_set_cursor_rc(ed.buffer_mut(), bot + 1, 0);
                hjkl_engine::motions::move_first_non_blank(ed.buffer_mut());
                ed.push_buffer_cursor_to_textarea();
            } else if reg_text.contains('\n') {
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(top, left),
                    text: reg_text.clone(),
                });
                buf_set_cursor_rc(ed.buffer_mut(), top, left);
                ed.push_buffer_cursor_to_textarea();
            } else {
                // Single-line charwise: replicate at the left column on every
                // block row. Rows shorter than `left` are SKIPPED (no
                // padding) — verified against nvim v0.12.4: pasting "d" over
                // a col-3 block whose middle row is only 2 chars leaves that
                // short row untouched.
                for r in top..=bot {
                    let line_len = buf_line_chars(ed.buffer(), r);
                    if left > line_len {
                        continue;
                    }
                    ed.mutate_edit(Edit::InsertStr {
                        at: Position::new(r, left),
                        text: reg_text.clone(),
                    });
                }
                let last_col = left + reg_text.chars().count().saturating_sub(1);
                buf_set_cursor_rc(ed.buffer_mut(), top, last_col);
                ed.push_buffer_cursor_to_textarea();
            }
        }
        _ => {}
    }

    // `P` preserves the source register; restore the snapshot.
    if let Some(slot) = saved_unnamed {
        ed.with_registers_mut(|regs| regs.unnamed = slot);
    }
    vim_mut(ed).mode = Mode::Normal;
    ed.set_sticky_col(Some(buf_cursor_pos(ed.buffer()).col));
}
/// Visual-mode `<C-a>` / `<C-x>` and `g<C-a>` / `g<C-x>`. Adds `delta` to the
/// first number on each selected line. When `sequential` is true the increment
/// grows by `delta` for each successive number found (vim's `g<C-a>`): the
/// first gets `delta`, the second `2*delta`, and so on.
pub(crate) fn adjust_number_visual<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    delta: i64,
    sequential: bool,
) {
    use hjkl_buffer::{Edit, MotionKind, Position};
    ed.sync_buffer_content_from_textarea();
    let mode = vim(ed).mode;
    let cursor = buf_cursor_pos(ed.buffer());

    // Resolve the row range + the per-row start column to scan from.
    let (top, bot, mut scan_col_first, block_left) = match mode {
        Mode::VisualLine => {
            let t = cursor.row.min(vim(ed).visual_line_anchor);
            let b = cursor.row.max(vim(ed).visual_line_anchor);
            (t, b, 0usize, None)
        }
        Mode::Visual => {
            let (a, c) = order(vim(ed).visual_anchor, (cursor.row, cursor.col));
            (a.0, c.0, a.1, None)
        }
        Mode::VisualBlock => {
            let (a, c) = order(vim(ed).block_anchor, (cursor.row, cursor.col));
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
        let chars: Vec<char> = match buf_line(ed.buffer(), row) {
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
    buf_set_cursor_rc(ed.buffer_mut(), top, block_left.unwrap_or(0));
    ed.push_buffer_cursor_to_textarea();
    vim_mut(ed).mode = Mode::Normal;
    ed.set_sticky_col(Some(buf_cursor_pos(ed.buffer()).col));
}
#[cfg(test)]
mod replace_char_tests {
    use hjkl_buffer::{View, rope_line_str};
    use hjkl_engine::{DefaultHost, Editor, Options};

    fn line(ed: &Editor<View, DefaultHost>, row: usize) -> String {
        rope_line_str(&ed.buffer().rope(), row)
    }

    #[test]
    fn replace_char_count_exceeding_line_replaces_nothing() {
        let buf = View::from_str("ab\ncd");
        let mut ed = crate::vim::vim_editor(buf, DefaultHost::new(), Options::default());
        // Cursor at (0,0); `3rx` needs 3 chars but the line has 2 — vim aborts
        // the whole command and replaces nothing (not a partial run).
        super::replace_char(&mut ed, 'x', 3);
        assert_eq!(line(&ed, 0), "ab", "partial replace must not happen");
        assert_eq!(line(&ed, 1), "cd", "must not spill onto the next line");
    }

    #[test]
    fn replace_char_count_fitting_replaces_run() {
        let buf = View::from_str("abc");
        let mut ed = crate::vim::vim_editor(buf, DefaultHost::new(), Options::default());
        super::replace_char(&mut ed, 'x', 2);
        assert_eq!(line(&ed, 0), "xxc");
    }
}
#[cfg(test)]
mod g_ampersand_tests {
    use super::*;
    use hjkl_buffer::{View, rope_line_str};
    use hjkl_engine::{DefaultHost, Editor, Options};

    fn make_editor(content: &str) -> Editor<View, DefaultHost> {
        let buf = View::from_str(content);
        let host = DefaultHost::new();
        crate::vim::vim_editor(buf, host, Options::default())
    }

    fn buf_line(ed: &Editor<View, DefaultHost>, row: usize) -> String {
        let rope = ed.buffer().rope();
        rope_line_str(&rope, row).trim_end_matches('\n').to_string()
    }

    /// `g&` repeats last `:s/foo/bar/` over every line (no /g flag → first
    /// match per line only).
    #[test]
    fn g_ampersand_repeats_last_substitute_on_whole_buffer() {
        let mut ed = make_editor("foo\nfoo bar foo\nbaz");
        // Simulate a prior `:s/foo/bar/` by setting last_substitute directly.
        let cmd = hjkl_engine::substitute::parse_substitute("/foo/bar/").unwrap();
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
        let cmd = hjkl_engine::substitute::parse_substitute("/foo/bar/g").unwrap();
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
