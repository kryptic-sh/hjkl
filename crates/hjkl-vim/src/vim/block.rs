//! Vim FSM: Visual-block geometry in display columns.
//!
//! vim's block is a range of display columns, not of characters: a tab or a
//! double-width character can straddle either edge, and each row works out
//! its own share of the block. [`block_row`] is that per-row step, ported
//! from vim's `block_prep` (`ops.c`).

use crate::vim_state::vim;
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::buf_line;

/// The display columns a block covers, both ends inclusive. A `$` block
/// (`:h v_b_$`) has `right == usize::MAX`: every row runs to its own end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockCols {
    pub left: usize,
    pub right: usize,
}

/// One row's share of a block (vim's `struct block_def`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockRow {
    /// Char index where the row's text starts (`textcol`).
    pub start: usize,
    /// Char index one past the row's text (`textcol + textlen`).
    pub end: usize,
    /// Cells written as spaces before the text: for a yank, the part of a
    /// character straddling the left edge that lies inside the block; for a
    /// delete, the part outside it, which the delete keeps.
    pub startspaces: usize,
    /// The same at the right edge.
    pub endspaces: usize,
}

/// Display cells `ch` takes when it starts at display column `vcol`.
pub fn block_cell_width(ch: char, vcol: usize, tabstop: usize) -> usize {
    if ch == '\t' {
        let ts = tabstop.max(1);
        ts - vcol % ts
    } else {
        hjkl_buffer::char_col_to_visual_col(ch.encode_utf8(&mut [0; 4]), 1, tabstop)
    }
}

/// vim's `block_prep` for one row of `line`. `is_del` is true for an
/// operator that removes the block's text (`d`, `c`): a character
/// straddling an edge is then taken whole and the cells of it outside the
/// block are put back as spaces. Otherwise (`y`, the case operators) only
/// characters wholly inside count, and the straddling cells inside the
/// block are reported as spaces.
pub fn block_row(line: &str, cols: BlockCols, tabstop: usize, is_del: bool) -> BlockRow {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut row = BlockRow::default();
    // Walk to the block's left edge.
    let (mut vcol, mut i, mut incr, mut prev_start) = (0, 0, 0, 0);
    while vcol < cols.left && i < n {
        incr = block_cell_width(chars[i], vcol, tabstop);
        vcol += incr;
        prev_start = i;
        i += 1;
    }
    let start_vcol = vcol;
    let mut start = i;
    let start_char_vcols = incr;
    if start_vcol < cols.left {
        // The row ends before the block: a yank pads it to the block's width.
        if !is_del && cols.right != usize::MAX {
            row.endspaces = cols.right - cols.left + 1;
        }
        row.start = n;
        row.end = n;
        return row;
    }
    row.startspaces = start_vcol - cols.left;
    if is_del && row.startspaces > 0 {
        row.startspaces = start_char_vcols - row.startspaces;
    }
    let mut end = start;
    if start_vcol > cols.right {
        // One character covers the whole block.
        row.startspaces = cols.right - cols.left + 1;
        if is_del {
            row.startspaces = start_char_vcols - (start_vcol - cols.left);
            row.endspaces = start_vcol - cols.right - 1;
        }
    } else {
        let (mut v, mut j, mut prev_end) = (start_vcol, start, start);
        while v <= cols.right && j < n {
            prev_end = j;
            incr = block_cell_width(chars[j], v, tabstop);
            v += incr;
            j += 1;
        }
        end = j;
        if v > cols.right && cols.right != usize::MAX {
            row.endspaces = v - cols.right - 1;
            if !is_del && row.endspaces > 0 {
                row.endspaces = incr - row.endspaces;
                if end != start {
                    end = prev_end;
                }
            }
        }
    }
    if is_del && row.startspaces > 0 {
        start = prev_start;
    }
    row.start = start;
    row.end = end;
    row
}

/// The character `vcol` falls in or before on `line`: the first whose cells
/// start at or after it, so text put there lands at that display column.
/// `None` when the line ends before `vcol`, with how many cells short it is.
pub fn char_at_vcol(line: &str, vcol: usize, tabstop: usize) -> Result<usize, usize> {
    let mut v = 0;
    for (i, ch) in line.chars().enumerate() {
        if v >= vcol {
            return Ok(i);
        }
        v += block_cell_width(ch, v, tabstop);
    }
    if v >= vcol {
        Ok(line.chars().count())
    } else {
        Err(vcol - v)
    }
}

/// The tab on `line` whose cells straddle display column `vcol` (start
/// before it, end at or after it), as its char index and its width. vim's
/// block `I` / `A` turn such a tab into spaces before inserting, so the text
/// lands at exactly `vcol`.
pub fn tab_cut_at(line: &str, vcol: usize, tabstop: usize) -> Option<(usize, usize, usize)> {
    let mut v = 0;
    for (i, ch) in line.chars().enumerate() {
        let w = block_cell_width(ch, v, tabstop);
        if v >= vcol {
            return None;
        }
        if ch == '\t' && v + w > vcol {
            return Some((i, v, w));
        }
        v += w;
    }
    None
}

/// The display columns of the character at `col` on `line`, first and last,
/// or the column just past the line when `col` is at or past its end (a
/// Visual cursor may stand there).
fn char_cells(line: &str, col: usize, tabstop: usize) -> (usize, usize) {
    let first = hjkl_buffer::char_col_to_visual_col(line, col, tabstop);
    match line.chars().nth(col) {
        Some(ch) => (
            first,
            first + block_cell_width(ch, first, tabstop).max(1) - 1,
        ),
        None => (first, first),
    }
}

/// The block's anchor and cursor corners in display columns, as
/// `((row, col), (row, col))` with each column on the block's left or right
/// edge the way the anchor and cursor sit (`usize::MAX` for the cursor after
/// `$`). What `buffer_selection` hands the renderer.
pub fn block_corners<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
) -> ((usize, usize), (usize, usize)) {
    let tabstop = ed.settings().tabstop;
    let (ar, ac) = vim(ed).block_anchor;
    let cr = ed.cursor().0;
    let anchor = char_cells(&buf_line(ed.buffer(), ar).unwrap_or_default(), ac, tabstop);
    let cursor = char_cells(
        &buf_line(ed.buffer(), cr).unwrap_or_default(),
        vim(ed).block_vcol,
        tabstop,
    );
    let (left, right) = (anchor.0.min(cursor.0), anchor.1.max(cursor.1));
    if vim(ed).block_to_eol {
        return ((ar, left), (cr, usize::MAX));
    }
    if anchor.0 <= cursor.0 {
        ((ar, left), (cr, right))
    } else {
        ((ar, right), (cr, left))
    }
}

/// The current VisualBlock selection as rows and display columns: the cells
/// of the anchor's character and of the cursor's (`block_vcol` on the
/// cursor's row), as vim's `do_pending_operator` takes them, or every row to
/// its end after `$`.
pub fn block_rect<H: hjkl_engine::types::Host>(
    ed: &Editor<hjkl_buffer::View, H>,
) -> (usize, usize, BlockCols) {
    let tabstop = ed.settings().tabstop;
    let (ar, ac) = vim(ed).block_anchor;
    let cr = ed.cursor().0;
    let cc = vim(ed).block_vcol;
    let anchor = char_cells(&buf_line(ed.buffer(), ar).unwrap_or_default(), ac, tabstop);
    let cursor = char_cells(&buf_line(ed.buffer(), cr).unwrap_or_default(), cc, tabstop);
    let right = if vim(ed).block_to_eol {
        usize::MAX
    } else {
        anchor.1.max(cursor.1)
    };
    let cols = BlockCols {
        left: anchor.0.min(cursor.0),
        right,
    };
    (ar.min(cr), ar.max(cr), cols)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(left: usize, right: usize) -> BlockCols {
        BlockCols { left, right }
    }

    #[test]
    fn plain_text_is_its_columns() {
        let row = block_row("abcdef", cols(1, 3), 4, false);
        assert_eq!(
            (row.start, row.end, row.startspaces, row.endspaces),
            (1, 4, 0, 0)
        );
        let row = block_row("abcdef", cols(1, 3), 4, true);
        assert_eq!(
            (row.start, row.end, row.startspaces, row.endspaces),
            (1, 4, 0, 0)
        );
    }

    #[test]
    fn yank_of_a_row_short_of_the_block_is_padded() {
        let row = block_row("ab", cols(4, 6), 4, false);
        assert_eq!((row.start, row.end, row.endspaces), (2, 2, 3));
        // Reaching exactly to the left edge yields nothing.
        let row = block_row("abcd", cols(4, 6), 4, false);
        assert_eq!((row.start, row.end, row.endspaces), (4, 4, 0));
    }

    #[test]
    fn tab_straddling_the_left_edge() {
        // `\tab` with tabstop 4: the tab covers columns 0-3.
        let yank = block_row("\tab", cols(2, 4), 4, false);
        assert_eq!((yank.start, yank.end, yank.startspaces), (1, 2, 2));
        let del = block_row("\tab", cols(2, 4), 4, true);
        assert_eq!(
            (del.start, del.end, del.startspaces, del.endspaces),
            (0, 2, 2, 0)
        );
    }

    #[test]
    fn one_tab_covering_the_whole_block() {
        let yank = block_row("\tab", cols(1, 2), 4, false);
        assert_eq!((yank.start, yank.end, yank.startspaces), (1, 1, 2));
        let del = block_row("\tab", cols(1, 2), 4, true);
        assert_eq!(
            (del.start, del.end, del.startspaces, del.endspaces),
            (0, 1, 1, 1)
        );
    }

    #[test]
    fn block_past_a_tab_line_end_is_empty() {
        // `\tab` ends at column 5; a block at columns 6-9 holds nothing.
        let row = block_row("\tab", cols(6, 9), 4, true);
        assert_eq!(
            (row.start, row.end, row.startspaces, row.endspaces),
            (3, 3, 0, 0)
        );
    }

    #[test]
    fn dollar_block_runs_to_each_row_end() {
        let row = block_row("abcdef", cols(2, usize::MAX), 4, false);
        assert_eq!((row.start, row.end, row.endspaces), (2, 6, 0));
    }
}
