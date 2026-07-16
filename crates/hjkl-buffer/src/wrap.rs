//! Soft-wrap helpers shared between the renderer, viewport scroll,
//! and the buffer's vertical motion code.

use std::cell::RefCell;

use unicode_width::UnicodeWidthChar;

thread_local! {
    /// Reused `(char, width)` scratch for [`wrap_segments`]. `wrap_segments` is
    /// called once per visible row per frame; reusing this buffer avoids a
    /// per-call heap allocation (it grows to the widest line seen, then stays).
    static WRAP_SCRATCH: RefCell<Vec<(char, u16)>> = const { RefCell::new(Vec::new()) };
}

/// Soft-wrap mode controlling how doc rows wider than the text area
/// turn into multiple visual rows. Default is [`Wrap::None`] — every
/// doc row is exactly one screen row and `top_col` clips the left
/// side, mirroring vim's `set nowrap` default for sqeel today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wrap {
    /// Single screen row per doc row; clip with `top_col`.
    #[default]
    None,
    /// Break at the cell boundary regardless of word edges.
    Char,
    /// Break at the last whitespace inside the visible width when
    /// possible; falls back to a char break for runs longer than the
    /// width.
    Word,
}

/// Split `line` into char-index segments `[start, end)` such that
/// each segment's display width fits within `width` cells.
/// `Wrap::Word` rewinds to the last whitespace inside the candidate
/// segment when a break would otherwise split a word; falls through
/// to a char break for runs longer than `width`. `Wrap::None` is not
/// expected here — callers branch before calling — but is handled
/// for completeness as a single segment covering the full line.
pub fn wrap_segments(line: &str, width: u16, mode: Wrap) -> Vec<(usize, usize)> {
    if matches!(mode, Wrap::None) || width == 0 || line.is_empty() {
        return vec![(0, line.chars().count())];
    }
    WRAP_SCRATCH.with(|scratch| {
        let mut chars = scratch.borrow_mut();
        chars.clear();
        chars.extend(
            line.chars()
                .map(|c| (c, c.width().unwrap_or(1).max(1) as u16)),
        );
        let total = chars.len();
        let mut segs = Vec::new();
        let mut start = 0usize;
        while start < total {
            let mut cells: u16 = 0;
            let mut i = start;
            while i < total {
                let w = chars[i].1;
                if cells + w > width {
                    break;
                }
                cells += w;
                i += 1;
            }
            // A single char wider than `width` (e.g. a double-width CJK/emoji
            // char in a 1-cell text area) consumes zero cells above, leaving
            // `i == start`. Force progress by emitting it as its own
            // overflowing segment; without this `break_at` collapses to
            // `start`, `start` never advances, and the loop spins forever
            // pushing `(start, start)` until the process OOMs.
            if i == start {
                i = start + 1;
            }
            if i == total {
                segs.push((start, total));
                break;
            }
            let break_at = if matches!(mode, Wrap::Word) {
                // Look for the last whitespace inside [start, i] so the
                // segment ends *after* that whitespace. Falls back to a
                // hard char break when the segment has no whitespace.
                (start..i)
                    .rev()
                    .find(|&k| chars[k].0.is_whitespace())
                    .map(|k| k + 1)
                    .filter(|&end| end > start)
                    .unwrap_or(i)
            } else {
                i
            };
            segs.push((start, break_at));
            start = break_at;
        }
        if segs.is_empty() {
            segs.push((0, 0));
        }
        segs
    })
}

/// Inverse of the per-char accounting `wrap_segments` uses to find where a
/// segment breaks: map a visual x offset (`visual_offset`, cells counted
/// from the segment's OWN left edge — i.e. 0 at `seg.0`, matching how the
/// renderer paints each wrapped row starting at its text area's left
/// column regardless of `seg.0`) to the char index within `seg = [start,
/// end)` it lands on.
///
/// Uses the exact same per-char width formula `wrap_segments` sums to find
/// segment boundaries (`char::width().unwrap_or(1).max(1)`), so a click
/// that `wrap_segments` would consider inside this segment resolves to the
/// same character `wrap_segments` wrapped around. This does NOT reproduce
/// the renderer's real tab-stop expansion (`wrap_segments` itself counts a
/// tab as 1 cell, not `tabstop` cells) — that mismatch predates this
/// function and is `wrap_segments`' own documented simplification; mouse
/// click mapping mirrors it for round-trip consistency with the wrap
/// engine rather than inventing a different tab model for wrapped rows.
///
/// Clamps to `end` (one past the segment's last char) once cumulative
/// width reaches or exceeds `visual_offset` — vim's "past EOL on this
/// visual row" landing spot.
pub fn char_col_for_visual_offset(line: &str, seg: (usize, usize), visual_offset: usize) -> usize {
    let (start, end) = seg;
    let mut cells = 0usize;
    for (i, ch) in line
        .chars()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
    {
        let w = ch.width().unwrap_or(1).max(1);
        if cells + w > visual_offset {
            return i;
        }
        cells += w;
    }
    end
}

/// Returns the index into `segments` whose `[start, end)` covers
/// `col`. The past-end cursor (`col == last segment's end`) maps to
/// the last segment, matching vim's "EOL on the visual row that
/// holds the line's last char" behaviour.
pub fn segment_for_col(segments: &[(usize, usize)], col: usize) -> usize {
    if segments.is_empty() {
        return 0;
    }
    if let Some(idx) = segments.iter().position(|&(s, e)| col >= s && col < e) {
        return idx;
    }
    segments.len() - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_returns_full_line_segment() {
        let segs = wrap_segments("hello world", 4, Wrap::None);
        assert_eq!(segs, vec![(0, 11)]);
    }

    #[test]
    fn wide_char_wider_than_width_terminates() {
        // Regression: a double-width char in a 1-cell area used to spin forever
        // (i == start → break_at == start → start never advances → OOM). Each
        // wide char must become its own overflowing segment and progress.
        let segs = wrap_segments("你好", 1, Wrap::Char);
        assert_eq!(segs, vec![(0, 1), (1, 2)]);
        let segs = wrap_segments("你好", 1, Wrap::Word);
        assert_eq!(segs, vec![(0, 1), (1, 2)]);
        // Mixed narrow + wide with a 1-cell width still fully covers the line.
        let segs = wrap_segments("a你b", 1, Wrap::Char);
        assert_eq!(segs, vec![(0, 1), (1, 2), (2, 3)]);
    }

    #[test]
    fn segment_for_col_finds_containing_segment() {
        let segs = vec![(0, 4), (4, 8), (8, 10)];
        assert_eq!(segment_for_col(&segs, 0), 0);
        assert_eq!(segment_for_col(&segs, 3), 0);
        assert_eq!(segment_for_col(&segs, 4), 1);
        assert_eq!(segment_for_col(&segs, 7), 1);
        assert_eq!(segment_for_col(&segs, 9), 2);
        // Past-end col clamps to last segment.
        assert_eq!(segment_for_col(&segs, 10), 2);
        assert_eq!(segment_for_col(&segs, 99), 2);
    }

    // ── char_col_for_visual_offset (Fix 3: cell_to_doc soft-wrap inverse) ──

    #[test]
    fn char_col_for_visual_offset_first_segment_start() {
        // "abcdefghij" @ width=4, Char wrap → segs (0,4)(4,8)(8,10).
        let line = "abcdefghij";
        let segs = wrap_segments(line, 4, Wrap::Char);
        assert_eq!(segs, vec![(0, 4), (4, 8), (8, 10)]);
        // Offset 0 in segment 0 → char 'a' (index 0).
        assert_eq!(char_col_for_visual_offset(line, segs[0], 0), 0);
        // Offset 3 in segment 0 → char 'd' (index 3, last char of the segment).
        assert_eq!(char_col_for_visual_offset(line, segs[0], 3), 3);
    }

    #[test]
    fn char_col_for_visual_offset_is_relative_to_segment_start_not_line_start() {
        // A click on the SECOND visual row (segment 1, chars [4,8) = "efgh")
        // at offset 0 must resolve to char index 4 ('e'), not 0 ('a') — the
        // whole point of Fix 3: continuation-row clicks must not collapse
        // back to the start of the line.
        let line = "abcdefghij";
        let segs = wrap_segments(line, 4, Wrap::Char);
        assert_eq!(
            char_col_for_visual_offset(line, segs[1], 0),
            4,
            "offset 0 within segment 1 must land on 'e' (char index 4), not the line start"
        );
        assert_eq!(
            char_col_for_visual_offset(line, segs[1], 2),
            6,
            "offset 2 within segment 1 must land on 'g' (char index 6)"
        );
    }

    #[test]
    fn char_col_for_visual_offset_past_segment_end_clamps() {
        let line = "abcdefghij";
        let segs = wrap_segments(line, 4, Wrap::Char);
        // Offset past the segment's width clamps to `end` (one past the
        // segment's last char) — vim's past-EOL-on-this-row landing spot.
        assert_eq!(char_col_for_visual_offset(line, segs[0], 99), segs[0].1);
        assert_eq!(char_col_for_visual_offset(line, segs[2], 99), segs[2].1);
    }

    #[test]
    fn char_col_for_visual_offset_wide_char_consumes_two_cells() {
        // "你好" @ width=1, Char wrap → segs (0,1)(1,2), one wide char each.
        let line = "你好";
        let segs = wrap_segments(line, 1, Wrap::Char);
        assert_eq!(segs, vec![(0, 1), (1, 2)]);
        assert_eq!(char_col_for_visual_offset(line, segs[0], 0), 0);
        assert_eq!(char_col_for_visual_offset(line, segs[1], 0), 1);
    }

    #[test]
    fn char_col_for_visual_offset_round_trips_with_wrap_segments() {
        // For every segment, every offset inside [0, segment_display_width)
        // must resolve to a char whose OWN segment (per `segment_for_col`)
        // is the same segment — i.e. this never "escapes" into a
        // neighboring segment's characters.
        let line = "the quick brown fox jumps over lazy dogs";
        for width in [3u16, 5, 8, 12] {
            let segs = wrap_segments(line, width, Wrap::Word);
            for (idx, &seg) in segs.iter().enumerate() {
                let seg_width: usize = line
                    .chars()
                    .skip(seg.0)
                    .take(seg.1 - seg.0)
                    .map(|c| c.width().unwrap_or(1).max(1))
                    .sum();
                for off in 0..seg_width {
                    let col = char_col_for_visual_offset(line, seg, off);
                    assert_eq!(
                        segment_for_col(&segs, col),
                        idx,
                        "width={width} seg={seg:?} off={off} resolved to char {col} \
                         which segment_for_col assigns to a different segment"
                    );
                }
            }
        }
    }
}
