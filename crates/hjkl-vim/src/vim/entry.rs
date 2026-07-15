//! Vim FSM: entry.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_engine::search::SearchPrompt;
use hjkl_vim_types::{Operator, RangeKind};

use super::*;
use crate::vim_state::{vim, vim_mut};
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_cursor_pos, buf_set_cursor_rc};
use hjkl_engine::tag::{is_html_filetype, scan_tag_opener};

/// Open the `/` (forward) or `?` (backward) search prompt. Clears any
/// live search highlight until the user commits a query. `last_search`
/// is preserved so an empty `<CR>` can re-run the previous pattern.
pub(crate) fn enter_search<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    forward: bool,
) {
    ed.set_search_prompt_state(Some(SearchPrompt {
        text: String::new(),
        cursor: 0,
        forward,
        operator: None,
    }));
    ed.set_search_history_cursor(None);
    // 0.0.37: clear via the engine search state (the buffer-side
    // bridge from 0.0.35 was removed in this patch — the `BufferView`
    // renderer reads the pattern from `Editor::search_state()`).
    ed.set_search_pattern(None);
}
/// `d/pat` / `c/pat` / `y/pat` (and `?` forms) — open the search prompt in
/// operator-pending mode. On commit the operator runs over the exclusive
/// charwise range from the current cursor to the match.
pub(crate) fn enter_search_op<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    forward: bool,
    op: Operator,
    count: usize,
) {
    let origin = ed.cursor();
    ed.set_search_prompt_state(Some(SearchPrompt {
        text: String::new(),
        cursor: 0,
        forward,
        operator: Some((op, count.max(1), origin)),
    }));
    ed.set_search_history_cursor(None);
    ed.set_search_pattern(None);
}
/// Apply a pending operator-search over the exclusive charwise range from
/// `origin` to the current (post-search) cursor. Used by the search-prompt
/// commit path for `d/` / `c/` / `y/`.
pub(crate) fn apply_op_search_range<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    origin: (usize, usize),
) {
    let target = ed.cursor();
    run_operator_over_range(ed, op, origin, target, RangeKind::Exclusive);
}
/// `g;` / `g,` body. `dir = -1` walks toward older entries (g;),
/// `dir = 1` toward newer (g,). `count` repeats the step. Stops at
/// the ends of the ring; off-ring positions are silently ignored.
pub(crate) fn walk_change_list<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    dir: isize,
    count: usize,
) {
    if ed.change_list().0.is_empty() {
        return;
    }
    let len = ed.change_list().0.len();
    let mut idx: isize = match (ed.change_list_cursor(), dir) {
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
    ed.set_change_list_cursor(Some(idx));
    let (row, col) = ed.change_list().0[idx];
    ed.jump_cursor(row, col);
}
/// `Ctrl-R {reg}` body — insert the named register's contents at the
/// cursor as charwise text. Embedded newlines split lines naturally via
/// `Edit::InsertStr`. Unknown selectors and empty slots are no-ops so
/// stray keystrokes don't mutate the buffer.
pub(crate) fn insert_register_text<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    selector: char,
) {
    use hjkl_buffer::Edit;
    // Special read-only registers: `/` = last search pattern, `.` = last
    // inserted text. Fall back to the register store for everything else.
    let text = match selector {
        '/' => match &ed.last_search_pattern() {
            Some(s) if !s.is_empty() => s.clone(),
            _ => return,
        },
        '.' => match &vim(ed).last_insert_text {
            Some(s) if !s.is_empty() => s.clone(),
            _ => return,
        },
        _ => match ed.registers().read(selector) {
            Some(slot) if !slot.text.is_empty() => slot.text.clone(),
            _ => return,
        },
    };
    ed.sync_buffer_content_from_textarea();
    let cursor = buf_cursor_pos(ed.buffer());
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
    buf_set_cursor_rc(ed.buffer_mut(), row, col);
    ed.push_buffer_cursor_to_textarea();
    ed.mark_content_dirty();
    if let Some(ref mut session) = vim_mut(ed).insert_session {
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
pub(super) fn compute_enter_indent(settings: &hjkl_engine::Settings, prev_line: &str) -> String {
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
