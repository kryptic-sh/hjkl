//! The `iw` / `aw` / `iW` / `aW` word text objects.

use super::text_object::is_wordchar;
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::buf_line;

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
