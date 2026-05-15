//! Pure geometry helpers for host-driven mouse translation.
//!
//! These helpers are host-agnostic: they operate on doc-space coordinates
//! (row/col in chars) and tab-expanded visual columns. The TUI host and any
//! future GUI host use them independently after doing their own
//! pixel-or-cell → visual-column conversion.

/// Inverse of `visual_col_for_char` (which lives in `hjkl-engine`).
///
/// Walk `line`'s chars accumulating tab-expanded visual width until the
/// accumulated width reaches or exceeds `visual_col`. Returns the char index
/// where the cursor would land — clamped to the line's char count (i.e. the
/// cursor can sit one past the last char, as in Insert mode).
///
/// # Tab expansion rule
///
/// A `\t` expands to the next `tab_width` stop:
/// `width += tab_width - (current_visual % tab_width)`.
///
/// Clicking on any cell within the expanded run of a tab char lands on the
/// tab's char index — matching Vim's behaviour where the cursor snaps to the
/// tab character itself, not past it.
///
/// # Wide-char note
///
/// Wide-char support (CJK, emoji) is a separate concern and is NOT implemented
/// here. This function treats every non-tab character as 1 visual cell wide,
/// consistent with the engine's `visual_col_for_char` assumption.
///
/// # Examples
///
/// ```rust
/// use hjkl_buffer::visual_col_to_char_col;
///
/// // ASCII line — exact match
/// assert_eq!(visual_col_to_char_col("hello", 2, 4), 2);
///
/// // Past EOL clamps to char count
/// assert_eq!(visual_col_to_char_col("hi", 99, 4), 2);
///
/// // Empty line always returns 0
/// assert_eq!(visual_col_to_char_col("", 5, 4), 0);
/// ```
pub fn visual_col_to_char_col(line: &str, visual_col: usize, tab_width: usize) -> usize {
    let tab_w = if tab_width == 0 { 1 } else { tab_width };
    let mut visual = 0usize;
    for (i, ch) in line.chars().enumerate() {
        if visual >= visual_col {
            // We've reached or passed the target before consuming this char.
            return i;
        }
        let advance = if ch == '\t' {
            tab_w - (visual % tab_w)
        } else {
            1
        };
        // If advancing this char would carry us to or past visual_col AND this
        // char is a tab, the click landed inside the expanded tab run — vim
        // lands the cursor on the tab char itself, so return its index.
        if ch == '\t' && visual + advance > visual_col {
            return i;
        }
        visual += advance;
    }
    // visual_col is past EOL — clamp to char count (Insert mode can sit there).
    line.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_exact_visual_col() {
        // "hello": each char is 1 cell wide
        assert_eq!(visual_col_to_char_col("hello", 0, 4), 0);
        assert_eq!(visual_col_to_char_col("hello", 1, 4), 1);
        assert_eq!(visual_col_to_char_col("hello", 3, 4), 3);
        assert_eq!(visual_col_to_char_col("hello", 4, 4), 4);
    }

    #[test]
    fn tab_expansion_click_inside_run_lands_on_tab_char() {
        // "x\tyz" with tab_width=4:
        //   x  → visual 0
        //   \t → visual 1..=3 (expands to stop 4, so 3 cells wide)
        //   y  → visual 4
        //   z  → visual 5
        // Clicking on visual 1, 2, or 3 should all land on char index 1 (the tab).
        let line = "x\tyz";
        assert_eq!(visual_col_to_char_col(line, 1, 4), 1); // inside tab → tab char
        assert_eq!(visual_col_to_char_col(line, 2, 4), 1); // inside tab → tab char
        assert_eq!(visual_col_to_char_col(line, 3, 4), 1); // inside tab → tab char
        assert_eq!(visual_col_to_char_col(line, 4, 4), 2); // y
        assert_eq!(visual_col_to_char_col(line, 5, 4), 3); // z
    }

    #[test]
    fn tab_at_column_boundary() {
        // Tab at visual col 4 with tab_width=4 expands to the next stop at 8.
        // "abcd\tefg": a=0,b=1,c=2,d=3 → \t at visual 4 → visual 8, then e=8,f=9,g=10
        let line = "abcd\tefg";
        assert_eq!(visual_col_to_char_col(line, 4, 4), 4); // tab char itself
        assert_eq!(visual_col_to_char_col(line, 5, 4), 4); // inside tab run → tab char
        assert_eq!(visual_col_to_char_col(line, 7, 4), 4); // still inside tab run
        assert_eq!(visual_col_to_char_col(line, 8, 4), 5); // e
    }

    #[test]
    fn past_eol_clamps_to_char_count() {
        // Insert mode allows cursor at char_count (one past last char).
        assert_eq!(visual_col_to_char_col("hi", 99, 4), 2);
        assert_eq!(visual_col_to_char_col("x", 100, 4), 1);
    }

    #[test]
    fn empty_line_always_zero() {
        assert_eq!(visual_col_to_char_col("", 0, 4), 0);
        assert_eq!(visual_col_to_char_col("", 5, 4), 0);
    }

    #[test]
    fn multibyte_single_cell_chars() {
        // Greek letters are single-cell (Latin Extended / Basic Greek block).
        // visual col == char index for single-cell multi-byte chars.
        let line = "αβγδε"; // 5 chars, each 1 visual cell
        assert_eq!(visual_col_to_char_col(line, 0, 4), 0);
        assert_eq!(visual_col_to_char_col(line, 2, 4), 2);
        assert_eq!(visual_col_to_char_col(line, 4, 4), 4);
        assert_eq!(visual_col_to_char_col(line, 5, 4), 5); // clamp = char_count
    }

    #[test]
    fn tab_width_one_treats_tab_as_single_cell() {
        // tab_width=1 → tab is 1 cell wide (stop at next multiple of 1 = always +1)
        let line = "a\tb";
        assert_eq!(visual_col_to_char_col(line, 0, 1), 0); // a
        assert_eq!(visual_col_to_char_col(line, 1, 1), 1); // tab
        assert_eq!(visual_col_to_char_col(line, 2, 1), 2); // b
    }

    #[test]
    fn tab_width_zero_treated_as_one() {
        // tab_width=0 is normalised to 1 to avoid divide-by-zero.
        let line = "a\tb";
        assert_eq!(visual_col_to_char_col(line, 0, 0), 0);
        assert_eq!(visual_col_to_char_col(line, 1, 0), 1);
        assert_eq!(visual_col_to_char_col(line, 2, 0), 2);
    }

    #[test]
    fn leading_tab_then_text() {
        // "\thello" with tab_width=4: tab occupies visual 0..3, h=4, e=5, ...
        let line = "\thello";
        assert_eq!(visual_col_to_char_col(line, 0, 4), 0); // tab char
        assert_eq!(visual_col_to_char_col(line, 3, 4), 0); // inside tab → tab char
        assert_eq!(visual_col_to_char_col(line, 4, 4), 1); // h
        assert_eq!(visual_col_to_char_col(line, 8, 4), 5); // o
    }
}
