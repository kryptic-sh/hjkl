//! Vim FSM: text object ops.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{Mode, Operator, RangeKind};

use hjkl_engine::rope_util::rope_to_lines_vec;

use super::*;
use crate::vim_state::vim_mut;
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_cursor_pos, buf_line_chars, buf_set_cursor_rc};

/// Resolve the range of `i<quote>` (inner quote) at the current cursor
/// position. `quote` is one of `'"'`, `'\''`, or `` '`' ``. Returns `None`
/// when the cursor's line contains fewer than two occurrences of `quote`.
pub(crate) fn text_object_inner_quote_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    quote: char,
) -> Option<((usize, usize), (usize, usize))> {
    quote_text_object(ed, quote, true)
}
/// Resolve the range of `a<quote>` (around quote) at the current cursor
/// position. Includes surrounding whitespace on one side per vim semantics.
pub(crate) fn text_object_around_quote_bridge<H: hjkl_engine::types::Host>(
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
pub(crate) fn text_object_inner_bracket_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    open: char,
) -> Option<((usize, usize), (usize, usize))> {
    bracket_text_object(ed, open, true, 1).map(|(s, e, _kind)| (s, e))
}
/// Resolve the range of `a<bracket>` (around bracket pair). Includes the
/// bracket characters themselves. `open` must be one of `'('`, `'{'`, `'['`,
/// `'<'`.
pub(crate) fn text_object_around_bracket_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    open: char,
) -> Option<((usize, usize), (usize, usize))> {
    bracket_text_object(ed, open, false, 1).map(|(s, e, _kind)| (s, e))
}
/// Resolve the range of `is` (inner sentence) at the cursor. Excludes
/// trailing whitespace.
pub(crate) fn text_object_inner_sentence_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    sentence_text_object(ed, true, 1)
}
/// Resolve the range of `as` (around sentence) at the cursor. Includes
/// trailing whitespace.
pub(crate) fn text_object_around_sentence_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    sentence_text_object(ed, false, 1)
}
/// Resolve the range of `ip` (inner paragraph) at the cursor. A paragraph
/// is a block of non-blank lines bounded by blank lines or buffer edges.
pub(crate) fn text_object_inner_paragraph_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    paragraph_text_object(ed, true, 1)
}
/// Resolve the range of `ap` (around paragraph) at the cursor. Includes one
/// trailing blank line when present.
pub(crate) fn text_object_around_paragraph_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    paragraph_text_object(ed, false, 1)
}
/// Resolve the range of `it` (inner tag) at the cursor. Matches XML/HTML-style
/// `<tag>...</tag>` pairs; returns the range of inner content between the open
/// and close tags.
pub(crate) fn text_object_inner_tag_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    tag_text_object(ed, true)
}
/// Resolve the range of `at` (around tag) at the cursor. Includes the open
/// and close tag delimiters themselves.
pub(crate) fn text_object_around_tag_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
) -> Option<((usize, usize), (usize, usize))> {
    tag_text_object(ed, false)
}
/// Pure greedy word-wrap of a slice of lines to `width` chars.
/// Returns `(original_slice, wrapped_lines)`.
/// Blank lines are preserved as paragraph separators.
pub(crate) fn greedy_wrap(original: &[String], width: usize) -> Vec<String> {
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
pub(crate) fn reflow_rows<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top: usize,
    bot: usize,
) {
    let width = ed.settings().textwidth.max(1);
    let mut lines: Vec<String> = rope_to_lines_vec(&hjkl_engine::types::Query::rope(ed.buffer()));
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
pub(crate) fn reflow_rows_keep_cursor<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top: usize,
    bot: usize,
) -> (Vec<String>, Vec<String>) {
    let width = ed.settings().textwidth.max(1);
    let mut lines: Vec<String> = rope_to_lines_vec(&hjkl_engine::types::Query::rope(ed.buffer()));
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
pub(crate) fn reflow_keep_cursor(
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
pub(crate) fn apply_case_op_to_selection<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    op: Operator,
    top: (usize, usize),
    bot: (usize, usize),
    kind: RangeKind,
) {
    use hjkl_buffer::Edit;
    ed.push_undo();
    let saved_yank = ed.yank().to_string();
    let saved_yank_linewise = ed.yank_linewise();
    let selection = cut_vim_range(ed, top, bot, kind);
    let transformed = match op {
        Operator::Uppercase => selection.to_uppercase(),
        Operator::Lowercase => selection.to_lowercase(),
        Operator::ToggleCase => toggle_case_str(&selection),
        Operator::Rot13 => rot13_str(&selection),
        _ => unreachable!(),
    };
    if !transformed.is_empty() {
        let cursor = buf_cursor_pos(ed.buffer());
        ed.mutate_edit(Edit::InsertStr {
            at: cursor,
            text: transformed,
        });
    }
    buf_set_cursor_rc(ed.buffer_mut(), top.0, top.1);
    ed.push_buffer_cursor_to_textarea();
    ed.set_yank(saved_yank);
    ed.set_yank_linewise(saved_yank_linewise);
    vim_mut(ed).mode = Mode::Normal;
}
/// Prepend `count * shiftwidth` spaces to each row in `[top, bot]`.
/// Rows that are empty are skipped (vim leaves blank lines alone when
/// indenting). `shiftwidth` is read from `editor.settings()` so
/// `:set shiftwidth=N` takes effect on the next operation.
pub(crate) fn indent_rows<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top: usize,
    bot: usize,
    count: usize,
) {
    ed.sync_buffer_content_from_textarea();
    let width = ed.settings().shiftwidth.saturating_mul(count.max(1));
    // Honour `expandtab` (#263): `>>` under `noexpandtab` must insert hard tabs,
    // not spaces. Render `width` columns as tabs (`width / tabstop`) plus any
    // sub-tab remainder as spaces; under `expandtab` it stays all spaces.
    let pad: String = if ed.settings().expandtab {
        " ".repeat(width)
    } else {
        let tabstop = ed.settings().tabstop.max(1);
        let tabs = width / tabstop;
        let spaces = width % tabstop;
        format!("{}{}", "\t".repeat(tabs), " ".repeat(spaces))
    };
    let mut lines: Vec<String> = rope_to_lines_vec(&hjkl_engine::types::Query::rope(ed.buffer()));
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
pub(crate) fn outdent_rows<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    top: usize,
    bot: usize,
    count: usize,
) {
    ed.sync_buffer_content_from_textarea();
    let width = ed.settings().shiftwidth.saturating_mul(count.max(1));
    let mut lines: Vec<String> = rope_to_lines_vec(&hjkl_engine::types::Query::rope(ed.buffer()));
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
pub(crate) fn bracket_net(line: &str) -> i32 {
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
pub(crate) fn auto_indent_rows<H: hjkl_engine::types::Host>(
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

    let mut lines: Vec<String> = rope_to_lines_vec(&hjkl_engine::types::Query::rope(ed.buffer()));
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
    ed.set_last_indent_range(Some((top, bot)));
}
pub(crate) fn toggle_case_str(s: &str) -> String {
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
pub(crate) fn order(a: (usize, usize), b: (usize, usize)) -> ((usize, usize), (usize, usize)) {
    if a <= b { (a, b) } else { (b, a) }
}
/// Clamp the buffer cursor to normal-mode valid position: col may not
/// exceed `line.chars().count().saturating_sub(1)` (or 0 on an empty
/// line). Vim applies this clamp on every return to Normal mode after an
/// operator or Esc-from-insert.
pub(crate) fn clamp_cursor_to_normal_mode<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
) {
    let (row, col) = ed.cursor();
    let line_chars = buf_line_chars(ed.buffer(), row);
    let max_col = line_chars.saturating_sub(1);
    if col > max_col {
        buf_set_cursor_rc(ed.buffer_mut(), row, max_col);
        ed.push_buffer_cursor_to_textarea();
    }
}
