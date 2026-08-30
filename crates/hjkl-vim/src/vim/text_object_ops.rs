//! Vim FSM: text object ops.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{Mode, Operator, RangeKind};

use super::command::{cut_vim_range_inner, indent_width, read_vim_range};
use super::*;
use crate::vim_state::vim_mut;
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_line_chars, buf_row_count, buf_set_cursor_rc};
use hjkl_engine::rope_util::rope_line_to_str;

/// Resolve the range of `i<quote>` (inner quote) at the current cursor
/// position. `quote` is one of `'"'`, `'\''`, or `` '`' ``. Returns `None`
/// when the cursor's line contains fewer than two occurrences of `quote`.
pub fn text_object_inner_quote_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    quote: char,
) -> Option<((usize, usize), (usize, usize))> {
    quote_text_object(ed, quote, true)
}
/// Resolve the range of `a<quote>` (around quote) at the current cursor
/// position. Includes surrounding whitespace on one side per vim semantics.
pub fn text_object_around_quote_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
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
pub fn text_object_inner_bracket_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    open: char,
) -> Option<((usize, usize), (usize, usize))> {
    bracket_text_object(ed, open, true, 1).map(|(s, e, _kind)| (s, e))
}
/// Resolve the range of `a<bracket>` (around bracket pair). Includes the
/// bracket characters themselves. `open` must be one of `'('`, `'{'`, `'['`,
/// `'<'`.
pub fn text_object_around_bracket_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    open: char,
) -> Option<((usize, usize), (usize, usize))> {
    bracket_text_object(ed, open, false, 1).map(|(s, e, _kind)| (s, e))
}
/// Resolve the range of `is` (inner sentence) at the cursor. Excludes
/// trailing whitespace.
pub fn text_object_inner_sentence_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
) -> Option<((usize, usize), (usize, usize))> {
    sentence_text_object(ed, true, 1)
}
/// Resolve the range of `as` (around sentence) at the cursor. Includes
/// trailing whitespace.
pub fn text_object_around_sentence_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
) -> Option<((usize, usize), (usize, usize))> {
    sentence_text_object(ed, false, 1)
}
/// Resolve the range of `ip` (inner paragraph) at the cursor. A paragraph
/// is a block of non-blank lines bounded by blank lines or buffer edges.
pub fn text_object_inner_paragraph_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
) -> Option<((usize, usize), (usize, usize))> {
    paragraph_text_object(ed, true, 1)
}
/// Resolve the range of `ap` (around paragraph) at the cursor. Includes one
/// trailing blank line when present.
pub fn text_object_around_paragraph_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
) -> Option<((usize, usize), (usize, usize))> {
    paragraph_text_object(ed, false, 1)
}
/// Resolve the range of `it` (inner tag) at the cursor. Matches XML/HTML-style
/// `<tag>...</tag>` pairs; returns the range of inner content between the open
/// and close tags.
pub fn text_object_inner_tag_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
) -> Option<((usize, usize), (usize, usize))> {
    tag_text_object(ed, true)
}
/// Resolve the range of `at` (around tag) at the cursor. Includes the open
/// and close tag delimiters themselves.
pub fn text_object_around_tag_bridge<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
) -> Option<((usize, usize), (usize, usize))> {
    tag_text_object(ed, false)
}
/// Pure greedy word-wrap of a slice of lines to `width` display columns.
/// Returns the wrapped lines.
///
/// Unlike a naive `split_whitespace` reflow, this preserves interior whitespace
/// runs and trailing whitespace the way nvim's `gq`/`gw` does: each word carries
/// its following gap (the whitespace between it and the next word), the gap is
/// kept when both words land on the same line and dropped when the next word
/// starts a new line, and the gap following the last word is kept verbatim.
///
/// Paragraphs are runs of non-blank lines separated by blank lines (which are
/// preserved as empty output lines). Joining a paragraph follows nvim's
/// `do_join` rule: the next line's leading whitespace is dropped, and a single
/// space separates two lines unless the previous line already ends in
/// whitespace. The first line's leading whitespace is the paragraph "leader"
/// and is re-emitted at the start of every wrapped line. `tabstop` measures
/// display columns, so a `\t` inside a gap advances to the next tab stop while
/// the tab character itself is preserved in the output.
pub fn greedy_wrap(original: &[String], width: usize, tabstop: usize) -> Vec<String> {
    let mut wrapped: Vec<String> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    for line in original {
        if line.trim().is_empty() {
            flush_paragraph(&mut paragraph, &mut wrapped, width, tabstop);
            wrapped.push(String::new());
        } else {
            paragraph.push(line.clone());
        }
    }
    flush_paragraph(&mut paragraph, &mut wrapped, width, tabstop);
    wrapped
}

/// Wrap `paragraph` (a run of non-blank lines) and append its wrapped lines to
/// `out`, then clear the paragraph buffer.
fn flush_paragraph(
    paragraph: &mut Vec<String>,
    out: &mut Vec<String>,
    width: usize,
    tabstop: usize,
) {
    if paragraph.is_empty() {
        return;
    }
    out.extend(wrap_paragraph(paragraph, width, tabstop));
    paragraph.clear();
}

/// Wrap a single paragraph (a run of non-blank lines) into lines bounded by
/// `width` display columns.
fn wrap_paragraph(lines: &[String], width: usize, tabstop: usize) -> Vec<String> {
    let (leader, tokens) = paragraph_tokens(lines);
    wrap_tokens(&tokens, &leader, width, tabstop)
}

/// Join a paragraph's lines the way nvim's `do_join` does: the next line's
/// leading whitespace is dropped, and a single space is inserted between two
/// lines unless the previous line already ends in whitespace (space or tab).
fn join_paragraph(lines: &[String]) -> String {
    let mut joined = lines[0].clone();
    for line in &lines[1..] {
        let stripped = line.trim_start_matches([' ', '\t']);
        let ends_ws = joined.chars().last().is_some_and(|c| c == ' ' || c == '\t');
        if !ends_ws {
            joined.push(' ');
        }
        joined.push_str(stripped);
    }
    joined
}

/// Split `s` into its leading whitespace run (spaces and tabs) and the rest.
fn split_leader(s: &str) -> (&str, &str) {
    let mut end = 0usize;
    for (i, c) in s.char_indices() {
        if c == ' ' || c == '\t' {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    s.split_at(end)
}

/// Tokenize a joined paragraph body into `(word, following_gap)` pairs. `body`
/// has its leading whitespace already stripped (it starts with a word), so each
/// word is followed by its gap run — possibly empty for the last word.
fn tokenize_body(body: &str) -> Vec<(String, String)> {
    let mut tokens = Vec::new();
    let mut chars = body.chars().peekable();
    let is_ws = |c: char| c == ' ' || c == '\t';
    while chars.peek().is_some() {
        let mut word = String::new();
        while let Some(&c) = chars.peek() {
            if is_ws(c) {
                break;
            }
            word.push(c);
            chars.next();
        }
        let mut gap = String::new();
        while let Some(&c) = chars.peek() {
            if !is_ws(c) {
                break;
            }
            gap.push(c);
            chars.next();
        }
        tokens.push((word, gap));
    }
    tokens
}

/// `(leader, tokens)` for a paragraph, where `tokens` is the `(word, gap)`
/// pairs covering the joined body (leading whitespace already in `leader`).
fn paragraph_tokens(lines: &[String]) -> (String, Vec<(String, String)>) {
    let joined = join_paragraph(lines);
    let (leader, body) = split_leader(&joined);
    (leader.to_string(), tokenize_body(body))
}

/// Display width (in screen cells) of `s`, expanding tabs to `tabstop`.
fn display_width(s: &str, tabstop: usize) -> usize {
    hjkl_buffer::char_col_to_visual_col(s, s.chars().count(), tabstop)
}

/// Greedy-wrap `tokens` with `leader` re-emitted at the start of every line.
fn wrap_tokens(
    tokens: &[(String, String)],
    leader: &str,
    width: usize,
    tabstop: usize,
) -> Vec<String> {
    wrap_tokens_with_map(tokens, leader, width, tabstop).0
}

/// Greedy-wrap `tokens` (each `(word, following_gap)`) with `leader` re-emitted
/// on every line. Returns the wrapped lines and, indexed by char position in
/// the joined text (`leader` + words + gaps, in order), the output `(row, col)`
/// of each char — `None` for gap chars dropped at a line break. Re-emitted
/// leader chars have no joined-text counterpart and are simply absent from the
/// map (the vector only ever indexes joined chars).
fn wrap_tokens_with_map(
    tokens: &[(String, String)],
    leader: &str,
    width: usize,
    tabstop: usize,
) -> (Vec<String>, Vec<Option<(usize, usize)>>) {
    let leader_width = display_width(leader, tabstop);
    let mut lines: Vec<String> = Vec::new();
    let mut map: Vec<Option<(usize, usize)>> = Vec::new();

    let mut current = String::new();
    let mut first = true;
    // `(joined char index where the pending gap starts, the gap string)`.
    let mut pending_gap: Option<(usize, String)> = None;

    // The first line's leader IS part of the joined text.
    for ch in leader.chars() {
        map.push(Some((0, current.chars().count())));
        current.push(ch);
    }
    let mut current_width = leader_width;

    for (word, gap) in tokens {
        let word_width = display_width(word, tabstop);
        if first {
            for ch in word.chars() {
                map.push(Some((lines.len(), current.chars().count())));
                current.push(ch);
            }
            current_width += word_width;
            first = false;
        } else {
            let (gap_start, gap_str) = pending_gap.take().expect("gap between words");
            let gap_width = display_width(&gap_str, tabstop);
            if current_width + gap_width + word_width > width {
                // Break before this word: drop the pending gap (its map entries
                // stay `None`), flush the line, and start a new line with the
                // re-emitted leader.
                lines.push(std::mem::take(&mut current));
                current.push_str(leader);
                for ch in word.chars() {
                    map.push(Some((lines.len(), current.chars().count())));
                    current.push(ch);
                }
                current_width = leader_width + word_width;
            } else {
                // Same line: keep the gap (back-fill its map entries).
                for (k, ch) in gap_str.chars().enumerate() {
                    map[gap_start + k] = Some((lines.len(), current.chars().count()));
                    current.push(ch);
                }
                for ch in word.chars() {
                    map.push(Some((lines.len(), current.chars().count())));
                    current.push(ch);
                }
                current_width += gap_width + word_width;
            }
        }
        // Record this word's following gap (unresolved until the next word).
        let gap_start = map.len();
        for _ in gap.chars() {
            map.push(None);
        }
        pending_gap = Some((gap_start, gap.clone()));
    }

    // The last word's following gap (trailing whitespace) is always kept.
    if let Some((gap_start, gap_str)) = pending_gap.take() {
        for (k, ch) in gap_str.chars().enumerate() {
            map[gap_start + k] = Some((lines.len(), current.chars().count()));
            current.push(ch);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }

    (lines, map)
}
/// Greedy word-wrap the rows in `[top, bot]` to `settings.textwidth`,
/// preserving interior and trailing whitespace (see [`greedy_wrap`]).
/// Splits on blank-line boundaries so paragraph structure is preserved.
/// Cursor lands at the first non-blank of the last non-blank reflowed line
/// (nvim's `gq` convention).
pub fn reflow_rows<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    top: usize,
    bot: usize,
) {
    let width = ed.settings().textwidth.max(1);
    let tabstop = ed.settings().tabstop.max(1);
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let n = rope.len_lines();
    let bot = bot.min(n.saturating_sub(1));
    if top > bot {
        return;
    }
    let original: Vec<String> = (top..=bot).map(|r| rope_line_to_str(&rope, r)).collect();
    let wrapped = greedy_wrap(&original, width, tabstop);

    // vim leaves the cursor on the last NON-BLANK line of the reflowed range
    // (a trailing blank from `ap` etc. is not counted).
    let last_offset = wrapped
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .unwrap_or(0);
    let last_row = top + last_offset;

    // Splice the reflowed rows back with ONE bounded Replace — the row count
    // changes, so a per-row edit can't express it. The span runs through the
    // newline after `bot`; when `bot` IS the buffer's last row the span
    // instead swallows the newline BEFORE `top` (mirroring
    // `Editor::splice_row_range`), so a trailing newline is never doubled
    // nor dropped. push_undo above means `u` reverses.
    use hjkl_buffer::{Edit, Position};
    let joined = wrapped.join("\n");
    let (start, end, with) = if bot + 1 < n {
        (
            Position::new(top, 0),
            Position::new(bot, buf_line_chars(ed.buffer(), bot)),
            joined,
        )
    } else if top > 0 {
        (
            Position::new(top - 1, buf_line_chars(ed.buffer(), top - 1)),
            Position::new(bot, buf_line_chars(ed.buffer(), bot)),
            format!("\n{joined}"),
        )
    } else {
        (
            Position::new(0, 0),
            Position::new(bot, buf_line_chars(ed.buffer(), bot)),
            joined,
        )
    };
    ed.mutate_edit(Edit::Replace { start, end, with });
    buf_set_cursor_rc(ed.buffer_mut(), last_row, 0);
    move_first_non_whitespace(ed);
    ed.mark_content_dirty();
}
/// Same reflow as `reflow_rows` but also returns the pre-reflow slice, the
/// wrapped lines, and the `(width, tabstop)` used, so the caller can compute a
/// character-preserving cursor position via [`reflow_keep_cursor`].
pub fn reflow_rows_keep_cursor<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    top: usize,
    bot: usize,
) -> (Vec<String>, Vec<String>, usize, usize) {
    let width = ed.settings().textwidth.max(1);
    let tabstop = ed.settings().tabstop.max(1);
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let n = rope.len_lines();
    let bot = bot.min(n.saturating_sub(1));
    if top > bot {
        return (Vec::new(), Vec::new(), width, tabstop);
    }
    let original: Vec<String> = (top..=bot).map(|r| rope_line_to_str(&rope, r)).collect();
    let wrapped = greedy_wrap(&original, width, tabstop);

    // Same single bounded splice as `reflow_rows` — see that fn for the
    // boundary-newline rationale.
    use hjkl_buffer::{Edit, Position};
    let joined = wrapped.join("\n");
    let (start, end, with) = if bot + 1 < n {
        (
            Position::new(top, 0),
            Position::new(bot, buf_line_chars(ed.buffer(), bot)),
            joined,
        )
    } else if top > 0 {
        (
            Position::new(top - 1, buf_line_chars(ed.buffer(), top - 1)),
            Position::new(bot, buf_line_chars(ed.buffer(), bot)),
            format!("\n{joined}"),
        )
    } else {
        (
            Position::new(0, 0),
            Position::new(bot, buf_line_chars(ed.buffer(), bot)),
            joined,
        )
    };
    ed.mutate_edit(Edit::Replace { start, end, with });
    buf_set_cursor_rc(ed.buffer_mut(), top, 0);
    ed.mark_content_dirty();
    (original, wrapped, width, tabstop)
}
/// Compute the new `(row, col)` that preserves the character the cursor
/// was on after `reflow_rows` has been applied to `[top, bot]`.
///
/// `before_lines`/`after_lines` are the pre-reflow snapshot and the wrapped
/// output; `width`/`tabstop` are the same values `greedy_wrap` was called with.
/// The cursor is mapped by character identity through the whitespace-preserving
/// join + wrap: a cursor on a word char lands on that same char in the wrapped
/// output; a cursor on a gap char that was dropped at a line break clamps to the
/// nearest surviving char before it (matching nvim's `gw`). A cursor on a blank
/// line maps to that blank line.
pub fn reflow_keep_cursor(
    top: usize,
    cursor_row: usize,
    cursor_col: usize,
    before_lines: &[String],
    after_lines: &[String],
    width: usize,
    tabstop: usize,
) -> (usize, usize) {
    let relative_row = cursor_row.saturating_sub(top);

    // Walk `before_lines`/`after_lines` in lockstep, paragraph by paragraph, to
    // find the paragraph holding the cursor and replay the same wrap greedy_wrap
    // performed so we can map the cursor's joined-text char index to its output.
    let mut before_i = 0usize;
    let mut after_i = 0usize;
    while before_i < before_lines.len() {
        if before_lines[before_i].trim().is_empty() {
            if before_i == relative_row {
                // Cursor on a blank line: maps to the matching blank output row.
                return (top + after_i, 0);
            }
            before_i += 1;
            after_i += 1;
            continue;
        }
        let para_start = before_i;
        while before_i < before_lines.len() && !before_lines[before_i].trim().is_empty() {
            before_i += 1;
        }
        let para = &before_lines[para_start..before_i];
        let (leader, tokens) = paragraph_tokens(para);
        let (wrapped, map) = wrap_tokens_with_map(&tokens, &leader, width, tabstop);
        if relative_row >= para_start && relative_row < before_i {
            let offset = cursor_joined_offset(para, relative_row - para_start, cursor_col);
            let (r, c) = map
                .get(offset)
                .copied()
                .flatten()
                .or_else(|| nearest_map_pos(&map, offset))
                .unwrap_or((0, 0));
            return (top + after_i + r, c);
        }
        after_i += wrapped.len();
    }

    // Cursor was beyond the reflowed content — clamp to the last char of the
    // last reflowed line.
    let last = after_lines.len().saturating_sub(1);
    let last_len = after_lines.get(last).map_or(0, |l| l.chars().count());
    let col = if last_len == 0 { 0 } else { last_len - 1 };
    (top + last, col)
}

/// Char index of `(row_in_para, col)` within a paragraph's joined text —
/// the same string `join_paragraph` produces (`leader` + words + gaps, where
/// the gap between two lines is the previous line's trailing whitespace if any,
/// else a single space, and the next line's leading whitespace is dropped).
fn cursor_joined_offset(para: &[String], row_in_para: usize, col: usize) -> usize {
    let mut offset = 0usize;
    for (i, line) in para.iter().enumerate() {
        if i == row_in_para {
            let (lead, _) = split_leader(line);
            let lead_len = lead.chars().count();
            if i == 0 {
                offset += col.min(line.chars().count());
            } else {
                offset += col.saturating_sub(lead_len);
            }
            return offset;
        }
        let (_lead, rest) = split_leader(line);
        offset += if i == 0 {
            line.chars().count()
        } else {
            rest.chars().count()
        };
        let ends_ws = line.chars().last().is_some_and(|c| c == ' ' || c == '\t');
        if !ends_ws {
            offset += 1; // single-space gap introduced by the join
        }
    }
    offset
}

/// Nearest surviving joined char to `offset`: the closest preceding mapped
/// position, else the closest following one.
fn nearest_map_pos(map: &[Option<(usize, usize)>], offset: usize) -> Option<(usize, usize)> {
    map[..offset.min(map.len())]
        .iter()
        .rev()
        .find_map(|p| *p)
        .or_else(|| map.iter().skip(offset).find_map(|p| *p))
}
/// Transform the range `[top, bot]` (vim `RangeKind`) in place with
/// the given case operator. Cursor lands on `top` afterward — vim
/// convention for `gU{motion}` / `gu{motion}` / `g~{motion}`.
/// Preserves the textarea yank buffer (vim's case operators don't
/// touch registers).
pub fn apply_case_op_to_selection<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    op: Operator,
    top: (usize, usize),
    bot: (usize, usize),
    kind: RangeKind,
) {
    use hjkl_buffer::{Edit, Position};
    ed.push_undo();
    let saved_yank = ed.yank();
    let saved_yank_linewise = ed.yank_linewise();
    // Read the text with read_vim_range so we get a consistently-formatted
    // string: trailing '\n' for linewise, no terminator for charwise.
    // cut_vim_range's inverse format varies (leading '\n' for last-line
    // deletes), which makes the re-insertion point context-dependent.
    let selection = read_vim_range(ed, top, bot, kind);
    // Snapshot the row count before the cut so we can tell whether the
    // original buffer had a phantom trailing empty row (ropey convention:
    // "foo\nbar\n" → 3 rows, "foo\nbar" → 2 rows).  Needed after the cut
    // to decide whether to strip read_vim_range's synthetic trailing '\n'.
    let n_rows_before = buf_row_count(ed.buffer());
    // Perform the delete WITHOUT recording: vim's case operators touch no
    // registers and no clipboard, so skip the yank/delete registration that
    // `cut_vim_range` normally performs. The inverse edit is what the
    // re-insertion below consumes.
    cut_vim_range_inner(ed, top, bot, kind, false);
    let transformed = match op {
        Operator::Uppercase => selection.to_uppercase(),
        Operator::Lowercase => selection.to_lowercase(),
        Operator::ToggleCase => toggle_case_str(&selection),
        Operator::Rot13 => rot13_str(&selection),
        _ => unreachable!(),
    };
    if !transformed.is_empty() {
        let (ordered_top, ordered_bot) = order(top, bot);
        // After the cut the buffer may be shorter. Compute the insertion
        // point from the current buffer state.
        let n_rows = buf_row_count(ed.buffer());
        let (insert_at, final_text) = if kind == RangeKind::Linewise && ordered_top.0 >= n_rows {
            // The cut removed the last row(s); no surviving rows remain
            // at or below the cut point.  read_vim_range's trailing '\n'
            // must become a leading '\n' so it re-joins with the last
            // surviving line above.
            let last_row = n_rows.saturating_sub(1);
            let last_col = buf_line_chars(ed.buffer(), last_row);
            // Strip trailing '\n' (always present from read_vim_range)
            // and prepend '\n' for append-after-last-line insertion.
            let body = transformed.trim_end_matches('\n');
            (Position::new(last_row, last_col), format!("\n{}", body))
        } else {
            let at = match kind {
                RangeKind::Linewise => Position::new(ordered_top.0, 0),
                _ => Position::new(ordered_top.0, ordered_top.1),
            };
            // When the cut covered the entire buffer (only the phantom
            // row remains, n_rows == 1) and the original buffer had *no*
            // trailing newline (n_rows_before == number of content rows
            // cut, not content_rows + 1 phantom), strip read_vim_range's
            // synthetic trailing '\n' so it doesn't create a spurious
            // blank line.
            let text = if kind == RangeKind::Linewise
                && n_rows == 1
                && ordered_top.0 == 0
                && n_rows_before == ordered_bot.0 - ordered_top.0 + 1
            {
                transformed.trim_end_matches('\n').to_string()
            } else {
                transformed
            };
            (at, text)
        };
        ed.mutate_edit(Edit::InsertStr {
            at: insert_at,
            text: final_text,
        });
    }
    buf_set_cursor_rc(ed.buffer_mut(), top.0, top.1);
    ed.set_yank(saved_yank);
    ed.set_yank_linewise(saved_yank_linewise);
    vim_mut(ed).mode = Mode::Normal;
}
/// Prepend `count * shiftwidth` spaces to each row in `[top, bot]`.
/// Rows that are empty are skipped (vim leaves blank lines alone when
/// indenting). `shiftwidth` is read from `editor.settings()` so
/// `:set shiftwidth=N` takes effect on the next operation.
pub fn indent_rows<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    top: usize,
    bot: usize,
    count: usize,
) {
    ed.sync_buffer_content_from_textarea();
    let width = ed.settings().shiftwidth.saturating_mul(count.max(1));
    let expandtab = ed.settings().expandtab;
    let tabstop = ed.settings().tabstop.max(1);
    use hjkl_buffer::{Edit, Position};
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let bot = bot.min(rope.len_lines().saturating_sub(1));
    for r in top..=bot {
        let line = rope_line_to_str(&rope, r);
        if line.is_empty() {
            continue;
        }
        // Recompute the WHOLE indent at the new width rather than prepending:
        // `>>` on a tab-indented line under `expandtab` must re-emit the indent
        // as spaces, not leave the original tab in place (`\tabc` → 8 spaces,
        // not 4 spaces + `\t`). `indent_fill` renders the leading tab stop
        // correctly under `noexpandtab` too.
        let old = indent_width(&line, tabstop);
        let fill = indent_fill(0, old + width, expandtab, tabstop);
        let ws = line.chars().take_while(|c| matches!(c, ' ' | '\t')).count();
        ed.mutate_edit(Edit::Replace {
            start: Position::new(r, 0),
            end: Position::new(r, ws),
            with: fill,
        });
    }
    // Restore cursor to first non-blank of the top row so the next
    // vertical motion aims sensibly — matches vim's `>>` convention.
    buf_set_cursor_rc(ed.buffer_mut(), top, 0);
    move_first_non_whitespace(ed);
}
/// Render `width` display columns of indent that will START at display
/// column `at_col`, honouring `expandtab` / `tabstop`.
///
/// Under `noexpandtab` this is NOT `width / tabstop` tabs plus a remainder:
/// a tab advances to the next tab STOP, so how many columns it is worth
/// depends on where it starts. Measured against neovim 0.12.4 — inserting 8
/// columns at column 2 with `tabstop=8` gives one tab (to column 8) and two
/// spaces, not one tab and no spaces.
fn indent_fill(at_col: usize, width: usize, expandtab: bool, tabstop: usize) -> String {
    if expandtab {
        return " ".repeat(width);
    }
    let tabstop = tabstop.max(1);
    let target = at_col + width;
    let mut out = String::new();
    let mut col = at_col;
    while col < target {
        let next_stop = col + tabstop - col % tabstop;
        if next_stop <= target {
            out.push('\t');
            col = next_stop;
        } else {
            out.push_str(&" ".repeat(target - col));
            col = target;
        }
    }
    out
}

/// Blockwise `>` — insert `count * shiftwidth` display columns at the
/// block's LEFT column on every row it covers.
///
/// vim does NOT treat a blockwise `>` as a linewise indent (hjkl did, with a
/// comment claiming vim agreed): `<C-v>jl>` with the block starting at column
/// 2 turns `"abcdef"` into `"ab    cdef"`, not `"    abcdef"`. A row too
/// short to reach `left` is skipped, as is an empty one — both verified
/// against neovim 0.12.4.
pub fn indent_block<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    top: usize,
    bot: usize,
    left: usize,
    count: usize,
) {
    ed.sync_buffer_content_from_textarea();
    let width = ed.settings().shiftwidth.saturating_mul(count.max(1));
    let expandtab = ed.settings().expandtab;
    let tabstop = ed.settings().tabstop.max(1);
    use hjkl_buffer::{Edit, Position};
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let bot = bot.min(rope.len_lines().saturating_sub(1));
    for r in top..=bot {
        let line = rope_line_to_str(&rope, r);
        if line.chars().count() < left {
            continue;
        }
        let at_col = hjkl_buffer::geom::char_col_to_visual_col(&line, left, tabstop);
        // `left` doubles as the char index of the insert point: the byte
        // `char_indices().nth(left)` points at IS the `left`-th char.
        let fill = indent_fill(at_col, width, expandtab, tabstop);
        ed.mutate_edit(Edit::InsertStr {
            at: Position::new(r, left),
            text: fill,
        });
    }
    buf_set_cursor_rc(ed.buffer_mut(), top, left);
}

/// Blockwise `<` — remove up to `count * shiftwidth` display columns of
/// whitespace starting AT the block's left column.
///
/// A tab that straddles the boundary is SPLIT: with `tabstop=8` and
/// `shiftwidth=4`, `"ab\tcd"` outdented from column 2 becomes `"ab  cd"` —
/// the tab was worth six columns there, four came off, two remain. A row
/// with no whitespace at that column is left alone, which is what makes
/// `<C-v>iw<` on `"\t(x).[y]"` a no-op. Verified against neovim 0.12.4.
pub fn outdent_block<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    top: usize,
    bot: usize,
    left: usize,
    count: usize,
) {
    ed.sync_buffer_content_from_textarea();
    let width = ed.settings().shiftwidth.saturating_mul(count.max(1));
    let tabstop = ed.settings().tabstop.max(1);
    use hjkl_buffer::{Edit, Position};
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let bot = bot.min(rope.len_lines().saturating_sub(1));
    for r in top..=bot {
        let line = rope_line_to_str(&rope, r);
        if line.chars().count() < left {
            continue;
        }
        let start_col = hjkl_buffer::geom::char_col_to_visual_col(&line, left, tabstop);
        let target = start_col + width;
        let mut col = start_col;
        // Char-index twin of the old byte-walk: the removed span
        // `[left, end)` holds exactly the whitespace chars consumed below.
        let mut end = left;
        let mut leftover = 0usize;
        for ch in line.chars().skip(left) {
            if !matches!(ch, ' ' | '\t') || col >= target {
                break;
            }
            let next_col = if ch == '\t' {
                col + tabstop - col % tabstop
            } else {
                col + 1
            };
            end += 1;
            if next_col > target {
                // The tab straddles the boundary — keep the columns past it.
                leftover = next_col - target;
                break;
            }
            col = next_col;
        }
        if end > left {
            ed.mutate_edit(Edit::Replace {
                start: Position::new(r, left),
                end: Position::new(r, end),
                with: " ".repeat(leftover),
            });
        }
    }
    buf_set_cursor_rc(ed.buffer_mut(), top, left);
}

/// Remove up to `count * shiftwidth` leading spaces (or tabs) from
/// each row in `[top, bot]`. Rows with less leading whitespace have
/// all their indent stripped, not clipped to zero length.
pub fn outdent_rows<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    top: usize,
    bot: usize,
    count: usize,
) {
    ed.sync_buffer_content_from_textarea();
    let width = ed.settings().shiftwidth.saturating_mul(count.max(1));
    let expandtab = ed.settings().expandtab;
    let tabstop = ed.settings().tabstop.max(1);
    use hjkl_buffer::{Edit, Position};
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let bot = bot.min(rope.len_lines().saturating_sub(1));
    for r in top..=bot {
        let line = rope_line_to_str(&rope, r);
        let old = indent_width(&line, tabstop);
        if old == 0 {
            continue;
        }
        // Recompute the remaining indent rather than deleting whole whitespace
        // chars: `<<` on a tab-indented line under `expandtab` must re-emit the
        // remaining indent as spaces (`\t\tabc` → 4 spaces, not `\tabc`).
        let fill = indent_fill(0, old.saturating_sub(width), expandtab, tabstop);
        let ws = line.chars().take_while(|c| matches!(c, ' ' | '\t')).count();
        ed.mutate_edit(Edit::Replace {
            start: Position::new(r, 0),
            end: Position::new(r, ws),
            with: fill,
        });
    }
    buf_set_cursor_rc(ed.buffer_mut(), top, 0);
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
pub fn bracket_net(line: &str) -> i32 {
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
pub fn auto_indent_rows<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
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

    use hjkl_buffer::{Edit, Position};
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let bot = bot.min(rope.len_lines().saturating_sub(1));

    // Accumulate bracket depth from row 0 up to `top - 1` so we start with
    // the correct depth for the first line of the target range.
    let mut depth: i32 = 0;
    for r in 0..top {
        let line = rope_line_to_str(&rope, r);
        depth += bracket_net(&line);
        if depth < 0 {
            depth = 0;
        }
    }

    for r in top..=bot {
        let line = rope_line_to_str(&rope, r);
        let line_chars = line.chars().count();
        let trimmed_owned = line.trim_start().to_owned();
        // Empty / whitespace-only lines stay empty.
        if trimmed_owned.is_empty() {
            // Only whitespace-only lines need an edit — a truly empty line is
            // already the target text (the old whole-buffer restore rewrote it
            // to the same "").
            if !line.is_empty() {
                ed.mutate_edit(Edit::Replace {
                    start: Position::new(r, 0),
                    end: Position::new(r, line_chars),
                    with: String::new(),
                });
            }
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
        // `.max(0)` is load-bearing: `depth` is an i32 and `saturating_sub`
        // saturates towards i32::MIN, not towards zero. An unmatched close
        // bracket at depth 0 therefore produced -1, and the `as usize` cast
        // wrapped it to usize::MAX — `indent_unit.repeat(usize::MAX)` panics
        // with "capacity overflow", killing the editor on a plain `==` over
        // a line starting with `}`.
        let effective_depth = if starts_with_close {
            depth.saturating_sub(1)
        } else if starts_with_dot {
            depth.saturating_add(1)
        } else {
            depth
        }
        .max(0) as usize;

        // Build new line: indent × depth + stripped content.
        let new_line = format!("{}{}", indent_unit.repeat(effective_depth), trimmed_owned);

        // Advance depth by this line's net bracket count (scan trimmed content).
        depth += bracket_net(&trimmed_owned);
        if depth < 0 {
            depth = 0;
        }

        ed.mutate_edit(Edit::Replace {
            start: Position::new(r, 0),
            end: Position::new(r, line_chars),
            with: new_line,
        });
    }

    // Restore cursor to the first non-blank of `top` (vim parity for `==`).
    buf_set_cursor_rc(ed.buffer_mut(), top, 0);
    move_first_non_whitespace(ed);
    // Record the touched row range so the host can display a visual flash.
    ed.set_last_indent_range(Some((top, bot)));
}
pub fn toggle_case_str(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_lowercase() {
                c.to_uppercase().collect::<String>()
            } else if c.is_uppercase() {
                c.to_lowercase().collect::<String>()
            } else {
                c.to_string()
            }
        })
        .collect()
}
pub fn order(a: (usize, usize), b: (usize, usize)) -> ((usize, usize), (usize, usize)) {
    if a <= b { (a, b) } else { (b, a) }
}
/// Clamp the buffer cursor to normal-mode valid position: col may not
/// exceed `line.chars().count().saturating_sub(1)` (or 0 on an empty
/// line). Vim applies this clamp on every return to Normal mode after an
/// operator or Esc-from-insert.
pub fn clamp_cursor_to_normal_mode<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) {
    let (row, col) = ed.cursor();
    let line_chars = buf_line_chars(ed.buffer(), row);
    let max_col = line_chars.saturating_sub(1);
    if col > max_col {
        buf_set_cursor_rc(ed.buffer_mut(), row, max_col);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_case_op_to_selection, greedy_wrap, indent_rows, outdent_rows, reflow_keep_cursor,
        reflow_rows, toggle_case_str,
    };
    use hjkl_buffer::{View, rope_line_str};
    use hjkl_engine::{DefaultHost, Editor, Options};

    #[test]
    fn toggle_case_preserves_multi_character_unicode_mappings() {
        assert_eq!(toggle_case_str("Straße"), "sTRASSE");
        assert_eq!(toggle_case_str("İ"), "i\u{307}");
    }

    #[test]
    fn outdent_rows_consumes_visual_tab_width() {
        let options = Options {
            tabstop: 4,
            shiftwidth: 4,
            expandtab: false,
            ..Options::default()
        };
        let mut ed = Editor::new(View::from_str("\t\tfoo"), DefaultHost::new(), options);

        outdent_rows(&mut ed, 0, 0, 1);

        assert_eq!(rope_line_str(&ed.buffer().rope(), 0), "\tfoo");
    }

    #[test]
    fn indent_rows_expandtab_recomputes_indent_as_spaces() {
        let options = Options {
            tabstop: 4,
            shiftwidth: 4,
            expandtab: true,
            ..Options::default()
        };
        let mut ed = Editor::new(View::from_str("\tabc"), DefaultHost::new(), options);
        indent_rows(&mut ed, 0, 0, 1);
        // `>>` under expandtab re-emits the WHOLE indent as spaces; the
        // original tab must not survive.
        assert_eq!(rope_line_str(&ed.buffer().rope(), 0), "        abc");
    }

    #[test]
    fn outdent_rows_expandtab_recomputes_indent_as_spaces() {
        let options = Options {
            tabstop: 4,
            shiftwidth: 4,
            expandtab: true,
            ..Options::default()
        };
        let mut ed = Editor::new(View::from_str("\t\tabc"), DefaultHost::new(), options);
        outdent_rows(&mut ed, 0, 0, 1);
        assert_eq!(rope_line_str(&ed.buffer().rope(), 0), "    abc");
    }

    // ── reflow splice boundaries ─────────────────────────────────────────────

    /// `gq` over a mid-buffer range replaces only the range; the trailing
    /// newline and the rows after it are byte-identical. The wrap changes the
    /// row count, so the splice must express the new rows through ONE bounded
    /// Replace (a per-row edit cannot).
    #[test]
    fn reflow_rows_mid_buffer_range() {
        let options = Options {
            textwidth: 4,
            ..Options::default()
        };
        let mut ed = Editor::new(
            View::from_str("a b c d e f g h\nTAIL\n"),
            DefaultHost::new(),
            options,
        );
        reflow_rows(&mut ed, 0, 0);
        let rope = ed.buffer().rope();
        assert_eq!(rope_line_str(&rope, 0), "a b");
        assert_eq!(rope_line_str(&rope, 1), "c d");
        assert_eq!(rope_line_str(&rope, 2), "e f");
        assert_eq!(rope_line_str(&rope, 3), "g h");
        assert_eq!(rope_line_str(&rope, 4), "TAIL");
    }

    /// `gq` over the last row with rows above it: the splice swallows the
    /// newline before `top` rather than doubling or dropping a terminator.
    #[test]
    fn reflow_rows_last_row_keeps_preceding_separator() {
        let options = Options {
            textwidth: 4,
            ..Options::default()
        };
        let mut ed = Editor::new(
            View::from_str("KEEP\na b c d e"),
            DefaultHost::new(),
            options,
        );
        reflow_rows(&mut ed, 1, 1);
        let rope = ed.buffer().rope();
        assert_eq!(rope_line_str(&rope, 0), "KEEP");
        assert_eq!(rope_line_str(&rope, 1), "a b");
        assert_eq!(rope_line_str(&rope, 2), "c d");
        assert_eq!(rope_line_str(&rope, 3), "e");
    }

    /// Whole-buffer `gq` (top 0, bot the only row): no preceding separator to
    /// steal.
    #[test]
    fn reflow_rows_whole_buffer() {
        let options = Options {
            textwidth: 4,
            ..Options::default()
        };
        let mut ed = Editor::new(View::from_str("a b c d e"), DefaultHost::new(), options);
        reflow_rows(&mut ed, 0, 0);
        let rope = ed.buffer().rope();
        assert_eq!(rope_line_str(&rope, 0), "a b");
        assert_eq!(rope_line_str(&rope, 1), "c d");
        assert_eq!(rope_line_str(&rope, 2), "e");
    }

    /// A trailing newline survives a whole-buffer reflow (phantom last row).
    #[test]
    fn reflow_rows_preserves_trailing_newline() {
        let options = Options {
            textwidth: 4,
            ..Options::default()
        };
        let mut ed = Editor::new(View::from_str("a b c d e\n"), DefaultHost::new(), options);
        reflow_rows(&mut ed, 0, 0);
        let rope = ed.buffer().rope();
        assert_eq!(rope_line_str(&rope, 0), "a b");
        assert_eq!(rope_line_str(&rope, 1), "c d");
        assert_eq!(rope_line_str(&rope, 2), "e");
        assert_eq!(rope_line_str(&rope, 3), "");
    }

    // ── greedy_wrap whitespace preservation ───────────────────────────────────

    fn mk(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    /// nvim: `gqq` with `textwidth=79` on `"aaa  bbb   ccc"` leaves it alone —
    /// the interior runs are not squeezed.
    #[test]
    fn greedy_wrap_preserves_interior_runs_when_within_width() {
        assert_eq!(
            greedy_wrap(&mk(&["aaa  bbb   ccc"]), 79, 8),
            mk(&["aaa  bbb   ccc"])
        );
    }

    /// nvim: `gqq` with `textwidth=10` keeps the interior gaps and drops only
    /// the gap immediately before a break.
    #[test]
    fn greedy_wrap_keeps_interior_gap_drops_gap_before_break() {
        assert_eq!(
            greedy_wrap(&mk(&["aaa  bbb   ccc  ddd  eee"]), 10, 8),
            mk(&["aaa  bbb", "ccc  ddd", "eee"])
        );
    }

    /// nvim `do_join` semantics: a line break becomes a single space when the
    /// previous line ends in a non-whitespace char, and no space when it already
    /// ends in whitespace; the next line's leading whitespace is dropped.
    #[test]
    fn greedy_wrap_joins_multiline_paragraph() {
        assert_eq!(
            greedy_wrap(&mk(&["aaa  bbb", "ccc  ddd"]), 10, 8),
            mk(&["aaa  bbb", "ccc  ddd"])
        );
        assert_eq!(
            greedy_wrap(&mk(&["aaa  ", "bbb  ccc"]), 40, 8),
            mk(&["aaa  bbb  ccc"])
        );
        assert_eq!(
            greedy_wrap(&mk(&["aaa", "   bbb ccc"]), 40, 8),
            mk(&["aaa bbb ccc"])
        );
    }

    /// The first line's leading whitespace is the paragraph leader, re-emitted
    /// on every wrapped line.
    #[test]
    fn greedy_wrap_reemits_leader_on_continuation_lines() {
        assert_eq!(
            greedy_wrap(&mk(&["   aaa  bbb", "ccc"]), 10, 8),
            mk(&["   aaa", "   bbb ccc"])
        );
    }

    /// Trailing whitespace after the last word survives on the last line.
    #[test]
    fn greedy_wrap_preserves_trailing_whitespace() {
        assert_eq!(
            greedy_wrap(&mk(&["aaa  bbb  ccc   "]), 10, 8),
            mk(&["aaa  bbb", "ccc   "])
        );
    }

    /// A word longer than `width` lands on its own line, overflowing.
    #[test]
    fn greedy_wrap_single_long_word_overflows_unchanged() {
        assert_eq!(
            greedy_wrap(&mk(&["supercalifragilistic"]), 10, 8),
            mk(&["supercalifragilistic"])
        );
        assert_eq!(
            greedy_wrap(&mk(&["aaa supercalifragilistic bbb"]), 10, 8),
            mk(&["aaa", "supercalifragilistic", "bbb"])
        );
    }

    /// Tabs count as `tabstop` for the wrap column but are preserved verbatim.
    #[test]
    fn greedy_wrap_tab_counts_tabstop_and_is_preserved() {
        assert_eq!(
            greedy_wrap(&mk(&["aaa\tbbb ccc ddd"]), 10, 8),
            mk(&["aaa", "bbb ccc", "ddd"])
        );
        assert_eq!(greedy_wrap(&mk(&["aa\tbb cc"]), 40, 8), mk(&["aa\tbb cc"]));
    }

    /// Blank lines are paragraph separators and survive verbatim.
    #[test]
    fn greedy_wrap_preserves_blank_line_separators() {
        assert_eq!(
            greedy_wrap(&mk(&["aaa  bbb", "", "ccc  ddd"]), 10, 8),
            mk(&["aaa  bbb", "", "ccc  ddd"])
        );
    }

    // ── reflow_keep_cursor (gw cursor mapping) ────────────────────────────────

    /// Cursor on a word char maps to the same char in the wrapped output.
    #[test]
    fn reflow_keep_cursor_preserves_word_char() {
        // 0-based col 8 = 't' of "three"; "three" starts the second wrapped line.
        let before = mk(&["one two three four five"]);
        let after = mk(&["one two", "three four", "five"]);
        assert_eq!(reflow_keep_cursor(0, 0, 8, &before, &after, 10, 8), (1, 0));
    }

    /// Cursor on a gap that was dropped at a break clamps to the last surviving
    /// char before it (nvim clamps to the end of the previous line).
    #[test]
    fn reflow_keep_cursor_clamps_dropped_gap_to_previous_line() {
        // 0-based col 9 is a space in the dropped "   " gap before "ccc".
        let before = mk(&["aaa  bbb   ccc  ddd  eee"]);
        let after = mk(&["aaa  bbb", "ccc  ddd", "eee"]);
        assert_eq!(reflow_keep_cursor(0, 0, 9, &before, &after, 10, 8), (0, 7));
    }

    // ── case operators must not touch registers / clipboard ──────────────────

    /// `gUw` uppercases the word but leaves the OS clipboard, `"-` and the
    /// unnamed register untouched — vim's case operators record nothing.
    /// Regression: `cut_vim_range` used to `record_yank_to_host` +
    /// `record_delete` the pre-transform text, clobbering all three.
    #[test]
    fn case_op_preserves_clipboard_small_delete_and_unnamed() {
        use hjkl_engine::{Host as _, Slot};
        use hjkl_vim_types::{Operator, RangeKind};

        let mut ed = crate::vim::vim_editor(
            View::from_str("ab1 cd"),
            DefaultHost::new(),
            Options::default(),
        );
        ed.host_mut().write_clipboard("CLIP-SENTINEL".into());
        ed.with_registers_mut(|regs| {
            regs.small_delete = Slot {
                text: "SMALL-SENTINEL".into(),
                ..Default::default()
            };
            regs.unnamed = Slot {
                text: "PRE-YANK".into(),
                ..Default::default()
            };
        });

        // `gUw` over "ab" at the cursor — the direct call.
        apply_case_op_to_selection(
            &mut ed,
            Operator::Uppercase,
            (0, 0),
            (0, 1),
            RangeKind::Inclusive,
        );

        assert_eq!(rope_line_str(&ed.buffer().rope(), 0), "AB1 cd");
        assert_eq!(ed.host_mut().read_clipboard(), Some("CLIP-SENTINEL".into()));
        ed.with_registers(|regs| {
            assert_eq!(regs.small_delete.text, "SMALL-SENTINEL");
            assert_eq!(regs.unnamed.text, "PRE-YANK");
        });
    }

    /// `gUU` on the first line must not shift the `"1`–`"9` delete ring
    /// (a linewise cut would otherwise push the pre-transform text into
    /// `"1`). Regression for the same `cut_vim_range` recording bug.
    #[test]
    fn linewise_case_op_does_not_shift_delete_ring() {
        use hjkl_engine::{Host as _, Slot};
        use hjkl_vim_types::{Operator, RangeKind};

        let mut ed = crate::vim::vim_editor(
            View::from_str("abc\ndef"),
            DefaultHost::new(),
            Options::default(),
        );
        ed.host_mut().write_clipboard("CLIP-SENTINEL".into());
        ed.with_registers_mut(|regs| {
            regs.delete_ring[0] = Slot {
                text: "RING-SENTINEL".into(),
                ..Default::default()
            };
            regs.unnamed = Slot {
                text: "PRE-YANK".into(),
                ..Default::default()
            };
        });

        // `gUU` on line 1 — the direct call.
        apply_case_op_to_selection(
            &mut ed,
            Operator::Uppercase,
            (0, 0),
            (0, 0),
            RangeKind::Linewise,
        );

        assert_eq!(rope_line_str(&ed.buffer().rope(), 0), "ABC");
        assert_eq!(rope_line_str(&ed.buffer().rope(), 1), "def");
        assert_eq!(ed.host_mut().read_clipboard(), Some("CLIP-SENTINEL".into()));
        ed.with_registers(|regs| {
            assert_eq!(regs.delete_ring[0].text, "RING-SENTINEL");
            assert_eq!(regs.unnamed.text, "PRE-YANK");
        });
    }
}
