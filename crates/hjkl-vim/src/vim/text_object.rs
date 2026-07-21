//! Vim FSM: text object.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_engine::abbrev::{Abbrev, AbbrevKind, AbbrevTrigger};
use hjkl_vim_types::{RangeKind, TextObject};

use hjkl_engine::rope_util::{rope_line_to_str, rope_to_lines_vec};

use crate::vim_state::vim;
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_cursor_pos, buf_line, buf_set_cursor_rc};

/// Cursor position as `(row, col)`.
pub(crate) type Pos = (usize, usize);
/// Returns `(start, end, kind)` where `end` is *exclusive* (one past the
/// last character to act on). `kind` is `Linewise` for line-oriented text
/// objects like paragraphs and `Exclusive` otherwise.
pub(crate) fn text_object_range<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    obj: TextObject,
    inner: bool,
    count: usize,
) -> Option<(Pos, Pos, RangeKind)> {
    match obj {
        TextObject::Word { big } => {
            word_text_object(ed, inner, big, count).map(|(s, e)| (s, e, RangeKind::Exclusive))
        }
        TextObject::Quote(q) => {
            quote_text_object(ed, q, inner).map(|(s, e)| (s, e, RangeKind::Exclusive))
        }
        TextObject::Bracket(open) => bracket_text_object(ed, open, inner, count),
        TextObject::Paragraph => {
            paragraph_text_object(ed, inner, count).map(|(s, e)| (s, e, RangeKind::Linewise))
        }
        TextObject::XmlTag => tag_text_object(ed, inner).map(|(s, e)| (s, e, RangeKind::Exclusive)),
        TextObject::Sentence => {
            sentence_text_object(ed, inner, count).map(|(s, e)| (s, e, RangeKind::Exclusive))
        }
    }
}
/// `.` / `?` / `!` — vim sentence terminators (`:h sentence`).
fn is_sentence_terminator(c: char) -> bool {
    matches!(c, '.' | '?' | '!')
}

/// Closing characters vim allows between a terminator and the trailing
/// whitespace that completes a sentence boundary (`:h sentence`): "Any
/// number of closing ')', ']', '"' and ''' characters may appear after
/// the '.', '!' or '?' before the spaces, tabs or end of line."
fn is_sentence_closing(c: char) -> bool {
    matches!(c, ')' | ']' | '"' | '\'')
}

/// `(` / `)` — walk to the next sentence boundary in `forward` direction.
/// Returns `(row, col)` of the boundary's first non-whitespace cell, or
/// `None` when already at the buffer's edge in that direction.
///
/// Implements vim's sentence rules (`:h sentence`): a sentence ends at a
/// terminator (`.`/`?`/`!`), optionally followed by closing punctuation
/// (`)`/`]`/`"`/`'`), then whitespace or end-of-line. A blank line is
/// *also* a sentence (and paragraph) boundary, independent of
/// punctuation — moving into or out of a run of blank lines is itself a
/// stop. When `)` finds no next sentence, it lands on the last character
/// of the buffer (a no-op if already there or past it).
pub(crate) fn sentence_boundary<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    forward: bool,
) -> Option<(usize, usize)> {
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let raw_n_lines = rope.len_lines();
    if raw_n_lines == 0 {
        return None;
    }
    let lines: Vec<Vec<char>> = (0..raw_n_lines)
        .map(|r| rope_line_to_str(&rope, r).chars().collect())
        .collect();
    // Skip vim's single phantom trailing empty row — ropey's len_lines()
    // always synthesizes one extra empty final "line" when the buffer
    // text ends in `\n` (see hjkl_engine::motions::move_bottom / the
    // content_row_count clamp it shares with every vertical motion). A
    // genuinely empty *real* last line (e.g. "One.\n\n") is left alone.
    let n_lines = if raw_n_lines > 1 && lines[raw_n_lines - 1].is_empty() {
        raw_n_lines - 1
    } else {
        raw_n_lines
    };
    if n_lines == 0 {
        return None;
    }
    let boundaries = sentence_boundaries(&lines, n_lines);
    let cursor = ed.cursor();
    let cursor = (cursor.0.min(n_lines - 1), cursor.1);

    if forward {
        if let Some(&p) = boundaries.iter().find(|&&p| p > cursor) {
            return Some(p);
        }
        // No next sentence: land on the last character of the buffer,
        // but never move backward past the cursor.
        let end = end_of_buffer_pos(&lines, n_lines);
        (end > cursor).then_some(end)
    } else {
        boundaries.into_iter().rfind(|&p| p < cursor)
    }
}

/// Every valid sentence-boundary landing position within `lines[..n_lines]`,
/// in ascending order (deduplicated). Always includes `(0, 0)`. Shared by
/// both directions of [`sentence_boundary`] so `(` and `)` agree on where
/// sentences start.
fn sentence_boundaries(lines: &[Vec<char>], n_lines: usize) -> Vec<(usize, usize)> {
    let mut out = vec![(0usize, 0usize)];
    for (row, line) in lines.iter().enumerate().take(n_lines) {
        let mut i = 0;
        while i < line.len() {
            if is_sentence_terminator(line[i]) {
                let mut j = i;
                while j + 1 < line.len() && is_sentence_terminator(line[j + 1]) {
                    j += 1;
                }
                let mut k = j;
                while k + 1 < line.len() && is_sentence_closing(line[k + 1]) {
                    k += 1;
                }
                if k + 1 < line.len() {
                    // Terminator (+ closing run) followed by more text on
                    // the same line: only a boundary if that text starts
                    // with whitespace.
                    if line[k + 1].is_whitespace()
                        && let Some(p) = skip_sentence_ws(lines, n_lines, row, k + 1)
                    {
                        out.push(p);
                    }
                    i = k + 1;
                    continue;
                }
                // Terminator run reaches end of line — the boundary (if
                // any) is whatever comes after the line break.
                if let Some(p) = skip_sentence_ws(lines, n_lines, row, line.len()) {
                    out.push(p);
                }
                break;
            }
            i += 1;
        }
        if row + 1 < n_lines {
            let now_blank = lines[row].is_empty();
            let next_blank = lines[row + 1].is_empty();
            if now_blank != next_blank {
                out.push((row + 1, 0));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Starting at `(row, col)` — a known-whitespace cell, or `col ==
/// lines[row].len()` (just past the line, i.e. the line break) — walk
/// forward over whitespace to the next non-whitespace cell. Stops early
/// at a blank-line transition even mid-skip: vim treats that as its own
/// boundary, taking priority over wherever the terminator's trailing
/// whitespace would otherwise land (`"One.\n\nTwo.\n"` stops at the blank
/// line, not at `"Two."`). `None` when the walk runs off the end of the
/// buffer without finding one.
fn skip_sentence_ws(
    lines: &[Vec<char>],
    n_lines: usize,
    mut row: usize,
    mut col: usize,
) -> Option<(usize, usize)> {
    loop {
        if col < lines[row].len() {
            if lines[row][col].is_whitespace() {
                col += 1;
                continue;
            }
            return Some((row, col));
        }
        if row + 1 >= n_lines {
            return None;
        }
        let was_blank = lines[row].is_empty();
        row += 1;
        col = 0;
        let now_blank = lines[row].is_empty();
        if now_blank != was_blank {
            return Some((row, 0));
        }
    }
}

/// The last valid cursor cell in `lines[..n_lines]` — vim's `)` landing
/// spot when there's no next sentence. The last row's last character, or
/// column 0 if that row happens to be empty.
fn end_of_buffer_pos(lines: &[Vec<char>], n_lines: usize) -> (usize, usize) {
    let last = n_lines - 1;
    let col = lines[last].len().saturating_sub(1);
    (last, col)
}
/// `is` / `as` — sentence: text up to and including the next sentence
/// terminator (`.`, `?`, `!`). Vim treats `.`/`?`/`!` followed by
/// whitespace (or end-of-line) as a boundary; runs of consecutive
/// terminators stay attached to the same sentence. `as` extends to
/// include trailing whitespace; `is` does not.
pub(crate) fn sentence_text_object<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    inner: bool,
    count: usize,
) -> Option<((usize, usize), (usize, usize))> {
    let count = count.max(1);
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let n_lines = rope.len_lines();
    if n_lines == 0 {
        return None;
    }
    // Flatten the buffer so a sentence can span lines (vim's behaviour).
    // Newlines count as whitespace for boundary detection.
    let line_lens: Vec<usize> = (0..n_lines)
        .map(|r| rope_line_to_str(&rope, r).chars().count())
        .collect();
    let pos_to_idx = |pos: (usize, usize)| -> usize {
        let idx: usize = line_lens.iter().take(pos.0).map(|&len| len + 1).sum();
        idx + pos.1
    };
    let idx_to_pos = |mut idx: usize| -> (usize, usize) {
        for (r, &len) in line_lens.iter().enumerate() {
            if idx <= len {
                return (r, idx);
            }
            idx -= len + 1;
        }
        let last = n_lines.saturating_sub(1);
        (last, line_lens[last])
    };
    let mut chars: Vec<char> = rope.chars().collect();
    if chars.last() == Some(&'\n') {
        chars.pop();
    }
    if chars.is_empty() {
        return None;
    }

    let cursor_idx = pos_to_idx(ed.cursor()).min(chars.len() - 1);
    let is_terminator = |c: char| matches!(c, '.' | '?' | '!');

    // Walk backward from cursor to find the start of the current
    // sentence. A boundary is: whitespace immediately after a run of
    // terminators (or start-of-buffer).
    let mut start = cursor_idx;
    while start > 0 {
        let prev = chars[start - 1];
        if prev.is_whitespace() {
            // Check if the whitespace follows a terminator — if so,
            // we've crossed a sentence boundary; the sentence begins
            // at the first non-whitespace cell *after* this run.
            let mut k = start - 1;
            while k > 0 && chars[k - 1].is_whitespace() {
                k -= 1;
            }
            if k > 0 && is_terminator(chars[k - 1]) {
                break;
            }
        }
        start -= 1;
    }
    // Skip leading whitespace (vim doesn't include it in the
    // sentence body).
    while start < chars.len() && chars[start].is_whitespace() {
        start += 1;
    }
    if start >= chars.len() {
        return None;
    }

    // Walk forward to the sentence end (last terminator before the
    // next whitespace boundary).
    let mut end = start;
    while end < chars.len() {
        if is_terminator(chars[end]) {
            // Consume any consecutive terminators (e.g. `?!`).
            while end + 1 < chars.len() && is_terminator(chars[end + 1]) {
                end += 1;
            }
            // If followed by whitespace or end-of-buffer, that's the
            // boundary.
            if end + 1 >= chars.len() || chars[end + 1].is_whitespace() {
                break;
            }
        }
        end += 1;
    }
    // `Nis` / `Nas`: extend across `count - 1` further sentences, skipping the
    // whitespace between each and walking to the next sentence's end.
    let mut rem = count - 1;
    while rem > 0 {
        let mut s = end + 1;
        while s < chars.len() && chars[s].is_whitespace() {
            s += 1;
        }
        if s >= chars.len() {
            break;
        }
        let mut e = s;
        while e < chars.len() {
            if is_terminator(chars[e]) {
                while e + 1 < chars.len() && is_terminator(chars[e + 1]) {
                    e += 1;
                }
                if e + 1 >= chars.len() || chars[e + 1].is_whitespace() {
                    break;
                }
            }
            e += 1;
        }
        end = e;
        rem -= 1;
    }
    // Inclusive end → exclusive end_idx.
    let end_idx = (end + 1).min(chars.len());

    let final_end = if inner {
        end_idx
    } else {
        // `as`: include trailing whitespace (but stop before the next
        // newline so we don't gobble a paragraph break — vim keeps
        // sentences within a paragraph for the trailing-ws extension).
        let mut e = end_idx;
        while e < chars.len() && chars[e].is_whitespace() && chars[e] != '\n' {
            e += 1;
        }
        e
    };

    Some((idx_to_pos(start), idx_to_pos(final_end)))
}
/// `it` / `at` — XML tag pair text object. Builds a flat char index of
/// the buffer, walks `<...>` tokens to pair tags via a stack, and
/// returns the innermost pair containing the cursor.
pub(crate) fn tag_text_object<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    inner: bool,
) -> Option<((usize, usize), (usize, usize))> {
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let n_lines = rope.len_lines();
    if n_lines == 0 {
        return None;
    }
    // Flatten char positions so we can compare cursor against tag
    // ranges without per-row arithmetic. `\n` between lines counts as
    // a single char.
    let line_lens: Vec<usize> = (0..n_lines)
        .map(|r| rope_line_to_str(&rope, r).chars().count())
        .collect();
    let pos_to_idx = |pos: (usize, usize)| -> usize {
        let idx: usize = line_lens.iter().take(pos.0).map(|&len| len + 1).sum();
        idx + pos.1
    };
    let idx_to_pos = |mut idx: usize| -> (usize, usize) {
        for (r, &len) in line_lens.iter().enumerate() {
            if idx <= len {
                return (r, idx);
            }
            idx -= len + 1;
        }
        let last = n_lines.saturating_sub(1);
        (last, line_lens[last])
    };
    let mut chars: Vec<char> = rope.chars().collect();
    if chars.last() == Some(&'\n') {
        chars.pop();
    }
    let cursor_idx = pos_to_idx(ed.cursor());

    // Walk `<...>` tokens. Track open tags on a stack; on a matching
    // close pop and consider the pair a candidate when the cursor lies
    // inside its content range. Innermost wins (replace whenever a
    // tighter range turns up). Also track the first complete pair that
    // starts at or after the cursor so we can fall back to a forward
    // scan (targets.vim-style) when the cursor isn't inside any tag.
    let mut stack: Vec<(usize, usize, String)> = Vec::new(); // (open_start, content_start, name)
    let mut innermost: Option<(usize, usize, usize, usize)> = None;
    let mut next_after: Option<(usize, usize, usize, usize)> = None;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '<' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && chars[j] != '>' {
            j += 1;
        }
        if j >= chars.len() {
            break;
        }
        let inside: String = chars[i + 1..j].iter().collect();
        let close_end = j + 1;
        let trimmed = inside.trim();
        if trimmed.starts_with('!') || trimmed.starts_with('?') {
            i = close_end;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('/') {
            let name = rest.split_whitespace().next().unwrap_or("").to_string();
            if !name.is_empty()
                && let Some(stack_idx) = stack.iter().rposition(|(_, _, n)| *n == name)
            {
                let (open_start, content_start, _) = stack[stack_idx].clone();
                stack.truncate(stack_idx);
                let content_end = i;
                let candidate = (open_start, content_start, content_end, close_end);
                // A pair encloses the cursor when the cursor lies anywhere
                // within the whole pair span — including ON the open or close
                // tag itself (vim `it`/`at` operate on the tag under the
                // cursor, not just its content). Innermost (tightest span)
                // wins; closes are seen innermost-first so the first enclosing
                // candidate is already the tightest.
                if cursor_idx >= open_start && cursor_idx < close_end {
                    innermost = match innermost {
                        Some((os, _, _, ce)) if os <= open_start && close_end <= ce => {
                            Some(candidate)
                        }
                        None => Some(candidate),
                        existing => existing,
                    };
                } else if open_start >= cursor_idx && next_after.is_none() {
                    next_after = Some(candidate);
                }
            }
        } else if !trimmed.ends_with('/') {
            let name: String = trimmed
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                stack.push((i, close_end, name));
            }
        }
        i = close_end;
    }

    let (open_start, content_start, content_end, close_end) = innermost.or(next_after)?;
    if inner {
        Some((idx_to_pos(content_start), idx_to_pos(content_end)))
    } else {
        Some((idx_to_pos(open_start), idx_to_pos(close_end)))
    }
}
pub(crate) fn is_wordchar(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
pub(crate) use hjkl_buffer::is_keyword_char;
pub(crate) fn abbrev_kind(lhs: &str, iskeyword: &str) -> AbbrevKind {
    let chars: Vec<char> = lhs.chars().collect();
    if chars.is_empty() {
        return AbbrevKind::NonKw;
    }
    let last = *chars.last().unwrap();
    let last_is_kw = is_keyword_char(last, iskeyword);
    if !last_is_kw {
        return AbbrevKind::NonKw;
    }
    // last is keyword — check if all chars are keyword
    let all_kw = chars.iter().all(|&c| is_keyword_char(c, iskeyword));
    if all_kw {
        AbbrevKind::Full
    } else {
        AbbrevKind::End
    }
}
/// Try to match and expand an abbreviation given the text before the cursor.
///
/// # Parameters
/// - `abbrevs` — the active abbreviation table (insert-mode entries).
/// - `line_before` — the text on the current line *before* the cursor (char slice).
/// - `mincol` — first column index (0-based, char-indexed) that belongs to the
///   current insert session on the **same row as the cursor**.  Chars before
///   `mincol` were in the buffer before insert mode started and must NOT be
///   consumed as part of the lhs.  When the cursor is on a different row than
///   `start_row`, `mincol` is treated as 0 (the entire line was typed in this
///   session).
/// - `trigger` — what the user did (typed a non-kw char, pressed CR/Esc/C-]).
/// - `iskeyword` — the active iskeyword spec string.
///
/// Returns `Some((lhs_char_len, rhs))` on a match, where `lhs_char_len` is the
/// number of characters to delete before the cursor (the lhs), and `rhs` is the
/// text to insert in their place.  Returns `None` when no abbreviation matches.
pub(crate) fn try_abbrev_expand(
    abbrevs: &[Abbrev],
    line_before: &str,
    mincol: usize,
    trigger: AbbrevTrigger,
    iskeyword: &str,
) -> Option<(usize, String)> {
    let chars: Vec<char> = line_before.chars().collect();
    let cursor_col = chars.len(); // col of the cursor (0-based)

    for abbrev in abbrevs {
        if !abbrev.insert {
            continue;
        }
        let lhs_chars: Vec<char> = abbrev.lhs.chars().collect();
        if lhs_chars.is_empty() {
            continue;
        }
        let lhs_len = lhs_chars.len();

        // Determine the lhs type.
        let kind = abbrev_kind(&abbrev.lhs, iskeyword);

        // Trigger rules by lhs type.
        match kind {
            AbbrevKind::Full | AbbrevKind::End => {
                // full-id / end-id: trigger char must be a NON-keyword char
                // (space, punctuation, CR, Esc, C-]).
                let trigger_char_is_kw = match trigger {
                    AbbrevTrigger::NonKeyword(c) => is_keyword_char(c, iskeyword),
                    AbbrevTrigger::CtrlBracket | AbbrevTrigger::Cr | AbbrevTrigger::Esc => false,
                };
                if trigger_char_is_kw {
                    // A keyword trigger char would extend the word — no expand.
                    continue;
                }
            }
            AbbrevKind::NonKw => {
                // non-id: only expand on CR, Esc, C-].  NOT on regular typed chars.
                match trigger {
                    AbbrevTrigger::Cr | AbbrevTrigger::Esc | AbbrevTrigger::CtrlBracket => {}
                    AbbrevTrigger::NonKeyword(_) => continue,
                }
            }
        }

        // Check that the text before the cursor ends with the lhs.
        if cursor_col < lhs_len {
            continue;
        }
        let lhs_start_col = cursor_col - lhs_len;

        // Enforce mincol: the lhs must not start before the insert-start column.
        if lhs_start_col < mincol {
            continue;
        }

        // Compare chars.
        let text_slice: &[char] = &chars[lhs_start_col..cursor_col];
        if text_slice != lhs_chars.as_slice() {
            continue;
        }

        // Check "front" rule: the char immediately before the lhs.
        if lhs_start_col > 0 {
            let ch_before = chars[lhs_start_col - 1];
            match kind {
                AbbrevKind::Full => {
                    // full-id: char before lhs must be a non-keyword char.
                    // Single-char full-id exception: if the char before is a
                    // non-keyword char that is NOT space/tab, it is NOT recognised
                    // (vim `:h abbreviations`: "A word in front of a full-id abbrev
                    // is a non-keyword char; but a single char abbrev is not
                    // recognised after a non-blank, non-keyword char").
                    // Actually vim's rule: full-id is not recognised if the char
                    // before is a NON-keyword char other than space/tab AND the lhs
                    // is a single keyword char. For multi-char full-id the rule is
                    // just "char before must be non-keyword".
                    if is_keyword_char(ch_before, iskeyword) {
                        continue; // char before is keyword → lhs is part of a longer word
                    }
                    if lhs_len == 1 && ch_before != ' ' && ch_before != '\t' {
                        // single-char full-id: non-blank non-keyword before → skip
                        continue;
                    }
                }
                AbbrevKind::End => {
                    // end-id: no constraint on the char before (any char is fine,
                    // including keyword chars — the non-keyword prefix of the lhs
                    // acts as the boundary).
                }
                AbbrevKind::NonKw => {
                    // non-id: the char before the lhs must be blank (space/tab) or
                    // it must be the start of the typed portion (mincol boundary).
                    if ch_before != ' ' && ch_before != '\t' {
                        continue;
                    }
                }
            }
        }
        // lhs_start_col == 0 means the lhs starts at the very beginning of the
        // line (or at the insert-start position); all types accept this.

        return Some((lhs_len, abbrev.rhs.clone()));
    }

    None
}
/// Check abbreviations and apply the expansion if a match is found.
///
/// Reads the current cursor position and the text before it, calls
/// `try_abbrev_expand`, and if a match is found, deletes the `lhs` chars
/// and inserts the `rhs`. Returns `true` if an expansion was applied.
///
/// `trigger` is what the user did; the trigger char itself is NOT inserted
/// here — the caller inserts it (or not, in the case of `C-]`).
pub(crate) fn check_and_apply_abbrev<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    trigger: AbbrevTrigger,
) -> bool {
    use hjkl_buffer::{Edit, Position};

    // Collect the data we need without holding borrows.
    let cursor = buf_cursor_pos(ed.buffer());
    let row = cursor.row;
    let col = cursor.col;
    let line_before: String = {
        let line = buf_line(ed.buffer(), row).unwrap_or_default();
        line.chars().take(col).collect()
    };
    let (mincol, on_start_row) = if let Some(ref s) = vim(ed).insert_session {
        if row == s.start_row {
            (s.start_col, true)
        } else {
            (0, false)
        }
    } else {
        (0, false)
    };
    // If cursor is before the insert start column on the same row, no lhs possible.
    if on_start_row && col <= mincol {
        return false;
    }

    let iskeyword = ed.settings().iskeyword.clone();
    let abbrevs = ed.abbrevs();

    let Some((lhs_len, rhs)) =
        try_abbrev_expand(&abbrevs, &line_before, mincol, trigger, &iskeyword)
    else {
        return false;
    };

    // Delete `lhs_len` chars before the cursor.
    let lhs_start = col.saturating_sub(lhs_len);
    if lhs_len > 0 {
        ed.mutate_edit(Edit::DeleteRange {
            start: Position::new(row, lhs_start),
            end: Position::new(row, col),
            kind: hjkl_buffer::MotionKind::Char,
        });
    }

    // Insert rhs at the (now updated) cursor position.
    let insert_pos = Position::new(row, lhs_start);
    if !rhs.is_empty() {
        ed.mutate_edit(Edit::InsertStr {
            at: insert_pos,
            text: rhs.clone(),
        });
    }

    // Move cursor to end of inserted rhs.
    let new_col = lhs_start + rhs.chars().count();
    buf_set_cursor_rc(ed.buffer_mut(), row, new_col);
    ed.push_buffer_cursor_to_textarea();

    true
}
pub(crate) fn word_text_object<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    inner: bool,
    big: bool,
    count: usize,
) -> Option<((usize, usize), (usize, usize))> {
    let count = count.max(1);
    let (row, col) = ed.cursor();
    let line = buf_line(ed.buffer(), row)?;
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let len = chars.len();
    let at = col.min(len.saturating_sub(1));
    let classify = |c: char| -> u8 {
        if c.is_whitespace() {
            0
        } else if big || is_wordchar(c) {
            1
        } else {
            2
        }
    };
    let cls = classify(chars[at]);
    let mut start = at;
    while start > 0 && classify(chars[start - 1]) == cls {
        start -= 1;
    }
    let mut end = at;
    while end + 1 < len && classify(chars[end + 1]) == cls {
        end += 1;
    }
    // Columns are char indices — the convention used by the operator
    // pipeline (`cut_vim_range` / `read_vim_range`) and the visual-mode
    // extend path. Pre-0.33.5 this converted to BYTE offsets, which those
    // consumers re-interpreted as char columns — `diw` / `viw` acted on
    // the wrong span whenever the line held multibyte text.
    let mut start_col = start;
    // Exclusive end: char index AFTER the last-included char. Assigned in each
    // branch below (inner / aw-on-whitespace / aw-on-word).
    let end_col;
    if inner {
        // `Niw` selects N alternating runs (word / punct / whitespace), so
        // extend the end over `count - 1` further runs.
        let mut rem = count - 1;
        while rem > 0 && end + 1 < len {
            let next_kind = classify(chars[end + 1]);
            end += 1;
            while end + 1 < len && classify(chars[end + 1]) == next_kind {
                end += 1;
            }
            rem -= 1;
        }
        end_col = end + 1;
    } else if cls == 0 {
        // `aw` with the cursor on whitespace: vim selects the whitespace run
        // plus the FOLLOWING word (`:help aw`). `start..end` already covers the
        // whitespace run; consume `count` following non-blank runs, including
        // any whitespace between them.
        let mut e = end;
        let mut rem = count;
        while rem > 0 && e + 1 < len {
            // Skip whitespace to the next word (no-op right after the initial
            // run, relevant only for count > 1).
            while e + 1 < len && chars[e + 1].is_whitespace() {
                e += 1;
            }
            if e + 1 >= len {
                break;
            }
            // Consume the word (non-blank run).
            e += 1;
            let k = classify(chars[e]);
            while e + 1 < len && classify(chars[e + 1]) == k {
                e += 1;
            }
            rem -= 1;
        }
        end_col = e + 1;
    } else {
        // `Naw` with the cursor on a word — include N non-blank runs plus the
        // whitespace between them, then the trailing whitespace after the last
        // run; if the last run has no trailing whitespace, absorb the leading
        // whitespace before the first instead (vim `:help aw`).
        let mut e = end;
        let mut words_done = 1;
        let mut included_trailing = false;
        loop {
            let mut t = e + 1;
            let mut got_ws = false;
            while t < len && chars[t].is_whitespace() {
                got_ws = true;
                t += 1;
            }
            if words_done == count {
                if got_ws {
                    e = t - 1;
                    included_trailing = true;
                }
                break;
            }
            if t >= len {
                break; // no further word to include
            }
            // Advance onto the next non-blank run and consume it.
            e = t;
            let k = classify(chars[e]);
            while e + 1 < len && classify(chars[e + 1]) == k {
                e += 1;
            }
            words_done += 1;
        }
        end_col = e + 1;
        if !included_trailing {
            let mut s = start;
            while s > 0 && chars[s - 1].is_whitespace() {
                s -= 1;
            }
            start_col = s;
        }
    }
    Some(((row, start_col), (row, end_col)))
}
pub(crate) fn quote_text_object<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    q: char,
    inner: bool,
) -> Option<((usize, usize), (usize, usize))> {
    let (row, col) = ed.cursor();
    let line = buf_line(ed.buffer(), row)?;
    // All columns here are CHAR indices — both the cursor `col` and the
    // returned range. Pre-0.33.5 this scanned BYTE offsets and compared
    // them against the char-indexed cursor, so `di"` / `ci"` picked the
    // wrong pair (and cut the wrong span) on lines with multibyte text.
    let chars: Vec<char> = line.chars().collect();
    // Find opening and closing quote on the same line.
    let mut positions: Vec<usize> = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == q {
            positions.push(i);
        }
    }
    if positions.len() < 2 {
        return None;
    }
    let mut open_idx: Option<usize> = None;
    let mut close_idx: Option<usize> = None;
    for pair in positions.chunks(2) {
        if pair.len() < 2 {
            break;
        }
        if col >= pair[0] && col <= pair[1] {
            open_idx = Some(pair[0]);
            close_idx = Some(pair[1]);
            break;
        }
        if col < pair[0] {
            open_idx = Some(pair[0]);
            close_idx = Some(pair[1]);
            break;
        }
    }
    let open = open_idx?;
    let close = close_idx?;
    // End columns are *exclusive* — one past the last character to act on.
    if inner {
        if close <= open + 1 {
            return None;
        }
        Some(((row, open + 1), (row, close)))
    } else {
        // `da<q>` — "around" includes the surrounding whitespace on one
        // side: trailing whitespace if any exists after the closing quote;
        // otherwise leading whitespace before the opening quote. This
        // matches vim's `:help text-objects` behaviour and avoids leaving
        // a double-space when the quoted span sits mid-sentence.
        let after_close = close + 1; // char index after closing quote
        if after_close < chars.len() && chars[after_close].is_ascii_whitespace() {
            // Eat trailing whitespace run.
            let mut end = after_close;
            while end < chars.len() && chars[end].is_ascii_whitespace() {
                end += 1;
            }
            Some(((row, open), (row, end)))
        } else if open > 0 && chars[open - 1].is_ascii_whitespace() {
            // Eat leading whitespace run.
            let mut start = open;
            while start > 0 && chars[start - 1].is_ascii_whitespace() {
                start -= 1;
            }
            Some(((row, start), (row, close + 1)))
        } else {
            Some(((row, open), (row, close + 1)))
        }
    }
}
pub(crate) fn bracket_text_object<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    open: char,
    inner: bool,
    count: usize,
) -> Option<(Pos, Pos, RangeKind)> {
    let close = match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        _ => return None,
    };
    let (row, col) = ed.cursor();
    let lines = rope_to_lines_vec(&hjkl_engine::types::Query::rope(ed.buffer()));
    let lines = lines.as_slice();
    // If the cursor sits ON the closing bracket, vim anchors the pair to that
    // bracket: the close is at the cursor and the open is found by scanning
    // backward from just before it. Without this, `find_open_bracket` counts
    // the cursor's own close, increments depth, and skips past its matching
    // open — making `di}`/`di{`-on-`}` a silent no-op.
    let cursor_char = lines.get(row).and_then(|l| l.chars().nth(col));
    let (open_pos, close_pos) = if cursor_char == Some(close) {
        let open_pos = if col > 0 {
            find_open_bracket(lines, row, col - 1, open, close)
        } else if row > 0 {
            let pr = row - 1;
            let pc = lines[pr].chars().count().saturating_sub(1);
            find_open_bracket(lines, pr, pc, open, close)
        } else {
            None
        }?;
        (open_pos, (row, col))
    } else {
        // Walk backward from cursor to find unbalanced opening. When the
        // cursor isn't inside any pair, fall back to scanning forward for
        // the next opening bracket (targets.vim-style: `ci(` works when
        // cursor is before the `(` on the same line or below).
        let open_pos = find_open_bracket(lines, row, col, open, close)
            .or_else(|| find_next_open(lines, row, col, open))?;
        let close_pos = find_close_bracket(lines, open_pos.0, open_pos.1 + 1, open, close)?;
        (open_pos, close_pos)
    };
    // Count: `2i{` / `2a{` target the Nth enclosing pair. Expand outward from
    // the innermost pair, re-anchoring to each enclosing bracket in turn. Stop
    // early (and use the outermost found) if there aren't `count` levels.
    let (open_pos, close_pos) = {
        let (mut op, mut cp) = (open_pos, close_pos);
        for _ in 1..count.max(1) {
            let outer = if op.1 > 0 {
                find_open_bracket(lines, op.0, op.1 - 1, open, close)
            } else if op.0 > 0 {
                let pr = op.0 - 1;
                let pc = lines[pr].chars().count().saturating_sub(1);
                find_open_bracket(lines, pr, pc, open, close)
            } else {
                None
            };
            let Some(oo) = outer else { break };
            let Some(oc) = find_close_bracket(lines, oo.0, oo.1 + 1, open, close) else {
                break;
            };
            op = oo;
            cp = oc;
        }
        (op, cp)
    };
    // End positions are *exclusive*.
    if inner {
        // The inner region is the raw charwise span from just after `{` to just
        // before `}`. Returned as Exclusive: the VISUAL path uses it directly
        // (so `vi{` is charwise — `vi{d` → "{}"), while the OPERATOR path
        // (`di{`/`ci{`) applies vim's exclusive-motion adjustment in
        // `apply_op_with_text_object` to collapse a contentful multi-line block
        // to bare braces ("{\n}") or promote a clean one to linewise.
        // Inner start = position just after `{`. When `{` is the last char on
        // its line, the inner region begins at the start of the next line (so
        // the exclusive-motion adjustment can promote to linewise). `advance_pos`
        // stops at end-of-line, so wrap explicitly here.
        let open_line_len = lines[open_pos.0].chars().count();
        let inner_start = if open_pos.1 + 1 >= open_line_len && open_pos.0 + 1 < lines.len() {
            (open_pos.0 + 1, 0)
        } else {
            advance_pos(lines, open_pos)
        };
        // Empty inner (`{}` / `( )` degenerate) → empty range at the inner
        // start. `di{` then no-ops; `ci{` inserts at that point.
        if inner_start.0 > close_pos.0
            || (inner_start.0 == close_pos.0 && inner_start.1 >= close_pos.1)
        {
            return Some((inner_start, inner_start, RangeKind::Exclusive));
        }
        // Whitespace-only multi-line inner: vim's `di{` is a no-op and `ci{`
        // inserts at the inner start without deleting the whitespace. Model as
        // an empty range at the inner start. Detected when every char strictly
        // between the braces (excluding newlines) is a space/tab, and there is
        // at least one — an inner of only newlines (empty lines) does NOT count
        // and falls through to the normal collapse.
        if close_pos.0 > open_pos.0 {
            let mut saw_ws = false;
            let mut saw_other = false;
            for r in inner_start.0..=close_pos.0 {
                let line: Vec<char> = lines
                    .get(r)
                    .map(|l| l.chars().collect())
                    .unwrap_or_default();
                let from = if r == inner_start.0 { inner_start.1 } else { 0 };
                let to = if r == close_pos.0 {
                    close_pos.1
                } else {
                    line.len()
                };
                for &c in line
                    .iter()
                    .take(to.min(line.len()))
                    .skip(from.min(line.len()))
                {
                    if c == ' ' || c == '\t' {
                        saw_ws = true;
                    } else {
                        saw_other = true;
                    }
                }
            }
            if saw_ws && !saw_other {
                return Some((inner_start, inner_start, RangeKind::Exclusive));
            }
        }
        Some((inner_start, close_pos, RangeKind::Exclusive))
    } else {
        Some((
            open_pos,
            advance_pos(lines, close_pos),
            RangeKind::Exclusive,
        ))
    }
}
pub(crate) fn find_open_bracket(
    lines: &[String],
    row: usize,
    col: usize,
    open: char,
    close: char,
) -> Option<(usize, usize)> {
    let mut depth: i32 = 0;
    let mut r = row;
    let mut c = col as isize;
    loop {
        let cur = &lines[r];
        let chars: Vec<char> = cur.chars().collect();
        // Clamp `c` to the line length: callers may seed `col` past
        // EOL on virtual-cursor lines (e.g., insert mode after `o`)
        // so direct indexing would panic on empty / short lines.
        if (c as usize) >= chars.len() {
            c = chars.len() as isize - 1;
        }
        while c >= 0 {
            let ch = chars[c as usize];
            if ch == close {
                depth += 1;
            } else if ch == open {
                if depth == 0 {
                    return Some((r, c as usize));
                }
                depth -= 1;
            }
            c -= 1;
        }
        if r == 0 {
            return None;
        }
        r -= 1;
        c = lines[r].chars().count() as isize - 1;
    }
}
pub(crate) fn find_close_bracket(
    lines: &[String],
    row: usize,
    start_col: usize,
    open: char,
    close: char,
) -> Option<(usize, usize)> {
    let mut depth: i32 = 0;
    let mut r = row;
    let mut c = start_col;
    loop {
        let cur = &lines[r];
        let chars: Vec<char> = cur.chars().collect();
        while c < chars.len() {
            let ch = chars[c];
            if ch == open {
                depth += 1;
            } else if ch == close {
                if depth == 0 {
                    return Some((r, c));
                }
                depth -= 1;
            }
            c += 1;
        }
        if r + 1 >= lines.len() {
            return None;
        }
        r += 1;
        c = 0;
    }
}
/// Forward scan from `(row, col)` for the next occurrence of `open`.
/// Multi-line. Used by bracket text objects to support targets.vim-style
/// "search forward when not currently inside a pair" behaviour.
pub(crate) fn find_next_open(
    lines: &[String],
    row: usize,
    col: usize,
    open: char,
) -> Option<(usize, usize)> {
    let mut r = row;
    let mut c = col;
    while r < lines.len() {
        let chars: Vec<char> = lines[r].chars().collect();
        while c < chars.len() {
            if chars[c] == open {
                return Some((r, c));
            }
            c += 1;
        }
        r += 1;
        c = 0;
    }
    None
}
pub(crate) fn advance_pos(lines: &[String], pos: (usize, usize)) -> (usize, usize) {
    let (r, c) = pos;
    let line_len = lines[r].chars().count();
    if c < line_len {
        (r, c + 1)
    } else if r + 1 < lines.len() {
        (r + 1, 0)
    } else {
        pos
    }
}
pub(crate) fn paragraph_text_object<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    inner: bool,
    count: usize,
) -> Option<((usize, usize), (usize, usize))> {
    let count = count.max(1);
    let (row, _) = ed.cursor();
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let raw_n_lines = rope.len_lines();
    if raw_n_lines == 0 {
        return None;
    }
    // Skip vim's single phantom trailing empty row — ropey's len_lines()
    // always synthesizes one extra empty final "line" when the buffer text
    // ends in `\n` (see hjkl_engine::motions::move_bottom / the
    // content_row_count clamp it shares with every vertical motion, and
    // this file's own `sentence_boundary`, which applies the identical
    // clamp). Without this, a blank-line run reaching real EOF (e.g. `dip`
    // on the trailing blank run of `"a\n\n\n\n"`) extends `bot` one row past
    // the buffer's real last line into the phantom row, which makes
    // `do_delete_range`'s "hi is the last row" branch eat the newline that
    // terminates the preceding *real* content line too — dropping a `\n`
    // that real vim keeps. A genuinely empty *real* last line (e.g.
    // `"One.\n\n"`) is left alone; only a single trailing phantom row is
    // ever skipped.
    let n_lines = if raw_n_lines > 1 && rope_line_to_str(&rope, raw_n_lines - 1).is_empty() {
        raw_n_lines - 1
    } else {
        raw_n_lines
    };
    if n_lines == 0 {
        return None;
    }
    // A paragraph is a run of non-blank lines.
    let is_blank = |r: usize| -> bool {
        if r >= n_lines {
            return true;
        }
        rope_line_to_str(&rope, r).trim().is_empty()
    };
    let mut top = row;
    let mut bot = row;
    if is_blank(row) {
        // B16: `:h ip`/`:h ap` on a blank line select the blank-line RUN
        // (not a no-op). `ip` stops at the run's edges. `ap` additionally
        // consumes the following non-blank paragraph, if one exists — but
        // if the run touches EOF with no paragraph after it, `ap` is a
        // no-op (verified against nvim: `dap` on a trailing blank run at
        // EOF leaves the buffer untouched).
        while top > 0 && is_blank(top - 1) {
            top -= 1;
        }
        while bot + 1 < n_lines && is_blank(bot + 1) {
            bot += 1;
        }
        if !inner {
            if bot + 1 < n_lines {
                bot += 1;
                while bot + 1 < n_lines && !is_blank(bot + 1) {
                    bot += 1;
                }
            } else {
                return None;
            }
        }
    } else {
        while top > 0 && !is_blank(top - 1) {
            top -= 1;
        }
        while bot + 1 < n_lines && !is_blank(bot + 1) {
            bot += 1;
        }
        // For `ap`, include one trailing blank line if present.
        if !inner && bot + 1 < n_lines && is_blank(bot + 1) {
            bot += 1;
        }
    }
    // `Nip` / `Nap` extend across `count - 1` further units. For `ip` a unit is
    // a single block — a maximal run of same-blankness lines — so counting
    // alternates paragraph → blank gap → paragraph …. For `ap` a unit is a
    // whole paragraph together with its trailing blank gap (vim `:help ap`),
    // so `2ap` reaches the blank lines after the second paragraph too.
    let mut rem = count - 1;
    while rem > 0 && bot + 1 < n_lines {
        if inner {
            let blank_next = is_blank(bot + 1);
            bot += 1;
            while bot + 1 < n_lines && is_blank(bot + 1) == blank_next {
                bot += 1;
            }
        } else {
            while bot + 1 < n_lines && !is_blank(bot + 1) {
                bot += 1;
            }
            while bot + 1 < n_lines && is_blank(bot + 1) {
                bot += 1;
            }
        }
        rem -= 1;
    }
    // vim `:h ap`: a paragraph object takes the trailing blank lines, or —
    // when the paragraph runs to the end of the buffer with no blank line
    // after it — the leading blank lines instead. `bot` sitting on a
    // non-blank line at real EOF (`bot + 1 >= n_lines`, the phantom row
    // already excluded above) means no trailing blank was available, so fall
    // back to absorbing the whole leading blank run. When a trailing blank
    // *was* taken `bot` is blank, so this is skipped and a middle-paragraph
    // `dap` keeps its leading run intact. Checked after the `Nap` count loop
    // so it reflects the final span.
    if !inner && bot + 1 >= n_lines && !is_blank(bot) {
        while top > 0 && is_blank(top - 1) {
            top -= 1;
        }
    }
    let end_col = rope_line_to_str(&rope, bot).chars().count();
    Some(((top, 0), (bot, end_col)))
}
