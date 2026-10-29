//! Vim FSM: text object.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_engine::abbrev::{Abbrev, AbbrevKind, AbbrevTrigger};
use hjkl_vim_types::{RangeKind, TextObject};

use hjkl_engine::rope_util::{rope_line_to_str, rope_to_lines_vec};

use super::vim_buf::{VimBuf, is_vim_white, walk_vim_buf, with_vim_buf};
use super::word_object::word_text_object;
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
            quote_text_object(ed, q, inner, count).map(|(s, e)| (s, e, RangeKind::Exclusive))
        }
        TextObject::Bracket(open) => bracket_text_object(ed, open, inner, count),
        TextObject::Paragraph => {
            paragraph_text_object(ed, inner, count).map(|(s, e)| (s, e, RangeKind::Linewise))
        }
        // A count walks outward as Visual mode's does (vim's
        // `current_tagblock`), whose inclusive end is made exclusive here.
        TextObject::XmlTag if count > 1 => {
            tag_visual(ed, ed.cursor(), inner, count).map(|(s, (row, col))| {
                let len = buf_line(ed.buffer(), row).map_or(0, |l| l.chars().count());
                let end = if col < len {
                    (row, col + 1)
                } else {
                    (row + 1, 0)
                };
                (s, end, RangeKind::Exclusive)
            })
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

/// A row the trailing-whitespace walk passes *through* without stopping:
/// non-blank and holding only whitespace.
fn row_skippable(line: &[char]) -> bool {
    !line.is_empty() && line.iter().all(|&c| c.is_whitespace())
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
    if inner && let Some(rung) = blank_run_rung(&starts, &lines, cursor) {
        starts.push(rung);
        starts.sort_unstable();
    }
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

/// The one whitespace object a reverse `is` walk can stop on, if any.
///
/// A run of BLANK rows between two sentences is an object in its own right
/// (`:h is` — "a sentence, or, when the cursor is in white space, that white
/// space"), so on `"aaa.\n   \nbbb.\nccc.\n"` with the block cursor inside
/// `bbb.` the ladder is `bbb.` / the blank row / `aaa.`: `<C-v>k2is` stops on
/// the blank row and only `3is` reaches `aaa.`. Vim stops on the FIRST row of
/// the run, at column 0.
///
/// Only ONE run is ever a rung: the one the cursor stands in, else the one
/// directly above the sentence the cursor is in. A run further up the buffer
/// is not offered — from `ccc.` in `"aaa.\n   \nbbb.\nccc.\nxxx."` the
/// third `is` reaches `aaa.`, not the blank row above `bbb.` (measured
/// against nvim 0.12.5). A bare line break between two adjacent sentences is
/// never such an object either.
///
/// `as` gets none of this: it has already swallowed the whitespace into the
/// sentence that trails it.
fn blank_run_rung(starts: &[Pos], lines: &[Vec<char>], cursor: Pos) -> Option<Pos> {
    let index = starts.iter().rposition(|&start| start <= cursor)?;
    let standing_in = starts
        .get(index + 1)
        .and_then(|&next| blank_gap(lines, starts[index], next))
        .filter(|&(row, _)| row <= cursor.0);
    standing_in.or_else(|| {
        let prev = *starts.get(index.checked_sub(1)?)?;
        blank_gap(lines, prev, starts[index])
    })
}

/// First row of the run of blank rows between two sentence starts, at column
/// 0. `None` when they sit on adjacent rows, or when a row between them holds
/// a non-blank.
fn blank_gap(lines: &[Vec<char>], prev: Pos, next: Pos) -> Option<Pos> {
    let first = prev.0 + 1;
    (first < next.0
        && lines[first..next.0]
            .iter()
            .all(|line| line.iter().all(|ch| ch.is_whitespace())))
    .then_some((first, 0))
}

/// Every valid sentence-boundary landing position within `lines[..n_lines]`,
/// in ascending order (deduplicated). Always includes `(0, 0)`. Used by
/// [`reverse_visual_block_sentence_landing`].
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

/// `[count])` under an operator: vim's `findsent` forward from the cursor,
/// then its `adjust_cursor` — a landing on the NUL past a line's last
/// character steps back onto that character and makes the motion inclusive,
/// which is how `d)` in a buffer's last sentence reaches its end. The flag
/// says whether that happened. `None` is vim's `FAIL`, a count the sentences
/// left cannot use up (`2d)` on the last character).
pub fn sentence_forward_for_operator<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    count: usize,
) -> Option<(Pos, bool)> {
    let found = with_vim_buf(ed, |buf, cursor| {
        let pos = buf.findsent(cursor, true, count.max(1))?;
        if pos.1 > 0 && pos.1 == buf.len(pos.0) {
            Some(((pos.0, pos.1 - 1), true))
        } else {
            Some((pos, false))
        }
    });
    // No characters at all: nothing to move over, and `)` stays put without
    // failing (`c)` on an empty buffer still enters Insert).
    found.unwrap_or_else(|| Some((ed.cursor(), false)))
}

/// `)` — vim's `findsent` forward, `count` sentences. `None` when a
/// repetition has nowhere left to go, which leaves the cursor put.
pub fn sentence_forward<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    count: usize,
) -> Option<Pos> {
    walk_vim_buf(ed, |buf, cursor| buf.findsent(cursor, true, count.max(1)))
}

/// `(` — vim's `findsent` backward, `count` sentences. `None` when a
/// repetition has nowhere left to go, which leaves the cursor put.
pub fn sentence_backward<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    count: usize,
) -> Option<Pos> {
    walk_vim_buf(ed, |buf, cursor| buf.findsent(cursor, false, count.max(1)))
}

/// `is` / `as` — sentence text object: a port of vim's `current_sent`
/// (`textobject.c`), outside the Visual-extend path. `as` takes the white
/// space after the sentence, or the white space before it when there is
/// none after. Returns `(start, end)` with `end` exclusive, as every text
/// object does. `None` only for a buffer without a single character.
///
/// The walk reads lines on demand (`VimBuf`), so it costs the distance it
/// covers, not the buffer's size.
pub fn sentence_text_object<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    inner: bool,
    count: usize,
) -> Option<((usize, usize), (usize, usize))> {
    with_vim_buf(ed, |buf, cursor| {
        buf.current_sent(cursor, inner, count.max(1))
    })
}

/// What a Visual-mode text object ported from vim (`is` / `as`, `iw` / `aw`)
/// does to the selection.
#[derive(Debug, PartialEq, Eq)]
pub enum VisualTextObject {
    /// Select the object afresh, charwise: anchor and cursor, inclusive.
    Select(Pos, Pos),
    /// Extend the selection: the cursor lands here, the anchor and the
    /// Visual mode stay.
    Extend(Pos),
}

/// Visual-mode `is` / `as` with the selection anchored at `anchor`: vim's
/// `current_sent` with Visual active (see `VimBuf::current_sent_visual`).
pub fn sentence_visual<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    anchor: Pos,
    inner: bool,
    count: usize,
) -> Option<VisualTextObject> {
    with_vim_buf(ed, |buf, cursor| {
        buf.current_sent_visual(cursor, anchor, inner, count.max(1))
    })
}

/// The object `current_sent` found before the Visual / operator split: its
/// first and last cells, and where the blank skip from the cursor stopped.
struct SentenceFound {
    start: Pos,
    last: Pos,
    skipped: Pos,
}

/// vim's sentence punctuation: what `findsent` backs up over.
fn is_sentence_punct(c: char) -> bool {
    is_sentence_terminator(c) || is_sentence_closing(c)
}

/// vim's sentence code, over [`VimBuf`].
impl VimBuf<'_> {
    /// vim `startPS(lnum, NUL, false)`: an empty line or one starting with a
    /// form feed. The nroff `'paragraphs'` / `'sections'` macros (`.PP`, …)
    /// are not recognised.
    fn start_ps(&self, row: usize) -> bool {
        self.len(row) == 0 || self.gchar((row, 0)) == Some('\x0c')
    }

    /// vim `find_first_blank`: back over the blanks before `p`.
    fn find_first_blank(&self, p: &mut Pos) {
        while self.decl(p) != -1 {
            if !self.gchar(*p).is_some_and(is_vim_white) {
                self.incl(p);
                break;
            }
        }
    }

    /// Whether `p` sits in its line's leading white space.
    fn in_leading_white(&self, p: Pos) -> bool {
        (0..p.1).all(|col| self.gchar((p.0, col)).is_some_and(is_vim_white))
    }

    /// vim `findsent` with the default `'cpoptions'` (no `J`): `count`
    /// sentence starts forward or back from `pos`. `None` is vim's `FAIL`,
    /// a count that runs off the buffer.
    fn findsent(&self, mut pos: Pos, forward: bool, mut count: usize) -> Option<Pos> {
        let step = |p: &mut Pos| if forward { self.incl(p) } else { self.decl(p) };
        let mut noskip = false;
        while count > 0 {
            count -= 1;
            let prev_pos = pos;
            'found: {
                if self.gchar(pos).is_none() {
                    // On an empty line: skip up to a non-empty one.
                    while step(&mut pos) != -1 && self.gchar(pos).is_none() {}
                    if forward {
                        break 'found;
                    }
                } else if forward && pos.1 == 0 && self.start_ps(pos.0) {
                    if pos.0 + 1 == self.n_lines {
                        return None;
                    }
                    pos.0 += 1;
                    break 'found;
                } else if !forward {
                    self.decl(&mut pos);
                }

                // Back to the previous non-white, non-punctuation character.
                let mut found_dot = false;
                while let Some(c) = self.gchar(pos)
                    && (is_vim_white(c) || is_sentence_punct(c))
                {
                    let mut tpos = pos;
                    if self.decl(&mut tpos) == -1 || (self.len(tpos.0) == 0 && forward) {
                        break;
                    }
                    if found_dot {
                        break;
                    }
                    if is_sentence_terminator(c) {
                        found_dot = true;
                    }
                    if is_sentence_closing(c) && !self.gchar(tpos).is_some_and(is_sentence_punct) {
                        break;
                    }
                    self.decl(&mut pos);
                }

                // Find the end of the sentence.
                let start_row = pos.0;
                loop {
                    let c = self.gchar(pos);
                    if c.is_none() || (pos.1 == 0 && self.start_ps(pos.0)) {
                        if !forward && pos.0 != start_row {
                            pos.0 += 1;
                        }
                        break;
                    }
                    if c.is_some_and(is_sentence_terminator) {
                        let mut tpos = pos;
                        // `None`: ran off the buffer; `Some(None)`: a NUL.
                        let after = loop {
                            if self.inc(&mut tpos) == -1 {
                                break None;
                            }
                            let c = self.gchar(tpos);
                            if !c.is_some_and(is_sentence_closing) {
                                break Some(c);
                            }
                        };
                        if matches!(after, None | Some(None) | Some(Some(' ' | '\t'))) {
                            pos = tpos;
                            if self.gchar(pos).is_none() {
                                self.inc(&mut pos);
                            }
                            break;
                        }
                    }
                    if step(&mut pos) == -1 {
                        if count > 0 {
                            return None;
                        }
                        noskip = true;
                        break;
                    }
                }
            }
            // Skip white space.
            while !noskip && self.gchar(pos).is_some_and(is_vim_white) {
                if self.incl(&mut pos) == -1 {
                    break;
                }
            }
            if pos == prev_pos {
                // Did not move: advance one character and try again.
                if step(&mut pos) == -1 {
                    if count > 0 {
                        return None;
                    }
                    break;
                }
                count += 1;
            }
        }
        Some(pos)
    }

    /// vim `current_sent` for an operator: the `is` / `as` range from
    /// `cursor` as `(start, exclusive end)`.
    fn current_sent(&self, cursor: Pos, inner: bool, count: usize) -> (Pos, Pos) {
        let found = self.current_sent_found(cursor, inner, count);
        let (start, mut cur) = (found.start, found.last);
        // The exclusive end is one past `last`, onto the next line when it
        // ends one (vim includes that line break). When that lands at the
        // start of a line and the object did not begin in indentation, the
        // operator's column-zero rule backs the break out.
        self.incl(&mut cur);
        let end = if cur.1 == 0 && cur.0 > 0 && !self.in_leading_white(start) {
            (cur.0 - 1, self.len(cur.0 - 1))
        } else {
            cur
        };
        (start, end)
    }

    /// vim `current_sent` with Visual active, `anchor` being where the
    /// selection started. A selection bigger than one character is extended
    /// by sentences, and so is one whose object comes out a single cell
    /// (vim's "avoid getting stuck" jump into the extend path); otherwise the
    /// object is selected.
    fn current_sent_visual(
        &self,
        cursor: Pos,
        anchor: Pos,
        inner: bool,
        count: usize,
    ) -> VisualTextObject {
        if anchor != cursor {
            let next = self.findsent(cursor, true, 1).unwrap_or(cursor);
            let landing = self.current_sent_extend(cursor, cursor, next, anchor, inner, count);
            return VisualTextObject::Extend(landing);
        }
        let found = self.current_sent_found(cursor, inner, count);
        if found.start == found.last {
            let landing =
                self.current_sent_extend(cursor, found.skipped, found.last, anchor, inner, count);
            return VisualTextObject::Extend(landing);
        }
        VisualTextObject::Select(found.start, found.last)
    }

    /// The body of vim `current_sent` outside the extend path: the object's
    /// first and last cells, plus where the blank skip from `cursor` stopped
    /// (the extend path picks up from there).
    fn current_sent_found(&self, cursor: Pos, inner: bool, count: usize) -> SentenceFound {
        let mut start = cursor;
        let mut pos = cursor;
        let mut cur = self.findsent(cursor, true, 1).unwrap_or(cursor);
        // Cursor on white space just before the next sentence: the blank run
        // itself is the first unit.
        while self.gchar(pos).is_some_and(is_vim_white) {
            if self.incl(&mut pos) == -1 {
                break;
            }
        }
        let start_blank = pos == cur;
        if start_blank {
            self.find_first_blank(&mut start);
        } else {
            cur = self.findsent(cur, false, 1).unwrap_or(cur);
            start = cur;
        }
        let ncount = if !inner {
            count * 2
        } else {
            count - usize::from(start_blank)
        };
        if ncount > 0 {
            cur = self.findsent_forward(cur, ncount, true);
        } else {
            self.decl(&mut cur);
        }
        if !inner {
            // The blank in front is included: exclude the blanks at the end.
            // No trailing blanks: include the leading ones instead.
            if start_blank {
                self.find_first_blank(&mut cur);
                if self.gchar(cur).is_some_and(is_vim_white) {
                    self.decl(&mut cur);
                }
            } else if !self.gchar(cur).is_some_and(is_vim_white) {
                self.find_first_blank(&mut start);
            }
        }
        SentenceFound {
            start,
            last: cur,
            skipped: pos,
        }
    }

    /// vim `current_sent`'s Visual-extend path from `start_pos` (where the
    /// cursor was), with `pos` and `cur` as vim's walk left them: on a real
    /// extend, `start_pos` and the next sentence start; on the jump from a
    /// single-cell object, the blank skip's stop and that cell. Returns the
    /// new cursor; the anchor stays.
    fn current_sent_extend(
        &self,
        start_pos: Pos,
        mut pos: Pos,
        mut cur: Pos,
        anchor: Pos,
        inner: bool,
        count: usize,
    ) -> Pos {
        let count = if inner { count } else { count * 2 };
        let not_white = |p: Pos| !self.gchar(p).is_some_and(is_vim_white);
        if start_pos < anchor {
            // Cursor at the start of the selection: in the white space before
            // a sentence, in a sentence or just after one, or at its start.
            let mut at_start_sent = true;
            self.decl(&mut pos);
            while pos < cur {
                if not_white(pos) {
                    at_start_sent = false;
                    break;
                }
                self.incl(&mut pos);
            }
            if !at_start_sent {
                cur = self.findsent(cur, false, 1).unwrap_or(cur);
                if cur == start_pos {
                    at_start_sent = true;
                } else {
                    // Inside a sentence: go to its end (the next start).
                    cur = self.findsent(cur, true, 1).unwrap_or(cur);
                }
            }
            for _ in 0..count {
                if at_start_sent {
                    self.find_first_blank(&mut cur);
                }
                if !at_start_sent || (inner && not_white(cur)) {
                    cur = self.findsent(cur, false, 1).unwrap_or(cur);
                }
                at_start_sent = !at_start_sent;
            }
            cur
        } else {
            // Cursor at the end of the selection: just before a sentence,
            // just before or in the white space before one, or in one.
            self.incl(&mut pos);
            let mut at_start_sent = true;
            if pos != cur {
                at_start_sent = false;
                while pos < cur {
                    if not_white(pos) {
                        at_start_sent = true;
                        break;
                    }
                    self.incl(&mut pos);
                }
                cur = if at_start_sent {
                    self.findsent(cur, false, 1).unwrap_or(cur)
                } else {
                    start_pos
                };
            }
            self.findsent_forward(cur, count, at_start_sent)
        }
    }

    /// vim `findsent_forward`: `count / 2` sentences and `count / 2`
    /// separating white space runs, alternating.
    fn findsent_forward(&self, mut pos: Pos, count: usize, mut at_start_sent: bool) -> Pos {
        for left in (0..count).rev() {
            if let Some(p) = self.findsent(pos, true, 1) {
                pos = p;
            }
            if at_start_sent {
                self.find_first_blank(&mut pos);
            }
            if left == 0 || at_start_sent {
                self.decl(&mut pos);
            }
            at_start_sent = !at_start_sent;
        }
        pos
    }
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

    // The innermost pair whose whole span holds the cursor — ON the open or
    // close tag included, as vim `it`/`at` operate on the tag under the
    // cursor; nested pairs, so the shortest is the innermost. With none, the
    // first pair (in close order) starting at or after the cursor: a forward
    // scan, targets.vim-style.
    let pairs = tag_pairs(&chars);
    let (open_start, content_start, content_end, close_end) = pairs
        .iter()
        .copied()
        .filter(|&(os, _, _, ce)| cursor_idx >= os && cursor_idx < ce)
        .min_by_key(|&(os, _, _, ce)| ce - os)
        .or_else(|| pairs.iter().copied().find(|&(os, ..)| os >= cursor_idx))?;
    if inner {
        Some((idx_to_pos(content_start), idx_to_pos(content_end)))
    } else {
        Some((idx_to_pos(open_start), idx_to_pos(close_end)))
    }
}

/// Every complete tag pair in `chars`, in the order their closes appear, as
/// `(open_start, content_start, content_end, close_end)` flat indices:
/// `content_start` is just past the open tag's `>`, `content_end` is the
/// close tag's `<`, `close_end` just past its `>`. Comments, declarations
/// (`<!…>`, `<?…>`) and self-closing tags pair with nothing.
fn tag_pairs(chars: &[char]) -> Vec<(usize, usize, usize, usize)> {
    let mut stack: Vec<(usize, usize, String)> = Vec::new(); // (open_start, content_start, name)
    let mut pairs = Vec::new();
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
                pairs.push((open_start, content_start, i, close_end));
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
    pairs
}

/// Visual-mode `it` / `at` from `anchor` to the cursor: vim's
/// `current_tagblock` with Visual active. From the selection's start it takes
/// the `count`-th tag pair around it, outward, not counting a pair that opens
/// right there; a pair whose close tag starts before the selection ends is
/// skipped for the next one out. An inner object that would select exactly the current
/// selection selects the whole pair instead (vim retries with `do_include`).
/// Returns the new selection's anchor and cursor, which vim makes charwise,
/// or `None` when no pair qualifies and the selection stays.
pub fn tag_visual<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    anchor: Pos,
    inner: bool,
    count: usize,
) -> Option<(Pos, Pos)> {
    let rope = hjkl_engine::types::Query::rope(ed.buffer());
    let line_lens: Vec<usize> = (0..rope.len_lines())
        .map(|r| rope_line_to_str(&rope, r).chars().count())
        .collect();
    let pos_to_idx = |pos: Pos| -> usize {
        let idx: usize = line_lens.iter().take(pos.0).map(|&len| len + 1).sum();
        idx + pos.1
    };
    let idx_to_pos = |mut idx: usize| -> Pos {
        for (r, &len) in line_lens.iter().enumerate() {
            if idx <= len {
                return (r, idx);
            }
            idx -= len + 1;
        }
        let last = line_lens.len().saturating_sub(1);
        (last, line_lens.get(last).copied().unwrap_or(0))
    };
    let chars: Vec<char> = rope.chars().collect();
    let mut pairs = tag_pairs(&chars);
    // Innermost first: a pair around another opens after it.
    pairs.sort_by_key(|&(os, ..)| std::cmp::Reverse(os));
    let cursor = pos_to_idx(ed.cursor());
    let anchor = pos_to_idx(anchor);
    let (old_start, old_end) = (anchor.min(cursor), anchor.max(cursor));
    // vim's backward search for an unclosed open tag, which does not match
    // at its start: a pair is around `at` when it opens before it and its
    // close tag starts at or after it.
    let around = |at: usize| {
        pairs
            .iter()
            .copied()
            .filter(move |&(os, _, ce, _)| os < at && at <= ce)
    };
    let mut include = !inner;
    let mut from = old_start;
    let mut nth = count.max(1);
    loop {
        let (os, cs, ce, end) = around(from).nth(nth - 1)?;
        // Its close tag starts before the selection ends: try the next pair
        // out, as vim's does when the matching end is before the old end.
        if ce < old_end {
            from = os;
            nth = 1;
            continue;
        }
        let (start, last) = if include {
            (os, end - 1)
        } else {
            (cs, ce.saturating_sub(1).max(cs))
        };
        if !include && (start, last) == (old_start, old_end) {
            include = true;
            from = old_start;
            nth = count.max(1);
            continue;
        }
        return Some((idx_to_pos(start), idx_to_pos(last)));
    }
}

/// Rows above and below the cursor that the windowed tag scan covers
/// before falling back to the whole buffer.
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
/// `i<q>` / `a<q>`. With a `count` of 2 or more, `i<q>` takes the quotes
/// themselves but none of the white space `a<q>` adds (`:h i"`).
pub fn quote_text_object<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    q: char,
    inner: bool,
    count: usize,
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
    // vim's `current_quote`: on a quote, the quotes pair up from the start of
    // the line; anywhere else the nearest quote before the cursor opens (the
    // first one after it when there is none) and the next quote closes, so
    // the `.` in `"ab"."cd"` sits inside `"."`.
    let (open, close) = if chars.get(col) == Some(&q) {
        let &[open, close] = positions
            .as_chunks::<2>()
            .0
            .iter()
            .find(|[open, close]| (*open..=*close).contains(&col))?;
        (open, close)
    } else {
        let open = positions
            .iter()
            .rev()
            .find(|&&p| p < col)
            .or_else(|| positions.iter().find(|&&p| p > col))
            .copied()?;
        let close = positions.iter().find(|&&p| p > open).copied()?;
        (open, close)
    };
    // End columns are *exclusive* — one past the last character to act on.
    if inner && count >= 2 {
        Some(((row, open), (row, close + 1)))
    } else if inner {
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
    let close = closing_bracket(open)?;
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
        match find_open_before(lines, (row, col), open, close) {
            Some(open_pos) => (open_pos, (row, col)),
            // An unmatched close under the cursor: like any cursor outside a
            // pair, take the next pair forward (`di(` on the `)` of
            // `")x (ab)"` empties `(ab)` in nvim 0.12.5).
            None => {
                let open_pos = find_next_open(lines, row, col, open)?;
                let close_pos = find_close_bracket(lines, open_pos.0, open_pos.1 + 1, open, close)?;
                (open_pos, close_pos)
            }
        }
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
            let oo = find_open_before(lines, op, open, close)?;
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
/// The bracket that closes `open` for the bracket text objects.
fn closing_bracket(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '<' => Some('>'),
        _ => None,
    }
}

/// The unmatched `open` strictly before `pos`, the way vim's `findmatch`
/// steps off its start position before it looks.
fn find_open_before(lines: &[String], pos: Pos, open: char, close: char) -> Option<Pos> {
    if pos.1 > 0 {
        find_open_bracket(lines, pos.0, pos.1 - 1, open, close)
    } else if pos.0 > 0 {
        let pr = pos.0 - 1;
        let pc = lines[pr].chars().count().saturating_sub(1);
        find_open_bracket(lines, pr, pc, open, close)
    } else {
        None
    }
}

/// The next `open` strictly after `pos` that no earlier `close` on the way
/// pairs with: vim's `findmatchlimit` searching forward (`FM_FORWARD`), where
/// a `close` passed raises the depth an `open` must bring back to zero.
fn find_open_after(lines: &[String], pos: Pos, open: char, close: char) -> Option<Pos> {
    let mut depth = 0usize;
    let mut col = pos.1 + 1;
    for (row, line) in lines.iter().enumerate().skip(pos.0) {
        for (c, ch) in line.chars().enumerate().skip(col) {
            if ch == close {
                depth += 1;
            } else if ch == open {
                if depth == 0 {
                    return Some((row, c));
                }
                depth -= 1;
            }
        }
        col = 0;
    }
    None
}

/// Visual-mode `i(` / `a(` and the other bracket objects on a selection
/// bigger than one cell, from `anchor` to the cursor: vim's `current_block`
/// with Visual active. The search starts from the selection's low end
/// without stepping off a bracket under it (so a selection starting on the
/// `(` finds no enclosing block); with none, it takes the next `(` after
/// that end instead, as vim's forward `findmatchlimit` does. An inner object
/// that would not be bigger than the selection grows to the next enclosing
/// block. Returns the new selection's anchor and cursor, which vim makes
/// charwise.
pub fn bracket_visual<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    anchor: Pos,
    open: char,
    inner: bool,
    count: usize,
) -> Option<(Pos, Pos)> {
    let close = closing_bracket(open)?;
    let lines = rope_to_lines_vec(&hjkl_engine::types::Query::rope(ed.buffer()));
    let (old_start, old_end) = super::order(anchor, ed.cursor());
    let enclosed = find_open_before(&lines, old_start, open, close).is_some();
    let mut open_pos = old_start;
    for _ in 0..count.max(1) {
        open_pos = if enclosed {
            find_open_before(&lines, open_pos, open, close)?
        } else {
            find_open_after(&lines, open_pos, open, close)?
        };
    }
    let mut close_pos = find_close_bracket(&lines, open_pos.0, open_pos.1 + 1, open, close)?;
    if !inner {
        return Some((open_pos, close_pos));
    }
    with_vim_buf(ed, |buf, _| {
        loop {
            let mut start = open_pos;
            buf.incl(&mut start);
            let mut end = close_pos;
            let mut sol = end.1 == 0;
            buf.decl(&mut end);
            // A close preceded only by indent: back over that indent.
            while in_indent(buf, end) {
                sol = true;
                if buf.decl(&mut end) != 0 {
                    break;
                }
            }
            if start >= old_start && end <= old_end {
                let mut from = old_start;
                buf.decl(&mut from);
                open_pos = find_open_before(&lines, from, open, close)?;
                close_pos = find_close_bracket(&lines, open_pos.0, open_pos.1 + 1, open, close)?;
                continue;
            }
            // The close started its line: take the line break before it.
            if sol && buf.gchar(end).is_some() {
                buf.inc(&mut end);
            }
            return Some((start, end));
        }
    })
    .flatten()
}

/// vim `inindent(1)`: whether `p` sits in its line's leading white space.
fn in_indent(buf: &VimBuf<'_>, p: Pos) -> bool {
    (0..=p.1).all(|col| buf.gchar((p.0, col)).is_some_and(is_vim_white))
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
    // `VimBuf::new` (`vim_buf.rs`), which applies the identical
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
    if !inner {
        // `ap` ("around paragraph"): faithful port of nvim's `current_par()`
        // operator path for `include == true` (src/nvim/textobject.c, nvim
        // 0.12.5). Line numbers below are 1-based to mirror nvim, then shifted
        // to hjkl's 0-based rows at the end.
        //
        // The count drives a UNIFORM loop over whole paragraph units — there is
        // no separate "first unit" step. `end == n_lines` FAILs at the TOP of
        // every iteration, so a unit with nowhere to start (a paragraph whose
        // trailing blank run reaches EOF, or a counted `Nap` that runs out of
        // paragraphs) is a no-op rather than a best-effort clamp. When the
        // cursor starts on a blank run, the LAST unit ends at its paragraph
        // instead of also taking the following blank run.
        //
        // `startPS()` (nroff-macro / form-feed paragraph starts) is ignored
        // here exactly as the `ip` path below ignores it: hjkl models paragraph
        // boundaries with `is_blank` (empty or whitespace-only lines) only.
        let white = |lnum: usize| is_blank(lnum - 1);
        let mut start = row + 1;
        let white_in_front = white(start);
        // Move back to the start of the paragraph or the white run.
        while start > 1 {
            if white_in_front {
                if !white(start - 1) {
                    break;
                }
            } else if white(start - 1) {
                break;
            }
            start -= 1;
        }
        // Move past the end of any white lines, then step back one so `end`
        // sits on the last line of the current (white or non-white) run.
        let mut end = start;
        while end <= n_lines && white(end) {
            end += 1;
        }
        end -= 1;
        let mut i = count;
        while i > 0 {
            i -= 1;
            if end == n_lines {
                return None;
            }
            // Skip to the end of the paragraph.
            end += 1;
            while end < n_lines && !white(end + 1) {
                end += 1;
            }
            // Starting on a blank run, the LAST unit stops at the paragraph
            // itself — it does not also take the following blank run.
            if i == 0 && white_in_front {
                break;
            }
            // Skip to the end of the white lines after the paragraph.
            while end < n_lines && white(end + 1) {
                end += 1;
            }
        }
        // When the paragraph has no trailing blank lines, take the leading
        // blank lines instead.
        if !white_in_front && !white(end) {
            while start > 1 && white(start - 1) {
                start -= 1;
            }
        }
        let end_col = rope_line_to_str(&rope, end - 1).chars().count();
        return Some(((start - 1, 0), (end - 1, end_col)));
    }

    // `ip` ("inner paragraph"): the cursor's same-blankness run.
    let mut top = row;
    let mut bot = row;
    if is_blank(row) {
        // `:h ip` on a blank line selects the blank-line RUN (not a no-op).
        while top > 0 && is_blank(top - 1) {
            top -= 1;
        }
        while bot + 1 < n_lines && is_blank(bot + 1) {
            bot += 1;
        }
    } else {
        while top > 0 && !is_blank(top - 1) {
            top -= 1;
        }
        while bot + 1 < n_lines && !is_blank(bot + 1) {
            bot += 1;
        }
    }
    // `Nip` extends across `count - 1` further same-blankness runs,
    // alternating paragraph → blank gap → paragraph ….
    let mut rem = count - 1;
    while rem > 0 && bot + 1 < n_lines {
        let blank_next = is_blank(bot + 1);
        bot += 1;
        while bot + 1 < n_lines && is_blank(bot + 1) == blank_next {
            bot += 1;
        }
        rem -= 1;
    }
    // When the buffer ends before all `count - 1` further runs could be
    // consumed, `rem` is still positive: nvim treats the counted object as
    // FAILED (a no-op) rather than best-effort extending to the whole buffer.
    // Returning `None` propagates to a no-op in both the operator and visual
    // paths (see `text_object_range`).
    if rem > 0 {
        return None;
    }
    let end_col = rope_line_to_str(&rope, bot).chars().count();
    Some(((top, 0), (bot, end_col)))
}

/// Landing row for `ip` / `ap` extending a Visual selection — vim's
/// `current_par` "extend" path (`textobject.c`): the anchor and visual mode
/// stay untouched and, `count` times, the cursor walks one same-blankness
/// run past itself, away from the anchor (forward when the cursor is on the
/// anchor's row), landing at the run's end on column 0. `ap`
/// (`inner == false`) continues with a second run of the opposite
/// blankness, mirroring `current_par`'s `include` loop. A repetition that
/// starts on the buffer's first / last line is vim's `FAIL`: the walk stops
/// there, and `None` means it never moved (a no-op, not a collapse).
/// Measured against neovim 0.12.4: `<C-v>jip` on `"one\n\ntwo\n"` lands
/// the cursor on row 2 with the block spanning rows 0-2 at column 0.
pub fn paragraph_extend_landing<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    cursor_row: usize,
    anchor_row: usize,
    inner: bool,
    count: usize,
) -> Option<usize> {
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
    let is_blank = |r: isize| rope_line_to_str(&rope, r as usize).trim().is_empty();
    // vim's `current_par` treats the first / last line as the walk's edge:
    // no run steps past it, and a repetition cannot start from it.
    let boundary = if dir < 0 { 0 } else { n_lines as isize - 1 };
    let mut land = cursor_row as isize;
    for _ in 0..count.max(1) {
        if land == boundary {
            break;
        }
        let mut prev_blank: Option<bool> = None;
        for _ in 0..2 {
            land += dir;
            let blank0 = is_blank(land);
            if prev_blank == Some(blank0) {
                land -= dir;
                break;
            }
            while land != boundary && is_blank(land + dir) == blank0 {
                land += dir;
            }
            if inner || land == boundary {
                break;
            }
            prev_blank = Some(blank0);
        }
    }
    (land != cursor_row as isize).then_some(land as usize)
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

    /// `is` / `as` on the second sentence of a two-sentence paragraph, with
    /// a blank line (paragraph break) after it. Pins the exact charwise
    /// ranges (nvim 0.12.5: `yas` yanks `" Second one."`).
    #[test]
    fn sentence_text_object_is_exact_range_mid_paragraph() {
        let mut ed = make_editor("First sentence. Second one.\n\nThird.");
        ed.set_cursor_quiet(0, 16); // start of "Second one."
        assert_eq!(
            sentence_text_object(&ed, true, 1),
            Some(((0, 16), (0, 27))),
            "is must span exactly \"Second one.\" (exclusive end after the '.')"
        );
        // `as` adds trailing white space, but there is none before the
        // paragraph break — so, as vim's `current_sent` does, it takes the
        // white space BEFORE the sentence instead.
        assert_eq!(
            sentence_text_object(&ed, false, 1),
            Some(((0, 15), (0, 27)))
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
}
