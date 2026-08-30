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
pub type Pos = (usize, usize);

/// Map a text-object key to its [`TextObject`]: `w`/`W` words, `"`/`'`/`` ` ``
/// quotes, `(`/`)`/`b`, `[`/`]`, `{`/`}`/`B`, `<`/`>`, `p` paragraph,
/// `t` XML tag, `s` sentence. `None` for keys with no text object.
///
/// Single source for the mapping — three callers previously carried
/// byte-identical copies (visual extend, operator-pending, sneak), which
/// drifted in fallback style. Each caller keeps its own `None` handling.
pub fn text_object_from_char(ch: char) -> Option<TextObject> {
    Some(match ch {
        'w' => TextObject::Word { big: false },
        'W' => TextObject::Word { big: true },
        '"' | '\'' | '`' => TextObject::Quote(ch),
        '(' | ')' | 'b' => TextObject::Bracket('('),
        '[' | ']' => TextObject::Bracket('['),
        '{' | '}' | 'B' => TextObject::Bracket('{'),
        '<' | '>' => TextObject::Bracket('<'),
        'p' => TextObject::Paragraph,
        't' => TextObject::XmlTag,
        's' => TextObject::Sentence,
        _ => return None,
    })
}
/// Returns `(start, end, kind)` where `end` is *exclusive* (one past the
/// last character to act on). `kind` is `Linewise` for line-oriented text
/// objects like paragraphs and `Exclusive` otherwise.
pub fn text_object_range<H: hjkl_engine::types::Host>(
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
///
/// Scans row by row outward from the cursor, stopping at the first
/// boundary, instead of materializing the whole buffer into per-row
/// `Vec<char>`s. The boundary rules are identical to the old
/// full-buffer `sentence_boundaries` walk: a terminator's trailing
/// whitespace pushes the boundary onto the *first* row above it that is
/// blank or holds a non-whitespace char (skipping non-blank
/// all-whitespace rows), which the `stopper_eol` flag below carries
/// across rows.
pub fn sentence_boundary<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    forward: bool,
) -> Option<(usize, usize)> {
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let raw_n_lines = rope.len_lines();
    if raw_n_lines == 0 {
        return None;
    }
    // Borrow row `r` as a `Cow<str>` instead of cloning a `String` per row
    // (`rope_line_to_str`). `rope_line_bytes` gives the byte length of the
    // row's content with every line separator excluded, matching
    // `hjkl_buffer::rope_line_str` exactly (see `viewport_math::rope_line_slice`).
    let line_of = |r: usize| -> std::borrow::Cow<'_, str> {
        let start = rope.line_to_byte(r);
        rope.byte_slice(start..start + hjkl_buffer::rope_line_bytes(&rope, r))
            .into()
    };
    let line_chars = |r: usize| -> Vec<char> { line_of(r).chars().collect() };
    // Skip vim's single phantom trailing empty row — ropey's len_lines()
    // always synthesizes one extra empty final "line" when the buffer
    // text ends in `\n` (see hjkl_engine::motions::move_bottom / the
    // content_row_count clamp it shares with every vertical motion). A
    // genuinely empty *real* last line (e.g. "One.\n\n") is left alone.
    let n_lines = if raw_n_lines > 1 && line_chars(raw_n_lines - 1).is_empty() {
        raw_n_lines - 1
    } else {
        raw_n_lines
    };
    if n_lines == 0 {
        return None;
    }
    let cursor = ed.cursor();
    let cursor = (cursor.0.min(n_lines - 1), cursor.1);
    let (cr, cc) = cursor;

    if forward {
        // The first real boundary strictly past the cursor — the scan
        // lives in [`first_sentence_boundary_forward`].
        if let Some(p) = first_sentence_boundary_forward(&line_chars, cr, cc, n_lines) {
            return Some(p);
        }
        // No next sentence: land on the last character of the buffer,
        // but never move backward past the cursor.
        let end_col = line_chars(n_lines - 1).len().saturating_sub(1);
        let end = (n_lines - 1, end_col);
        (end > cursor).then_some(end)
    } else {
        // Closest stopper row below the current row, with whether its
        // terminator's trailing whitespace runs to EOL. Maintained
        // incrementally as the scan descends (rows below are visited
        // first), so each row's walk-from-below boundary is a flag check
        // rather than a per-row re-walk.
        let mut origin: Option<(usize, bool)> = if cr == 0 {
            None
        } else {
            closest_stopper_below(&line_chars, cr - 1)
        };
        for r in (0..=cr).rev() {
            // Fetch the row's chars once — the blank check, first-non-ws and
            // `scan_row_boundaries` used to re-materialize the row each time.
            let lc = line_chars(r);
            let blank = lc.is_empty();
            let first_ns = lc.iter().position(|&c| !c.is_whitespace());
            let (mid, _has_eol) = scan_row_boundaries(r, &lc);
            // Candidates in descending order: mid-line boundaries (largest
            // col first), the walk-from-below landing, the blank-line
            // transition, then `(0, 0)`.
            let mut cands: Vec<(usize, usize)> = Vec::new();
            cands.extend(mid.into_iter().rev());
            if origin.is_some_and(|(_, eol)| eol)
                && let Some(c) = first_ns
            {
                cands.push((r, c));
            }
            // The transition boundary compares against the row *below* `r`
            // (smaller index) — read it directly, since the descending scan
            // has already passed it.
            let prev_blank = r > 0 && line_chars(r - 1).is_empty();
            if r > 0 && prev_blank != blank {
                cands.push((r, 0));
            }
            if r == 0 {
                cands.push((0, 0));
            }
            for (row, col) in cands {
                if r < cr || col < cc {
                    return Some((row, col));
                }
            }
            origin = if r > 0 {
                if row_skippable(&line_chars(r - 1)) {
                    // The row below the next row is skippable, so the
                    // closest stopper below it is unchanged.
                    origin
                } else if r >= 2 {
                    // `r - 1` is a stopper itself but sits *above* the
                    // next row's search range `[0, r-2]` — walk down past
                    // the skippable run directly below it.
                    closest_stopper_below(&line_chars, r - 2)
                } else {
                    None
                }
            } else {
                None
            };
        }
        None
    }
}

/// The first real sentence boundary strictly past cursor `(cr, cc)` —
/// the forward half of [`sentence_boundary`]'s scan, extracted so
/// [`sentence_step_forward`] can reuse it. Scans rows `cr..n_lines`
/// row by row, stopping at the first candidate with `r > cr ||
/// col > cc`. `None` when no boundary exists before end-of-buffer —
/// the caller applies its own run-off-the-end fallback.
fn first_sentence_boundary_forward<F: Fn(usize) -> Vec<char>>(
    line_chars: &F,
    cr: usize,
    cc: usize,
    n_lines: usize,
) -> Option<(usize, usize)> {
    // Closest row below the cursor whose terminator's trailing
    // whitespace runs to end-of-line. Such a terminator pushes a
    // boundary onto the first stopper row above it — which may be the
    // cursor's own row — so the flag is seeded by walking down from
    // `cr - 1` past skippable rows.
    let mut stopper_eol = if cr == 0 {
        false
    } else {
        closest_stopper_below(line_chars, cr - 1).is_some_and(|(_, eol)| eol)
    };
    let mut prev_blank = cr > 0 && line_chars(cr - 1).is_empty();
    for r in cr..n_lines {
        // Fetch the row's chars once — the blank check, first-non-ws and
        // `scan_row_boundaries` used to re-materialize the row each time.
        let lc = line_chars(r);
        let blank = lc.is_empty();
        let first_ns = lc.iter().position(|&c| !c.is_whitespace());
        let mut cands: Vec<(usize, usize)> = Vec::new();
        // Blank-line transition: `(r, 0)` when the previous row's
        // blankness differs.
        if r > 0 && prev_blank != blank {
            cands.push((r, 0));
        }
        // Trailing-whitespace walk from a terminator below this row
        // lands here on this row's first non-whitespace cell.
        if stopper_eol && let Some(c) = first_ns {
            cands.push((r, c));
        }
        let (mid, has_eol) = scan_row_boundaries(r, &lc);
        cands.extend(mid);
        for (row, col) in cands {
            if r > cr || col > cc {
                return Some((row, col));
            }
        }
        prev_blank = blank;
        stopper_eol = if first_ns.is_some() || blank {
            has_eol
        } else {
            stopper_eol
        };
    }
    None
}

/// A row the trailing-whitespace walk passes *through* without stopping:
/// non-blank and holding only whitespace.
fn row_skippable(line: &[char]) -> bool {
    !line.is_empty() && line.iter().all(|&c| c.is_whitespace())
}

/// The closest row at or below `p` (walking down past skippable rows)
/// that the trailing-whitespace walk would stop on — blank or holding a
/// non-whitespace char — with whether its terminator's whitespace runs
/// to end-of-line. `None` when every row down to 0 is skippable.
fn closest_stopper_below<F: Fn(usize) -> Vec<char>>(
    line_chars: &F,
    mut p: usize,
) -> Option<(usize, bool)> {
    while p > 0 && row_skippable(&line_chars(p)) {
        p -= 1;
    }
    (!row_skippable(&line_chars(p))).then(|| (p, row_has_eol_walk(line_chars, p)))
}

/// True when row `r` has a terminator whose trailing whitespace (or the
/// terminator itself) runs to end-of-line — such a terminator pushes a
/// boundary onto a later row.
fn row_has_eol_walk<F: Fn(usize) -> Vec<char>>(line_chars: &F, r: usize) -> bool {
    scan_row_boundaries(r, &line_chars(r)).1
}

/// Per-row sentence-boundary scan, mirroring `sentence_boundaries`'s
/// within-row loop: terminator runs, optional closing-punctuation runs,
/// and the whitespace that completes a boundary. Returns the mid-line
/// boundaries landing on this row (ascending) and whether a terminator's
/// trailing whitespace runs to end-of-line (its boundary lands on a later
/// row — see [`sentence_boundary`]).
fn scan_row_boundaries(r: usize, line: &[char]) -> (Vec<(usize, usize)>, bool) {
    let lc = line;
    let mut mid: Vec<(usize, usize)> = Vec::new();
    let mut has_eol = false;
    let mut i = 0;
    while i < lc.len() {
        if is_sentence_terminator(lc[i]) {
            let mut j = i;
            while j + 1 < lc.len() && is_sentence_terminator(lc[j + 1]) {
                j += 1;
            }
            let mut k = j;
            while k + 1 < lc.len() && is_sentence_closing(lc[k + 1]) {
                k += 1;
            }
            if k + 1 < lc.len() {
                // Terminator (+ closing run) followed by more text on the
                // same line: only a boundary if that text starts with
                // whitespace, and the boundary is the first non-whitespace
                // cell after that whitespace — or a later row when the
                // whitespace runs to EOL.
                if lc[k + 1].is_whitespace() {
                    let mut c = k + 1;
                    while c < lc.len() && lc[c].is_whitespace() {
                        c += 1;
                    }
                    if c < lc.len() {
                        mid.push((r, c));
                    } else {
                        has_eol = true;
                    }
                }
                i = k + 1;
            } else {
                // Terminator run reaches end of line — the boundary (if
                // any) is whatever comes after the line break.
                has_eol = true;
                break;
            }
        } else {
            i += 1;
        }
    }
    (mid, has_eol)
}

/// Resolve the reverse landing used by a multi-row VisualBlock `is` / `as`.
///
/// Vim walks sentence bodies and same-line separator whitespace as separate
/// units while moving left on a row. Across a line break it advances directly
/// between sentence bodies: the newline is not a selectable separator unit.
pub fn reverse_visual_block_sentence_landing<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    inner: bool,
    count: usize,
) -> Option<Pos> {
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let raw_n_lines = rope.len_lines();
    if raw_n_lines == 0 {
        return None;
    }
    let mut lines: Vec<Vec<char>> = (0..raw_n_lines)
        .map(|row| rope_line_to_str(&rope, row).chars().collect())
        .collect();
    if lines.len() > 1 && lines.last().is_some_and(Vec::is_empty) {
        lines.pop();
    }
    let n_lines = lines.len();
    if n_lines == 0 {
        return None;
    }
    let cursor = (ed.cursor().0.min(n_lines - 1), ed.cursor().1);
    let mut starts: Vec<Pos> = sentence_boundaries(&lines, n_lines)
        .into_iter()
        .filter(|&(row, col)| lines[row].get(col).is_some_and(|ch| !ch.is_whitespace()))
        .collect();
    for row in 1..n_lines {
        let Some(col) = lines[row].iter().position(|ch| !ch.is_whitespace()) else {
            continue;
        };
        let mut previous = row - 1;
        while previous > 0 && row_skippable(&lines[previous]) {
            previous -= 1;
        }
        let mut tail = &lines[previous][..];
        while tail
            .last()
            .is_some_and(|ch| ch.is_whitespace() || is_sentence_closing(*ch))
        {
            tail = &tail[..tail.len() - 1];
        }
        if lines[previous].is_empty() || tail.last().is_some_and(|ch| is_sentence_terminator(*ch)) {
            starts.push((row, col));
        }
    }
    starts.sort_unstable();
    starts.dedup();
    let mut index = starts.iter().rposition(|&start| start <= cursor)?;
    // A reverse block first moves onto the row above its anchor. When that row
    // starts a sentence at the active cursor, Vim's first text-object count
    // resolves the preceding body instead; crossing a line break never exposes
    // the active row as a separator unit.
    if starts[index] == cursor && index > 0 && starts[index - 1].0 < cursor.0 {
        index -= 1;
    }
    let current = starts[index];
    let separator_start = |(row, col): Pos| {
        let line = &lines[row];
        let mut start = col;
        while start > 0 && line[start - 1].is_whitespace() {
            start -= 1;
        }
        (start < col).then_some((row, start))
    };

    let mut landings = Vec::new();
    if inner {
        landings.push(current);
    } else {
        landings.push(separator_start(current).unwrap_or(current));
    }
    let mut last = current;
    while index > 0 && starts[index - 1].0 == current.0 {
        index -= 1;
        if inner {
            if let Some(separator) = separator_start(last) {
                landings.push(separator);
            }
            landings.push(starts[index]);
        } else if let Some(separator) = separator_start(starts[index]) {
            landings.push(separator);
        } else {
            landings.push(starts[index]);
        }
        last = starts[index];
    }
    let count = count.max(1);
    if let Some(&landing) = landings.get(count - 1) {
        return Some(landing);
    }

    let mut remaining = count - landings.len();
    while remaining > 0 && index > 0 {
        index -= 1;
        remaining -= 1;
    }
    starts.get(index).copied()
}

/// Every valid sentence-boundary landing position within `lines[..n_lines]`,
/// in ascending order (deduplicated). Always includes `(0, 0)`. Kept as the
/// full-buffer reference for the differential tests (`old_sentence_boundary`
/// / `old_sentence_step_forward`).
#[cfg_attr(not(test), allow(dead_code))]
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
#[cfg_attr(not(test), allow(dead_code))]
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
/// column 0 if that row happens to be empty. Test-only reference now
/// (the row-by-row `sentence_step_forward` computes this from the rope).
#[cfg_attr(not(test), allow(dead_code))]
fn end_of_buffer_pos(lines: &[Vec<char>], n_lines: usize) -> (usize, usize) {
    let last = n_lines - 1;
    let col = lines[last].len().saturating_sub(1);
    (last, col)
}

/// One repetition of `)`, classified the way vim's `findsent` classifies it.
///
/// `findsent` returns `FAIL` — and `nv_brace` then beeps and leaves the cursor
/// exactly where it was — when a repetition has to walk off the end of the
/// buffer while further repetitions are still owed. Landing at end-of-buffer
/// is only a legal *final* landing. A repetition that starts already at the
/// last cell is a no-op success, which is why `9)` at end-of-buffer neither
/// moves nor beeps.
#[derive(Debug, PartialEq, Eq)]
pub enum SentenceStep {
    /// A real boundary: a sentence terminator (possibly the one that closes
    /// the buffer) or a blank-line transition.
    Boundary((usize, usize)),
    /// No boundary left. The landing is the buffer's last cell, and it counts
    /// only when this is the last repetition.
    EndOfBuffer((usize, usize)),
    /// Cursor is already at (or past) the last cell — nothing to do.
    AtEnd,
}

/// Classify the next `)` step from the cursor. See [`SentenceStep`].
pub fn sentence_step_forward<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
) -> SentenceStep {
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let raw_n_lines = rope.len_lines();
    if raw_n_lines == 0 {
        return SentenceStep::AtEnd;
    }
    // Borrow row `r` as a `Cow<str>` instead of cloning a `String` per row
    // (`rope_line_to_str`). `rope_line_bytes` gives the byte length of the
    // row's content with every line separator excluded, matching
    // `hjkl_buffer::rope_line_str` exactly (see `viewport_math::rope_line_slice`).
    let line_of = |r: usize| -> std::borrow::Cow<'_, str> {
        let start = rope.line_to_byte(r);
        rope.byte_slice(start..start + hjkl_buffer::rope_line_bytes(&rope, r))
            .into()
    };
    let line_chars = |r: usize| -> Vec<char> { line_of(r).chars().collect() };
    // Same phantom-trailing-row clamp as `sentence_boundary`.
    let n_lines = if raw_n_lines > 1 && line_chars(raw_n_lines - 1).is_empty() {
        raw_n_lines - 1
    } else {
        raw_n_lines
    };
    if n_lines == 0 {
        return SentenceStep::AtEnd;
    }
    let cursor = ed.cursor();
    let cursor = (cursor.0.min(n_lines - 1), cursor.1);
    if let Some(p) = first_sentence_boundary_forward(&line_chars, cursor.0, cursor.1, n_lines) {
        return SentenceStep::Boundary(p);
    }
    let last_row = line_chars(n_lines - 1);
    let end = (n_lines - 1, last_row.len().saturating_sub(1));
    if end <= cursor {
        return SentenceStep::AtEnd;
    }
    // A terminator (plus any closing run and trailing whitespace) that closes
    // the buffer still ends a sentence in vim, so this landing is a real
    // boundary rather than the run-off-the-end fallback: `3)` on
    // `"One. Two."` stops at the last char, while `3)` on `"One. Two"` fails.
    let mut tail: &[char] = &last_row;
    while tail.last().is_some_and(|c| c.is_whitespace()) {
        tail = &tail[..tail.len() - 1];
    }
    while tail.last().is_some_and(|c| is_sentence_closing(*c)) {
        tail = &tail[..tail.len() - 1];
    }
    if tail.last().is_some_and(|c| is_sentence_terminator(*c)) {
        SentenceStep::Boundary(end)
    } else {
        SentenceStep::EndOfBuffer(end)
    }
}
/// `is` / `as` — sentence: text up to and including the next sentence
/// terminator (`.`, `?`, `!`). Vim treats `.`/`?`/`!` followed by
/// whitespace (or end-of-line) as a boundary; runs of consecutive
/// terminators stay attached to the same sentence. `as` extends to
/// include trailing whitespace; `is` does not.
///
/// Runs the flat-Vec scan over a window of rows around the cursor
/// instead of the whole buffer; when a walk would need to cross the
/// window's edge the full-buffer scan takes over, so the result is
/// byte-for-byte identical either way.
pub fn sentence_text_object<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    inner: bool,
    count: usize,
) -> Option<((usize, usize), (usize, usize))> {
    let count = count.max(1);
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let raw_n_lines = rope.len_lines();
    if raw_n_lines == 0 {
        return None;
    }
    let cursor = ed.cursor();
    let win_lo = cursor.0.saturating_sub(SENTENCE_WINDOW_ROWS);
    let win_hi = (cursor.0 + SENTENCE_WINDOW_ROWS).min(raw_n_lines - 1);
    let line_of = |r: usize| -> std::borrow::Cow<'_, str> {
        let start = rope.line_to_byte(r);
        rope.byte_slice(start..start + hjkl_buffer::rope_line_bytes(&rope, r))
            .into()
    };
    let line_chars = |r: usize| -> Vec<char> { line_of(r).chars().collect() };
    let line_len = |r: usize| -> usize { line_of(r).chars().count() };
    let win_off = rope.line_to_char(win_lo);
    let (win_lens, chars, last_content) =
        window_flat(raw_n_lines, win_lo, win_hi, &line_len, &line_chars);
    let flat_len = chars.len();
    let whole_buffer = win_lo == 0 && win_hi >= last_content;
    if flat_len == 0 {
        // The old scan returns None for a content-less buffer; a window
        // that starts past the buffer's content (phantom-row cursor)
        // needs the full scan to answer.
        return if whole_buffer {
            None
        } else {
            sentence_text_object_full(ed, inner, count)
        };
    }
    // Flat index ↔ (row, col) over the window. One extra `win_lens`
    // entry past the window's top row makes a flat index sitting on a
    // `\n` map to the next row exactly like the old whole-buffer
    // arithmetic.
    let idx_to_pos = |mut idx: usize| -> (usize, usize) {
        for (i, &len) in win_lens.iter().enumerate() {
            if idx <= len {
                return (win_lo + i, idx);
            }
            idx -= len + 1;
        }
        let last = win_lens.len() - 1;
        (win_lo + last, win_lens[last])
    };
    // Cursor's flat index: the rope's char offset of the cursor minus the
    // window's offset. ropey's char space matches the flat scan — every
    // row contributes its chars plus one separator, and the popped final
    // newline only sits past the last content row.
    let cursor_idx = (rope.line_to_char(cursor.0) + cursor.1 - win_off).min(flat_len - 1);

    let Some((start, end, clipped)) =
        sentence_text_object_on_chars(&chars, cursor_idx, inner, count)
    else {
        return if whole_buffer {
            None
        } else {
            sentence_text_object_full(ed, inner, count)
        };
    };
    // A walk that ran off the window's edge is only trustworthy when the
    // window covers the whole buffer; otherwise recompute on the full buffer.
    if clipped && !whole_buffer {
        return sentence_text_object_full(ed, inner, count);
    }

    Some((idx_to_pos(start), idx_to_pos(end)))
}

/// The whole-buffer `is`/`as` scan — the fallback for windowed
/// [`sentence_text_object`] when a walk crosses the window's edge.
fn sentence_text_object_full<H: hjkl_engine::types::Host>(
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

    let (start, end, _) = sentence_text_object_on_chars(&chars, cursor_idx, inner, count)?;

    Some((idx_to_pos(start), idx_to_pos(end)))
}

/// Flat-char `decl`: step back one character, skipping an internal `\n`
/// (vim's NUL at end-of-line), so positions stay on real characters.
fn sentence_decl(chars: &[char], i: usize) -> Option<usize> {
    if i == 0 {
        None
    } else if chars[i - 1] == '\n' {
        i.checked_sub(2)
    } else {
        Some(i - 1)
    }
}

/// Flat-char `incl`: step forward one character, skipping an internal `\n`.
fn sentence_incl(chars: &[char], i: usize) -> Option<usize> {
    let n = chars.len();
    if i + 1 >= n {
        None
    } else if chars[i + 1] == '\n' {
        if i + 2 >= n { None } else { Some(i + 2) }
    } else {
        Some(i + 1)
    }
}

/// vim `find_first_blank`: walk back over same-line blanks to the first blank
/// of the run ending at `i`.
fn sentence_first_blank(chars: &[char], i: usize) -> usize {
    let mut p = i;
    while let Some(q) = sentence_decl(chars, p) {
        if chars[q] == ' ' || chars[q] == '\t' {
            p = q;
        } else {
            break;
        }
    }
    p
}

/// Flat port of vim's `findsent(FORWARD, 1)`: the next sentence start (first
/// non-blank cell) strictly after `pos`, or the blank-line / end-of-buffer
/// landing when no next sentence exists.
fn sentence_next_start(chars: &[char], pos: usize, clipped: &mut bool) -> usize {
    let len = chars.len();
    if pos >= len {
        *clipped = true;
        return len;
    }
    let is_ws = |c: char| c == ' ' || c == '\t';
    let is_punct = |c: char| matches!(c, '.' | '!' | '?' | ')' | ']' | '"' | '\'');
    let is_term = |c: char| matches!(c, '.' | '!' | '?');
    let is_close = |c: char| matches!(c, ')' | ']' | '"' | '\'');

    // (1) Back up over white space and punctuation to the previous
    // non-white, non-punctuation character (vim's "go back" loop).
    let mut p = pos;
    let mut found_dot = false;
    loop {
        let c = chars[p];
        if !is_ws(c) && !is_punct(c) {
            break;
        }
        let Some(tp) = sentence_decl(chars, p) else {
            break;
        };
        if found_dot {
            break;
        }
        if is_term(c) {
            found_dot = true;
        }
        if is_close(c) && !is_punct(chars[tp]) {
            break;
        }
        p = tp;
    }

    // (2) Scan forward for the sentence's terminator boundary, then skip
    // same-line blanks (and the line break of a sentence ending at EOL) to
    // the next sentence start.
    let mut e = p;
    while e < len {
        let c = chars[e];
        if is_term(c) {
            let mut t = e;
            while t + 1 < len && is_term(chars[t + 1]) {
                t += 1;
            }
            while t + 1 < len && is_close(chars[t + 1]) {
                t += 1;
            }
            if t + 1 >= len {
                return len;
            }
            let n = chars[t + 1];
            if is_ws(n) {
                let mut k = t + 1;
                while k < len && is_ws(chars[k]) {
                    k += 1;
                }
                return k;
            } else if n == '\n' {
                let mut k = t + 2;
                while k < len && is_ws(chars[k]) {
                    k += 1;
                }
                return k;
            }
            e = t + 1;
        } else {
            e += 1;
        }
    }
    *clipped = true;
    len
}

/// True when `i` sits within the leading white space of its line (vim's
/// `inindent` at column 0 of the object start decides whether the operator's
/// column-zero adjustment keeps or drops the trailing line break).
fn sentence_at_line_leading_ws(chars: &[char], i: usize) -> bool {
    let mut ls = i;
    while ls > 0 && chars[ls - 1] != '\n' {
        ls -= 1;
    }
    (ls..i).all(|k| chars[k] == ' ' || chars[k] == '\t')
}

/// The counted `is` / `as` scan over a flat char slice, shared by the windowed
/// and whole-buffer paths. Mirrors vim's `current_sent`: it walks sentence
/// bodies and same-line separators as alternating units (vim's `findsent_forward`),
/// so an even count ends after a separator and an over-run caps at the buffer
/// end. Returns `(start, end_exclusive)` flat indices plus whether the walk ran
/// off the edge of `chars` (only meaningful for a window that is not the whole
/// buffer).
fn sentence_text_object_on_chars(
    chars: &[char],
    cursor_idx: usize,
    inner: bool,
    count: usize,
) -> Option<(usize, usize, bool)> {
    let len = chars.len();
    if len == 0 {
        return None;
    }
    let count = count.max(1);
    let cursor_idx = cursor_idx.min(len - 1);
    let is_ws = |c: char| c == ' ' || c == '\t' || c == '\n';
    let is_term = |c: char| matches!(c, '.' | '?' | '!');
    let mut clipped = false;

    // Cursor on white space immediately after a terminator selects the blank
    // run itself as the first unit (vim's `start_blank` path).
    let mut blank = false;
    if is_ws(chars[cursor_idx]) {
        let mut k = cursor_idx;
        while k > 0 && is_ws(chars[k - 1]) {
            k -= 1;
        }
        if k > 0 && is_term(chars[k - 1]) {
            blank = true;
        }
    }

    let start = if blank {
        sentence_first_blank(chars, cursor_idx)
    } else {
        let mut s = cursor_idx;
        while s > 0 {
            let prev = chars[s - 1];
            if is_ws(prev) {
                let mut k = s - 1;
                while k > 0 && is_ws(chars[k - 1]) {
                    k -= 1;
                }
                if k > 0 && is_term(chars[k - 1]) {
                    break;
                }
            }
            s -= 1;
        }
        if s == 0 {
            clipped = true;
        }
        while s < len && is_ws(chars[s]) {
            s += 1;
        }
        if s >= len {
            return None;
        }
        s
    };

    // `as` walks twice as many units; a blank start already consumed its blank
    // as the first unit.
    let ncount = if !inner {
        count * 2
    } else if blank {
        count - 1
    } else {
        count
    };

    let mut pos = if blank {
        sentence_next_start(chars, cursor_idx, &mut clipped)
    } else {
        start
    };
    if ncount == 0 {
        // Blank start with `count == 1` (`is` only): vim steps back from the
        // next sentence start, so the object is exactly the blank run.
        pos = sentence_decl(chars, pos).unwrap_or(pos);
    }
    let mut at_start_sent = true;
    let mut remaining = ncount;
    while remaining > 0 {
        pos = sentence_next_start(chars, pos, &mut clipped);
        if at_start_sent {
            pos = sentence_first_blank(chars, pos);
        }
        if remaining == 1 || at_start_sent {
            match sentence_decl(chars, pos) {
                Some(d) => pos = d,
                None => clipped = true,
            }
        }
        at_start_sent = !at_start_sent;
        remaining -= 1;
    }

    // Exclusive end: vim moves one character past (skipping a NUL) and then,
    // when that lands at the start of a line and the object did not begin in
    // indentation, backs the trailing line break out again.
    let raw_end = sentence_incl(chars, pos).unwrap_or(len);
    let at_line_start = raw_end > 0 && chars[raw_end - 1] == '\n';
    let end = if at_line_start && !sentence_at_line_leading_ws(chars, start) {
        raw_end - 1
    } else {
        raw_end
    };

    Some((start, end, clipped))
}
/// `it` / `at` — XML tag pair text object. Builds a flat char index of
/// the buffer, walks `<...>` tokens to pair tags via a stack, and
/// returns the innermost pair containing the cursor.
///
/// Runs the token walk over a window of rows around the cursor; when no
/// pair is found inside the window, or the found pair touches the
/// window's edge (its open or close may extend past it), the
/// whole-buffer walk decides.
pub fn tag_text_object<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    inner: bool,
) -> Option<((usize, usize), (usize, usize))> {
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let raw_n_lines = rope.len_lines();
    if raw_n_lines == 0 {
        return None;
    }
    let cursor = ed.cursor();
    let win_lo = cursor.0.saturating_sub(TAG_WINDOW_ROWS);
    let win_hi = (cursor.0 + TAG_WINDOW_ROWS).min(raw_n_lines - 1);
    let line_of = |r: usize| -> std::borrow::Cow<'_, str> {
        let start = rope.line_to_byte(r);
        rope.byte_slice(start..start + hjkl_buffer::rope_line_bytes(&rope, r))
            .into()
    };
    let line_chars = |r: usize| -> Vec<char> { line_of(r).chars().collect() };
    let line_len = |r: usize| -> usize { line_of(r).chars().count() };
    let win_off = rope.line_to_char(win_lo);
    let (win_lens, chars, last_content) =
        window_flat(raw_n_lines, win_lo, win_hi, &line_len, &line_chars);
    let flat_len = chars.len();
    let whole_buffer = win_lo == 0 && win_hi >= last_content;
    if flat_len == 0 {
        return if whole_buffer {
            None
        } else {
            tag_text_object_full(ed, inner)
        };
    }
    let idx_to_pos = |mut idx: usize| -> (usize, usize) {
        for (i, &len) in win_lens.iter().enumerate() {
            if idx <= len {
                return (win_lo + i, idx);
            }
            idx -= len + 1;
        }
        let last = win_lens.len() - 1;
        (win_lo + last, win_lens[last])
    };
    // Cursor's flat index — deliberately unclamped, matching the old
    // scan (a past-end column still compares sensibly against tag spans).
    let cursor_idx = rope.line_to_char(cursor.0) + cursor.1 - win_off;

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
    while i < flat_len {
        if chars[i] != '<' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < flat_len && chars[j] != '>' {
            j += 1;
        }
        if j >= flat_len {
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

    let Some((open_start, content_start, content_end, close_end)) = innermost.or(next_after) else {
        return tag_text_object_full(ed, inner);
    };
    // A pair whose open sits on the window's first row (with content
    // below it) or whose close sits on/after the window's last row could
    // be a fragment of a pair extending past the window — the
    // whole-buffer scan decides.
    let open_row = idx_to_pos(open_start).0;
    let close_row = idx_to_pos(close_end).0;
    if (open_row == win_lo && win_lo > 0) || (close_row >= win_hi && win_hi < raw_n_lines - 1) {
        return tag_text_object_full(ed, inner);
    }
    if inner {
        Some((idx_to_pos(content_start), idx_to_pos(content_end)))
    } else {
        Some((idx_to_pos(open_start), idx_to_pos(close_end)))
    }
}

/// The whole-buffer `it`/`at` walk — the fallback for windowed
/// [`tag_text_object`].
fn tag_text_object_full<H: hjkl_engine::types::Host>(
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

/// Rows above and below the cursor that the windowed text-object scans
/// cover before falling back to the whole buffer.
const SENTENCE_WINDOW_ROWS: usize = 200;
const TAG_WINDOW_ROWS: usize = 50;

/// Build the windowed flat char scan over rows `[win_lo, win_hi]` — a
/// slice of the buffer around the cursor. Returns the per-row char
/// counts for the rows that contribute content (plus one extra entry for
/// the row just past the window when it stops short of the buffer's last
/// content row, so a flat index sitting on a `\n` maps to the next row
/// exactly like the old whole-buffer arithmetic), the joined flat
/// `Vec<char>` (with a trailing `\n` only when the window stops short of
/// the buffer's end), and the buffer's last content row.
///
/// `line_len`/`line_chars` must both read row `r`'s content with the
/// trailing line separator excluded (the `rope_line_to_str` contract).
fn window_flat<F: Fn(usize) -> usize, G: Fn(usize) -> Vec<char>>(
    raw_n_lines: usize,
    win_lo: usize,
    win_hi: usize,
    line_len: &F,
    line_chars: &G,
) -> (Vec<usize>, Vec<char>, usize) {
    // The last row that contributes content: the old scan pops the
    // buffer's final `\n`, so ropey's phantom empty last row (synthesized
    // for a trailing newline) contributes nothing.
    let last_content = if raw_n_lines > 1 && line_len(raw_n_lines - 1) == 0 {
        raw_n_lines - 2
    } else {
        raw_n_lines - 1
    };
    let hi = win_hi.min(last_content);
    let mut win_lens: Vec<usize> = (win_lo..=hi).map(line_len).collect();
    let mut flat: Vec<char> = Vec::new();
    for r in win_lo..=hi {
        flat.extend(line_chars(r));
        if r < hi {
            flat.push('\n');
        }
    }
    if hi < last_content {
        flat.push('\n');
        win_lens.push(line_len(hi + 1));
    }
    (win_lens, flat, last_content)
}
pub fn is_wordchar(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
pub use hjkl_buffer::is_keyword_char;
pub fn abbrev_kind(lhs: &str, iskeyword: &str) -> AbbrevKind {
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
pub fn try_abbrev_expand(
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
pub fn check_and_apply_abbrev<H: hjkl_engine::types::Host>(
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

    true
}
pub fn word_text_object<H: hjkl_engine::types::Host>(
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
pub fn quote_text_object<H: hjkl_engine::types::Host>(
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
pub fn bracket_text_object<H: hjkl_engine::types::Host>(
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
    // the innermost pair, re-anchoring to each enclosing bracket in turn. An
    // unavailable enclosing level makes the Visual text object a no-op.
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
            let oo = outer?;
            let oc = find_close_bracket(lines, oo.0, oo.1 + 1, open, close)?;
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
pub fn find_open_bracket(
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
pub fn find_close_bracket(
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
pub fn find_next_open(
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
pub fn advance_pos(lines: &[String], pos: (usize, usize)) -> (usize, usize) {
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
pub fn paragraph_text_object<H: hjkl_engine::types::Host>(
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
    // When the buffer ends before all `count - 1` further units could be
    // consumed, `rem` is still positive: nvim treats the counted object as
    // FAILED (a no-op) rather than best-effort extending to the whole buffer.
    // Returning `None` propagates to a no-op in both the operator and visual
    // paths (see `text_object_range`).
    if rem > 0 {
        return None;
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

/// Landing row for `ip`/`ap` in a MULTI-LINE visual selection — vim's
/// `current_par` "extend" path (`textobject.c`): the anchor and visual mode
/// stay untouched and the cursor walks one same-blankness run past itself,
/// away from the anchor, landing at the run's end on column 0. `ap`
/// (`inner == false`) continues with a second run of the opposite
/// blankness, mirroring `current_par`'s `include` loop. `None` when the
/// block is a single row (vim takes the normal object path there) or the
/// first step leaves the buffer (vim's `current_par` FAILs — a no-op, not
/// a collapse). Measured against neovim 0.12.4: `<C-v>jip` on
/// `"one\n\ntwo\n"` lands the cursor on row 2 with the block spanning rows
/// 0-2 at column 0.
pub fn paragraph_extend_landing<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    cursor_row: usize,
    anchor_row: usize,
    inner: bool,
) -> Option<usize> {
    if cursor_row == anchor_row {
        return None;
    }
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let raw_n_lines = rope.len_lines();
    // Mirror `paragraph_text_object`'s phantom-row-aware count: the single
    // trailing empty row ropey synthesizes for a buffer ending in `\n` is
    // not a real line and must not be walked onto.
    let n_lines = if raw_n_lines > 1 && rope_line_to_str(&rope, raw_n_lines - 1).is_empty() {
        raw_n_lines - 1
    } else {
        raw_n_lines
    };
    let dir: isize = if cursor_row < anchor_row { -1 } else { 1 };
    let first = cursor_row as isize + dir;
    if first < 0 || first >= n_lines as isize {
        return None;
    }
    let edge = |r: isize| r < 0 || r >= n_lines as isize;
    let is_blank = |r: isize| rope_line_to_str(&rope, r as usize).trim().is_empty();
    // vim's `current_par` treats the first / last line as the walk's edge:
    // the t=1 run never starts from the boundary line.
    let boundary = if dir < 0 { 0 } else { n_lines as isize - 1 };
    let mut land = first;
    let mut prev_blank: Option<bool> = None;
    for t in 0..2 {
        if t == 1 {
            land += dir;
            if prev_blank == Some(is_blank(land)) {
                land -= dir;
                break;
            }
        }
        let blank0 = is_blank(land);
        while !edge(land + dir) && is_blank(land + dir) == blank0 {
            land += dir;
        }
        if inner || land == boundary {
            break;
        }
        prev_blank = Some(blank0);
    }
    Some(land as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_buffer::View;
    use hjkl_engine::{DefaultHost, Editor, Options};

    fn make_editor(content: &str) -> Editor<View, DefaultHost> {
        let buf = View::from_str(content);
        let host = DefaultHost::new();
        crate::vim::vim_editor(buf, host, Options::default())
    }

    /// `is` on the second sentence of a two-sentence paragraph, with a
    /// blank line (paragraph break) after it. Pins the exact charwise
    /// range the windowed flat-Vec scan must reproduce byte-for-byte.
    #[test]
    fn sentence_text_object_is_exact_range_mid_paragraph() {
        let mut ed = make_editor("First sentence. Second one.\n\nThird.");
        ed.set_cursor_quiet(0, 16); // start of "Second one."
        assert_eq!(
            sentence_text_object(&ed, true, 1),
            Some(((0, 16), (0, 27))),
            "is must span exactly \"Second one.\" (exclusive end after the '.')"
        );
        // `as` adds trailing whitespace, but a paragraph break (the '\n')
        // is never gobbled, so it lands on the same exclusive end.
        assert_eq!(
            sentence_text_object(&ed, false, 1),
            Some(((0, 16), (0, 27)))
        );
    }

    /// `is` on a terminator-less sentence with trailing whitespace: nvim ends
    /// the inner object at the last non-blank char, so the trailing spaces are
    /// excluded.
    #[test]
    fn sentence_text_object_is_trim_trailing_whitespace() {
        let mut ed = make_editor("abc def   ");
        ed.set_cursor_quiet(0, 0);
        // `is` → "abc def" (exclusive end at col 7).
        assert_eq!(sentence_text_object(&ed, true, 1), Some(((0, 0), (0, 7))));
        // `as` still includes the trailing whitespace.
        assert_eq!(sentence_text_object(&ed, false, 1), Some(((0, 0), (0, 10))));
    }

    /// Counted `is` walks body / same-line-separator units, so an even count
    /// ends after a separator (`2is` = sentence + following space).
    #[test]
    fn sentence_text_object_counted_inner_alternates_units() {
        let mut ed = make_editor("aaaa. bbbb. cccc.");
        ed.set_cursor_quiet(0, 0);
        assert_eq!(sentence_text_object(&ed, true, 1), Some(((0, 0), (0, 5))));
        assert_eq!(sentence_text_object(&ed, true, 2), Some(((0, 0), (0, 6))));
        assert_eq!(sentence_text_object(&ed, true, 3), Some(((0, 0), (0, 11))));
        assert_eq!(sentence_text_object(&ed, true, 4), Some(((0, 0), (0, 12))));
        // Over-run caps at the buffer end rather than a best-effort extension
        // or a failure.
        assert_eq!(sentence_text_object(&ed, true, 9), Some(((0, 0), (0, 17))));
    }

    /// Counted `as` bundles each body with its following separator, so
    /// `2as` = two (sentence + trailing space) units.
    #[test]
    fn sentence_text_object_counted_around_bundles_separator() {
        let mut ed = make_editor("aaaa. bbbb. cccc.");
        ed.set_cursor_quiet(0, 0);
        assert_eq!(sentence_text_object(&ed, false, 1), Some(((0, 0), (0, 6))));
        assert_eq!(sentence_text_object(&ed, false, 2), Some(((0, 0), (0, 12))));
        assert_eq!(sentence_text_object(&ed, false, 3), Some(((0, 0), (0, 17))));
    }

    /// A blank start (cursor on the separator whitespace) selects the blank
    /// run as its first unit, matching nvim (`1is` = the space, `2is` = space
    /// + next sentence).
    #[test]
    fn sentence_text_object_counted_blank_start() {
        let mut ed =
            make_editor("one two three. four five. six. seven eight nine.\n ten eleven twelve.");
        // Cursor on the space after "six.".
        ed.set_cursor_quiet(0, 30);
        assert_eq!(sentence_text_object(&ed, true, 1), Some(((0, 30), (0, 31))));
        assert_eq!(sentence_text_object(&ed, true, 2), Some(((0, 30), (0, 48))));
        assert_eq!(sentence_text_object(&ed, true, 3), Some(((0, 30), (1, 1))));
        assert_eq!(sentence_text_object(&ed, true, 4), Some(((0, 30), (1, 19))));
    }

    /// Counted `ip` / `ap` over-run: when the count exceeds the available
    /// paragraph units, the object fails (`None`) instead of best-effort
    /// extending to the whole buffer. Pins the nvim-verified no-op.
    #[test]
    fn paragraph_text_object_counted_over_run_is_none() {
        // Single paragraph, no blank lines: only one `ip` unit exists.
        let mut ed = make_editor("aaa.\nbbb.\nccc.\n");
        ed.set_cursor_quiet(0, 0);
        // count 1 still selects the whole paragraph.
        assert_eq!(paragraph_text_object(&ed, true, 1), Some(((0, 0), (2, 4))));
        // `2ip` / `3ip` run out of units → fail.
        assert_eq!(paragraph_text_object(&ed, true, 2), None);
        assert_eq!(paragraph_text_object(&ed, true, 3), None);
        // `ap` on a single paragraph with no trailing blank also fails at
        // count 2 (`2ap` needs a second paragraph + blank gap).
        assert_eq!(paragraph_text_object(&ed, false, 2), None);

        // Three paragraphs (`aaa`, `bbb`, `ccc`) separated by blank lines.
        // `ip` walks same-blankness runs: para, blank, para, blank, para
        // = 5 units, so counts 1..=5 succeed and 6 over-runs.
        let mut ed = make_editor("aaa.\n\nbbb.\n\nccc.\n");
        ed.set_cursor_quiet(0, 0);
        assert_eq!(paragraph_text_object(&ed, true, 1), Some(((0, 0), (0, 4))));
        assert_eq!(paragraph_text_object(&ed, true, 2), Some(((0, 0), (1, 0))));
        assert_eq!(paragraph_text_object(&ed, true, 3), Some(((0, 0), (2, 4))));
        assert_eq!(paragraph_text_object(&ed, true, 4), Some(((0, 0), (3, 0))));
        assert_eq!(paragraph_text_object(&ed, true, 5), Some(((0, 0), (4, 4))));
        assert_eq!(paragraph_text_object(&ed, true, 6), None);
        // `ap` bundles each paragraph with its trailing blank gap: three
        // units here (the last paragraph has no trailing blank), so `3ap`
        // succeeds and `4ap` over-runs → fail.
        assert_eq!(paragraph_text_object(&ed, false, 1), Some(((0, 0), (1, 0))));
        assert_eq!(paragraph_text_object(&ed, false, 2), Some(((0, 0), (3, 0))));
        assert_eq!(paragraph_text_object(&ed, false, 3), Some(((0, 0), (4, 4))));
        assert_eq!(paragraph_text_object(&ed, false, 4), None);
    }

    /// `(` / `)` boundary walking across a blank-line paragraph break.
    /// `sentence_boundary` must find the same (row, col) landings the
    /// full-buffer scan produced.
    #[test]
    fn sentence_boundary_matches_full_scan_across_paragraph_break() {
        let mut ed = make_editor("First sentence. Second one.\n\nThird.");
        // Forward from buffer start → start of "Second one."
        ed.set_cursor_quiet(0, 0);
        assert_eq!(sentence_boundary(&ed, true), Some((0, 16)));
        // Forward from start of "Second one." → the blank-line boundary.
        ed.set_cursor_quiet(0, 16);
        assert_eq!(sentence_boundary(&ed, true), Some((1, 0)));
        // Backward from start of "Second one." → buffer start.
        assert_eq!(sentence_boundary(&ed, false), Some((0, 0)));
        // Backward from buffer start → no previous boundary.
        ed.set_cursor_quiet(0, 0);
        assert_eq!(sentence_boundary(&ed, false), None);
    }

    /// `it` / `at` on a small nested tag buffer. The whole buffer fits in
    /// the window, so the result must match the full scan exactly.
    #[test]
    fn tag_text_object_exact_range_nested() {
        let mut ed = make_editor("<div>\n  <p>Hello</p>\n</div>");
        ed.set_cursor_quiet(1, 5); // the 'H' of "Hello"
        assert_eq!(tag_text_object(&ed, true), Some(((1, 5), (1, 10))));
        assert_eq!(tag_text_object(&ed, false), Some(((1, 2), (1, 14))));
    }

    /// `it` / `at` edge cases: cursor before any tag (forward fallback),
    /// inside a single-line pair, and inside a pair spanning rows.
    #[test]
    fn tag_text_object_edge_cases() {
        // Cursor before the first tag → forward scan to the next pair.
        let mut ed = make_editor("text <b>x</b>");
        ed.set_cursor_quiet(0, 0);
        assert_eq!(tag_text_object(&ed, true), Some(((0, 8), (0, 9))));
        assert_eq!(tag_text_object(&ed, false), Some(((0, 5), (0, 13))));
        // Inside a multi-line pair: content runs from just past the open
        // tag to just before the close tag, across the newlines.
        let mut ed = make_editor("<a>\n  body\n</a>");
        ed.set_cursor_quiet(1, 3);
        assert_eq!(tag_text_object(&ed, true), Some(((0, 3), (2, 0))));
        assert_eq!(tag_text_object(&ed, false), Some(((0, 0), (2, 4))));
        // Unmatched close → no pair.
        let ed = make_editor("</a>");
        assert_eq!(tag_text_object(&ed, true), None);
    }

    // ── Differential tests: the new incremental scans vs. the old
    // full-buffer implementations, kept verbatim as test-only references.

    /// Reference: the pre-incremental full-buffer `sentence_boundary`.
    fn old_sentence_boundary<H: hjkl_engine::types::Host>(
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
            let end = end_of_buffer_pos(&lines, n_lines);
            (end > cursor).then_some(end)
        } else {
            boundaries.into_iter().rfind(|&p| p < cursor)
        }
    }

    /// Reference: the pre-incremental full-buffer `sentence_step_forward`.
    fn old_sentence_step_forward<H: hjkl_engine::types::Host>(
        ed: &Editor<hjkl_buffer::View, H>,
    ) -> SentenceStep {
        let rope = hjkl_engine::types::Query::rope(ed.buffer());
        let raw_n_lines = rope.len_lines();
        if raw_n_lines == 0 {
            return SentenceStep::AtEnd;
        }
        let lines: Vec<Vec<char>> = (0..raw_n_lines)
            .map(|r| rope_line_to_str(&rope, r).chars().collect())
            .collect();
        // Same phantom-trailing-row clamp as `sentence_boundary`.
        let n_lines = if raw_n_lines > 1 && lines[raw_n_lines - 1].is_empty() {
            raw_n_lines - 1
        } else {
            raw_n_lines
        };
        if n_lines == 0 {
            return SentenceStep::AtEnd;
        }
        let cursor = ed.cursor();
        let cursor = (cursor.0.min(n_lines - 1), cursor.1);
        if let Some(&p) = sentence_boundaries(&lines, n_lines)
            .iter()
            .find(|&&p| p > cursor)
        {
            return SentenceStep::Boundary(p);
        }
        let end = end_of_buffer_pos(&lines, n_lines);
        if end <= cursor {
            return SentenceStep::AtEnd;
        }
        // A terminator (plus any closing run and trailing whitespace) that
        // closes the buffer still ends a sentence in vim, so this landing is
        // a real boundary rather than the run-off-the-end fallback: `3)` on
        // `"One. Two."` stops at the last char, while `3)` on `"One. Two"`
        // fails.
        let mut tail = lines[n_lines - 1].as_slice();
        while tail.last().is_some_and(|c| c.is_whitespace()) {
            tail = &tail[..tail.len() - 1];
        }
        while tail.last().is_some_and(|c| is_sentence_closing(*c)) {
            tail = &tail[..tail.len() - 1];
        }
        if tail.last().is_some_and(|c| is_sentence_terminator(*c)) {
            SentenceStep::Boundary(end)
        } else {
            SentenceStep::EndOfBuffer(end)
        }
    }

    /// Cursor samples for the corpus below: every cell of every row plus
    /// a couple of positions past each row's end (the cursor column is
    /// not clamped, so past-EOL cursors are valid inputs too).
    fn cursor_samples(content: &str) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for (row, line) in content.split('\n').enumerate() {
            let len = line.chars().count();
            for col in 0..=len + 2 {
                out.push((row, col));
            }
        }
        out
    }

    /// Corpus covering the sentence-boundary rules: mid-line terminators,
    /// terminator runs, closing punctuation, trailing whitespace, EOL
    /// terminators, blank lines, all-whitespace rows, and buffers with no
    /// terminator at all. The big-buffer case (sentence boundaries further
    /// than the scan window from the cursor) is appended at run time so the
    /// window-clip fallback is exercised too.
    const CORPUS: &[&str] = &[
        "",
        "\n",
        "One.",
        "One.\n",
        "One. Two.",
        "One. Two.\n",
        "One. Two. Three!",
        "One? Two!",
        "One.  Two.",
        "One. \nTwo.",
        "One.\nTwo.",
        "One.\n\nTwo.",
        "One.\n\n\nTwo.",
        "One.   \n   Two.",
        "One.   \n \n   Two.",
        "One.) Two.",
        "One.\" Two.",
        "One.'] Two.",
        "Hello world",
        "Hello world\n",
        "  One.  Two.  ",
        "One. Two",
        "Really?! Yes.",
        "A.\nB.\nC.",
        "First sentence. Second one.\n\nThird.",
        "\n\n",
        "One.\n\n",
    ];

    fn corpus_buffers() -> Vec<String> {
        let mut bufs: Vec<String> = CORPUS.iter().map(|s| (*s).to_string()).collect();
        // A sentence with no boundary within ±200 rows of the cursor forces
        // the windowed scan onto its full-buffer fallback.
        bufs.push("x\n".repeat(300) + "One. Two.\n" + &"y\n".repeat(300));
        bufs
    }

    /// Every corpus buffer × every cursor sample × both directions: the
    /// row-by-row scan must agree with the old full-buffer scan.
    #[test]
    fn sentence_boundary_matches_full_scan_on_corpus() {
        for buf in corpus_buffers() {
            let mut ed = make_editor(&buf);
            for (row, col) in cursor_samples(&buf) {
                ed.set_cursor_quiet(row, col);
                for forward in [true, false] {
                    assert_eq!(
                        sentence_boundary(&ed, forward),
                        old_sentence_boundary(&ed, forward),
                        "buffer {:?} cursor ({row},{col}) forward={forward}",
                        buf
                    );
                }
            }
        }
    }

    /// The same corpus: the windowed sentence scan must produce the same
    /// ranges as the whole-buffer scan, for `is` and `as`.
    #[test]
    fn sentence_text_object_matches_full_scan_on_corpus() {
        for buf in corpus_buffers() {
            let mut ed = make_editor(&buf);
            for (row, col) in cursor_samples(&buf) {
                ed.set_cursor_quiet(row, col);
                for inner in [true, false] {
                    assert_eq!(
                        sentence_text_object(&ed, inner, 1),
                        sentence_text_object_full(&ed, inner, 1),
                        "buffer {:?} cursor ({row},{col}) inner={inner}",
                        buf
                    );
                }
            }
        }
    }

    /// The same corpus: the row-by-row `)` classifier must agree with the
    /// old full-buffer classification at every cursor sample.
    #[test]
    fn sentence_step_forward_matches_full_scan_on_corpus() {
        for buf in corpus_buffers() {
            let mut ed = make_editor(&buf);
            for (row, col) in cursor_samples(&buf) {
                ed.set_cursor_quiet(row, col);
                assert_eq!(
                    sentence_step_forward(&ed),
                    old_sentence_step_forward(&ed),
                    "buffer {:?} cursor ({row},{col})",
                    buf
                );
            }
        }
    }
}
