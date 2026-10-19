//! The `iw` / `aw` / `iW` / `aW` word text objects: vim's `current_word`
//! (`textobject.c`) and the word motions it walks with (`fwd_word`,
//! `bck_word`, `end_word`, `bckend_word`), ported over [`VimBuf`]. vim's
//! closed-fold steps (`hasFolding`) are not ported.

use super::text_object::{Pos, VisualTextObject, is_wordchar};
use super::vim_buf::{VimBuf, with_vim_buf};
use hjkl_engine::Editor;

/// `iw` / `aw` (`big`: `iW` / `aW`) for an operator: `(start, end)` with
/// `end` exclusive. `None` when vim's `current_word` fails, and for a buffer
/// without a single character.
pub fn word_text_object<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    inner: bool,
    big: bool,
    count: usize,
) -> Option<(Pos, Pos)> {
    with_vim_buf(ed, |buf, cursor| {
        let words = Words { buf, big };
        let (found, ok) = words.current_word(cursor, None, !inner, count.max(1));
        ok.then(|| (found.start, words.exclusive_end(&found)))
    })
    .flatten()
}

/// Where an operator's `iw` / `aw` that FAILs leaves the cursor: vim's
/// `current_word` moves it as it walks, so `d9aw` with fewer words cancels
/// the delete but still moves the cursor. `None` when the object succeeds.
pub fn word_object_stop<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    inner: bool,
    big: bool,
    count: usize,
) -> Option<Pos> {
    with_vim_buf(ed, |buf, cursor| {
        let words = Words { buf, big };
        let (found, ok) = words.current_word(cursor, None, !inner, count.max(1));
        (!ok).then_some(found.cursor)
    })
    .flatten()
}

/// Visual-mode `iw` / `aw` with the selection anchored at `anchor`: vim's
/// `current_word` with Visual active. A one-character selection selects the
/// object afresh; a wider one extends from the cursor's end of it. vim moves
/// the cursor as it goes, so a count that runs out of words still leaves the
/// selection where the walk stopped (`v3aw` on `"a b"` ends on the `b`).
pub fn word_visual<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
    anchor: Pos,
    inner: bool,
    big: bool,
    count: usize,
) -> Option<VisualTextObject> {
    with_vim_buf(ed, |buf, cursor| {
        let words = Words { buf, big };
        let (found, _) = words.current_word(cursor, Some(anchor), !inner, count.max(1));
        if anchor == cursor {
            VisualTextObject::Select(found.start, found.cursor)
        } else {
            VisualTextObject::Extend(found.cursor)
        }
    })
}

/// What `current_word` leaves behind: the object's start (vim's `oap->start`
/// or `VIsual`), the cursor at its end, and whether that end is inclusive.
struct Found {
    start: Pos,
    cursor: Pos,
    inclusive: bool,
}

/// vim's word code for one `iw` / `iW` flavour (`cls_bigword`).
struct Words<'b, 'a> {
    buf: &'b VimBuf<'a>,
    big: bool,
}

impl Words<'_, '_> {
    /// vim `cls`: 0 for blank (or the NUL past a line's end), 1 for
    /// punctuation, 2 for a keyword character; a `W` object counts every
    /// non-blank as 1.
    fn cls(&self, p: Pos) -> u8 {
        match self.buf.gchar(p) {
            None | Some(' ' | '\t') => 0,
            Some(c) if !c.is_ascii() && c.is_whitespace() => 0,
            Some(_) if self.big => 1,
            Some(c) if is_wordchar(c) => 2,
            Some(_) => 1,
        }
    }

    fn line_empty(&self, row: usize) -> bool {
        self.buf.len(row) == 0
    }

    /// vim `oneleft` outside virtual editing: false at column 0.
    fn oneleft(p: &mut Pos) -> bool {
        if p.1 == 0 {
            return false;
        }
        p.1 -= 1;
        true
    }

    /// vim `back_in_line`: back to the start of the word or white space
    /// under `p`, within the line.
    fn back_in_line(&self, p: &mut Pos) {
        let sclass = self.cls(*p);
        while p.1 > 0 {
            self.buf.dec(p);
            if self.cls(*p) != sclass {
                self.buf.inc(p);
                break;
            }
        }
    }

    /// vim `skip_chars`: past a run of class `cclass`; true at a buffer edge.
    fn skip_chars(&self, p: &mut Pos, cclass: u8, forward: bool) -> bool {
        while self.cls(*p) == cclass {
            let r = if forward {
                self.buf.inc(p)
            } else {
                self.buf.dec(p)
            };
            if r == -1 {
                return true;
            }
        }
        false
    }

    /// vim `fwd_word`: `count` words forward; with `eol` the last word stops
    /// at the end of its line. False (`FAIL`) when `p` started on the last
    /// character of the buffer.
    fn fwd_word(&self, p: &mut Pos, mut count: usize, eol: bool) -> bool {
        while count > 0 {
            count -= 1;
            let sclass = self.cls(*p);
            let last_line = p.0 + 1 == self.buf.n_lines;
            let i = self.buf.inc(p);
            if i == -1 || (i >= 1 && last_line) {
                return false;
            }
            if i >= 1 && eol && count == 0 {
                return true;
            }
            if sclass != 0 {
                while self.cls(*p) == sclass {
                    let i = self.buf.inc(p);
                    if i == -1 || (i >= 1 && eol && count == 0) {
                        return true;
                    }
                }
            }
            // An empty line counts as a word.
            while self.cls(*p) == 0 {
                if p.1 == 0 && self.line_empty(p.0) {
                    break;
                }
                let i = self.buf.inc(p);
                if i == -1 || (i >= 1 && eol && count == 0) {
                    return true;
                }
            }
        }
        true
    }

    /// vim `bck_word`: `count` words back; with `stop`, a cursor already on
    /// a word's start moves one less. False at the start of the buffer.
    fn bck_word(&self, p: &mut Pos, mut count: usize, mut stop: bool) -> bool {
        while count > 0 {
            count -= 1;
            let sclass = self.cls(*p);
            if self.buf.dec(p) == -1 {
                return false;
            }
            'finished: {
                if !stop || sclass == self.cls(*p) || sclass == 0 {
                    while self.cls(*p) == 0 {
                        if p.1 == 0 && self.line_empty(p.0) {
                            break 'finished;
                        }
                        if self.buf.dec(p) == -1 {
                            return true;
                        }
                    }
                    if self.skip_chars(p, self.cls(*p), false) {
                        return true;
                    }
                }
                // Overshot: forward one.
                self.buf.inc(p);
            }
            stop = false;
        }
        true
    }

    /// vim `end_word`: to the end of `count` words; with `stop`, a cursor
    /// already on a word's end moves one less, and with `empty` an empty
    /// line stops it. False at the end of the buffer.
    fn end_word(&self, p: &mut Pos, mut count: usize, mut stop: bool, empty: bool) -> bool {
        while count > 0 {
            count -= 1;
            let sclass = self.cls(*p);
            if self.buf.inc(p) == -1 {
                return false;
            }
            'finished: {
                if self.cls(*p) == sclass && sclass != 0 {
                    if self.skip_chars(p, sclass, true) {
                        return false;
                    }
                } else if !stop || sclass == 0 {
                    while self.cls(*p) == 0 {
                        if p.1 == 0 && self.line_empty(p.0) && empty {
                            break 'finished;
                        }
                        if self.buf.inc(p) == -1 {
                            return false;
                        }
                    }
                    if self.skip_chars(p, self.cls(*p), true) {
                        return false;
                    }
                }
                // Overshot: back one.
                self.buf.dec(p);
            }
            stop = false;
        }
        true
    }

    /// vim `bckend_word`: back to the end of `count` previous words; with
    /// `eol`, stops at a line's end. False at the start of the buffer.
    fn bckend_word(&self, p: &mut Pos, mut count: usize, eol: bool) -> bool {
        while count > 0 {
            count -= 1;
            let sclass = self.cls(*p);
            let i = self.buf.dec(p);
            if i == -1 {
                return false;
            }
            if eol && i == 1 {
                return true;
            }
            if sclass != 0 {
                while self.cls(*p) == sclass {
                    let i = self.buf.dec(p);
                    if i == -1 || (eol && i == 1) {
                        return true;
                    }
                }
            }
            while self.cls(*p) == 0 {
                if p.1 == 0 && self.line_empty(p.0) {
                    break;
                }
                let i = self.buf.dec(p);
                if i == -1 || (eol && i == 1) {
                    return true;
                }
            }
        }
        true
    }

    /// vim `current_word`: `count` words (`include`: with their white
    /// space) from `cursor`. `visual` is the Visual anchor when Visual mode
    /// is active; a selection wider than one character extends from the
    /// cursor instead of selecting afresh. The flag is false for vim's
    /// `FAIL`; the state is where the walk stopped either way, since vim
    /// moves the cursor (and `VIsual`) as it goes.
    fn current_word(
        &self,
        cursor: Pos,
        visual: Option<Pos>,
        include: bool,
        mut count: usize,
    ) -> (Found, bool) {
        let mut found = Found {
            start: visual.unwrap_or(cursor),
            cursor,
            inclusive: true,
        };
        let ok = self.walk_words(&mut found, visual.is_some(), include, &mut count);
        (found, ok)
    }

    /// The body of [`Self::current_word`], moving `found` in place.
    fn walk_words(
        &self,
        found: &mut Found,
        visual: bool,
        include: bool,
        count: &mut usize,
    ) -> bool {
        let Found {
            start,
            cursor: cur,
            inclusive,
        } = found;
        let mut start_pos = *cur;
        let mut include_white = false;

        if !visual || *start == *cur {
            // The word or white space under the cursor.
            self.back_in_line(cur);
            start_pos = *cur;
            if (self.cls(*cur) == 0) == include {
                // On white space that is included (" word"), or on a word
                // whose white space is not ("word"): find the word's end.
                if !self.end_word(cur, 1, true, true) {
                    return false;
                }
            } else {
                // On a word whose white space is included ("word  "), or on
                // white space that is not ("  "): find the next start. A
                // single-char word can land in column 0 of the next line;
                // back up to the end of this one.
                self.fwd_word(cur, 1, true);
                if cur.1 == 0 {
                    self.buf.decl(cur);
                } else {
                    Self::oneleft(cur);
                }
                include_white = include;
            }
            *start = start_pos;
            *count -= 1;
        }

        // Extend with more objects.
        while *count > 0 {
            *inclusive = true;
            if visual && *cur < *start {
                // Visual with the cursor at the start: move it back.
                if self.buf.decl(cur) == -1 {
                    return false;
                }
                if include != (self.cls(*cur) != 0) {
                    if !self.bck_word(cur, 1, true) {
                        return false;
                    }
                } else {
                    if !self.bckend_word(cur, 1, true) {
                        return false;
                    }
                    self.buf.incl(cur);
                }
            } else {
                // Forward one word and/or white area.
                if self.buf.incl(cur) == -1 {
                    return false;
                }
                if include != (self.cls(*cur) == 0) {
                    if !self.fwd_word(cur, 1, true) && *count > 1 {
                        return false;
                    }
                    // An end just past a line break must not take the next
                    // line's first character: put it on the white space.
                    if !Self::oneleft(cur) {
                        *inclusive = false;
                    }
                } else if !self.end_word(cur, 1, true, true) {
                    return false;
                }
            }
            *count -= 1;
        }

        // With no white space taken after the words, take the white space
        // before them instead — but never a line's indent.
        if include_white && (self.cls(*cur) != 0 || (cur.1 == 0 && !*inclusive)) {
            let mut p = start_pos;
            if Self::oneleft(&mut p) {
                self.back_in_line(&mut p);
                if self.cls(p) == 0 && p.1 > 0 {
                    *start = p;
                }
            }
        }
        true
    }

    /// The exclusive end of an operator range ending at `found.cursor`. An
    /// inclusive end on a line's NUL covers nothing more: vim's charwise
    /// operators never take the line break through it (`diw` on an empty
    /// line is a no-op).
    fn exclusive_end(&self, found: &Found) -> Pos {
        let (row, col) = found.cursor;
        if found.inclusive && col < self.buf.len(row) {
            (row, col + 1)
        } else {
            (row, col)
        }
    }
}
