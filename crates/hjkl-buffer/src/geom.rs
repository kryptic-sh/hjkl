//! Pure geometry helpers for host-driven mouse translation.
//!
//! These helpers are host-agnostic: they operate on doc-space coordinates
//! (row/col in chars) and tab-expanded visual columns. The TUI host and any
//! future GUI host use them independently after doing their own
//! pixel-or-cell → visual-column conversion.
//!
//! # Cell-width semantics
//!
//! Both helpers measure a char with exactly the rule `hjkl-buffer-tui`'s
//! `paint_row` uses to advance the cursor across the terminal, because being
//! consistent with the cell the glyph was actually painted in is the entire
//! point of these functions:
//!
//! | char | cells | why |
//! | ---- | ----- | --- |
//! | `\t` | to the next `tab_width` stop, measured from the **visual** column | tab stops are screen positions, so a preceding wide char shifts them |
//! | `width() == Some(w)` | `w` | the `unicode-width` table — 2 for CJK and most emoji, 1 for ASCII |
//! | `width() == Some(0)` | 0 | combining marks and variation selectors compose onto the preceding cell |
//! | `width() == None` | 1 | control characters; see below |
//!
//! **Zero-width chars** (combining marks, `U+FE0F`) advance nothing and are
//! never a cursor landing site — [`visual_col_to_char_col`] skips them, so a
//! click or a `j` into that column lands on the base char they compose onto
//! (or, past the composed cell, on the next real char). Verified against
//! neovim 0.12.4: on `"ae\u{301}b"`, `virtcol` reports the mark and the `e` at
//! the same column, and `j` from column 3 of an ASCII line lands on the `b`,
//! never on the mark.
//!
//! **Control chars** (`width() == None`) count as **one** cell. This
//! deliberately diverges from vim, which renders `^A` in two: `paint_row`
//! resolves them through `sanitize_control`, which maps every C0/C1 control to
//! a single-width Control Pictures glyph (`U+0001` → `␁`), and then advances
//! by `ch.width().unwrap_or(1)` — one cell. Matching the renderer is the
//! contract here; matching vim's `^X` notation would put the cursor one cell
//! left of its own glyph on every line containing a control char.
//!
//! **Known divergence from vim: emoji presentation sequences.** vim widens
//! `U+2764 U+FE0F` ("❤️") to two cells because it resolves the sequence as a
//! grapheme cluster. `unicode-width` is consulted per `char` here (and in
//! `paint_row`), so `U+2764` measures 1 (East Asian Ambiguous) and the
//! variation selector 0 — one cell total. The two sides of hjkl agree with
//! each other, which keeps the cursor on its glyph; they are both one cell
//! narrower than vim would be. Fixing that means grapheme segmentation in the
//! renderer first.

use unicode_width::UnicodeWidthChar;

/// Cells occupied by `ch` when it is painted starting at visual column
/// `visual`. See the module docs for the full rule; `tab_w` must already be
/// normalised to be non-zero.
#[inline]
fn cell_width(ch: char, visual: usize, tab_w: usize) -> usize {
    if ch == '\t' {
        tab_w - (visual % tab_w)
    } else {
        // `unwrap_or(1)` covers only `None` (control chars); `Some(0)` stays 0.
        ch.width().unwrap_or(1)
    }
}

/// Inverse of [`char_col_to_visual_col`].
///
/// Walk `line`'s chars accumulating painted cell width until the run of cells
/// belonging to a char contains `visual_col`. Returns that char's index —
/// clamped to the line's char count (i.e. the cursor can sit one past the last
/// char, as in Insert mode).
///
/// Landing anywhere inside a char's cell run yields that char: the middle of a
/// tab's expansion, or the trailing cell of a double-width glyph. This matches
/// vim, which snaps the cursor to the character itself rather than past it —
/// confirmed with neovim, where `j` from column 4 of `abcdefgh` onto
/// `ab世界cd` lands on `世` (whose cells are 2 and 3).
///
/// Zero-width chars are skipped rather than returned; see the module docs.
///
/// # Examples
///
/// ```rust
/// use hjkl_buffer::visual_col_to_char_col;
///
/// // ASCII line — exact match
/// assert_eq!(visual_col_to_char_col("hello", 2, 4), 2);
///
/// // Both cells of a double-width char map back to it
/// assert_eq!(visual_col_to_char_col("ab世界cd", 2, 4), 2);
/// assert_eq!(visual_col_to_char_col("ab世界cd", 3, 4), 2);
///
/// // Past EOL clamps to char count
/// assert_eq!(visual_col_to_char_col("hi", 99, 4), 2);
///
/// // Empty line always returns 0
/// assert_eq!(visual_col_to_char_col("", 5, 4), 0);
/// ```
pub fn visual_col_to_char_col(line: &str, visual_col: usize, tab_width: usize) -> usize {
    let tab_w = if tab_width == 0 { 1 } else { tab_width };
    // Char 0 always starts at visual column 0 — including the degenerate case
    // of a line that opens with a combining mark, where the "skip zero-width"
    // rule below would otherwise hand back char 1.
    if visual_col == 0 {
        return 0;
    }
    let mut visual = 0usize;
    for (i, ch) in line.chars().enumerate() {
        let advance = cell_width(ch, visual, tab_w);
        if advance == 0 {
            // Composes onto the preceding cell — not a landing site.
            continue;
        }
        if visual + advance > visual_col {
            // The target cell falls inside this char's run.
            return i;
        }
        visual += advance;
    }
    // visual_col is past EOL — clamp to char count (Insert mode can sit there).
    line.chars().count()
}

/// Map a character index in `line` to its starting visual column (the
/// screen-cell offset from line start of the char's FIRST cell). The inverse of
/// [`visual_col_to_char_col`].
///
/// Used to anchor screen overlays (e.g. the K-key hover popup) at a document
/// position — the doc→cell counterpart of mouse-click translation — and to
/// store vim's `curswant`. Returning the first cell matches vim: with the
/// cursor on `世` in `ab世界cd`, `getcurpos()[4]` (curswant) is 3 in vim's
/// 1-based columns, i.e. the leading cell, not the trailing one.
///
/// `tab_width == 0` is treated as 1. `char_col` past the line end clamps to the
/// line's full visual width. See the module docs for the per-char width rule.
///
/// # Examples
///
/// ```rust
/// use hjkl_buffer::char_col_to_visual_col;
///
/// // ASCII line — visual col == char col
/// assert_eq!(char_col_to_visual_col("hello", 2, 4), 2);
///
/// // A leading tab pushes the next char to the tab stop
/// assert_eq!(char_col_to_visual_col("\tx", 1, 4), 4);
///
/// // A double-width char takes two cells
/// assert_eq!(char_col_to_visual_col("ab世界cd", 3, 4), 4);
///
/// // Past EOL clamps to the line's visual width
/// assert_eq!(char_col_to_visual_col("hi", 99, 4), 2);
/// ```
pub fn char_col_to_visual_col(line: &str, char_col: usize, tab_width: usize) -> usize {
    let tab_w = if tab_width == 0 { 1 } else { tab_width };
    let mut visual = 0usize;
    for (i, ch) in line.chars().enumerate() {
        if i == char_col {
            return visual;
        }
        visual += cell_width(ch, visual, tab_w);
    }
    visual
}

/// Visual width of `line`'s leading run of whitespace: `' '` occupies one
/// cell, `'\t'` advances to the next multiple of `tab_width`, and the walk
/// stops at the first other char. The indent-guide renderers in both TUI
/// layers compute exactly this to decide where guides may be painted.
pub fn leading_visual_width(line: &str, tab_width: usize) -> usize {
    let tab_w = if tab_width == 0 { 1 } else { tab_width };
    let mut visual = 0usize;
    for ch in line.chars() {
        match ch {
            ' ' => visual += 1,
            '\t' => visual += tab_w - (visual % tab_w),
            _ => break,
        }
    }
    visual
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

    // ── Wide / zero-width characters ─────────────────────────────────────
    //
    // Every expectation below was read out of neovim 0.12.4 (which is
    // wide-char correct) via `virtcol('.', true)`, then converted from vim's
    // 1-based columns to our 0-based ones.

    #[test]
    fn cjk_char_col_to_visual_col() {
        // nvim, `ab世界cd`, virtcol start per char (1-based): 1,2,3,5,7,8
        //   → 0-based starts: 0,1,2,4,6,7; strdisplaywidth = 8.
        let line = "ab世界cd";
        assert_eq!(char_col_to_visual_col(line, 0, 4), 0); // a
        assert_eq!(char_col_to_visual_col(line, 1, 4), 1); // b
        assert_eq!(char_col_to_visual_col(line, 2, 4), 2); // 世
        assert_eq!(char_col_to_visual_col(line, 3, 4), 4); // 界
        assert_eq!(char_col_to_visual_col(line, 4, 4), 6); // c
        assert_eq!(char_col_to_visual_col(line, 5, 4), 7); // d
        assert_eq!(char_col_to_visual_col(line, 6, 4), 8); // past EOL = full width
    }

    #[test]
    fn cjk_visual_col_to_char_col() {
        // nvim `j` from `abcdefgh` onto `ab世界cd`: narrow byte col 4
        // (0-based visual 3, the trailing cell of 世) lands on 世, and byte
        // col 6 (0-based visual 5, trailing cell of 界) lands on 界.
        let line = "ab世界cd";
        assert_eq!(visual_col_to_char_col(line, 0, 4), 0); // a
        assert_eq!(visual_col_to_char_col(line, 1, 4), 1); // b
        assert_eq!(visual_col_to_char_col(line, 2, 4), 2); // 世, leading cell
        assert_eq!(visual_col_to_char_col(line, 3, 4), 2); // 世, trailing cell
        assert_eq!(visual_col_to_char_col(line, 4, 4), 3); // 界, leading cell
        assert_eq!(visual_col_to_char_col(line, 5, 4), 3); // 界, trailing cell
        assert_eq!(visual_col_to_char_col(line, 6, 4), 4); // c
        assert_eq!(visual_col_to_char_col(line, 7, 4), 5); // d
        assert_eq!(visual_col_to_char_col(line, 8, 4), 6); // past EOL → char count
    }

    #[test]
    fn emoji_is_two_cells() {
        // nvim `a🦀b`: 0-based starts 0,1,3; strdisplaywidth = 4.
        let line = "a🦀b";
        assert_eq!(char_col_to_visual_col(line, 1, 4), 1);
        assert_eq!(char_col_to_visual_col(line, 2, 4), 3);
        assert_eq!(char_col_to_visual_col(line, 3, 4), 4);
        assert_eq!(visual_col_to_char_col(line, 1, 4), 1); // crab, leading cell
        assert_eq!(visual_col_to_char_col(line, 2, 4), 1); // crab, trailing cell
        assert_eq!(visual_col_to_char_col(line, 3, 4), 2); // b
    }

    #[test]
    fn variation_selector_is_zero_width() {
        // `a❤️b` = a, U+2764, U+FE0F, b. `unicode-width` gives U+2764 = 1
        // (East Asian Ambiguous) and U+FE0F = 0, so the pair occupies ONE
        // cell here and `paint_row` paints it in one cell. nvim renders the
        // emoji-presentation sequence two cells wide; see the doc comment —
        // matching the renderer is the contract, not matching nvim.
        let line = "a\u{2764}\u{fe0f}b";
        assert_eq!(char_col_to_visual_col(line, 1, 4), 1); // U+2764
        assert_eq!(char_col_to_visual_col(line, 2, 4), 2); // U+FE0F starts after it
        assert_eq!(char_col_to_visual_col(line, 3, 4), 2); // b — VS16 added nothing
        assert_eq!(char_col_to_visual_col(line, 4, 4), 3);
        // The VS16 is never a landing site: cell 2 is `b`.
        assert_eq!(visual_col_to_char_col(line, 1, 4), 1);
        assert_eq!(visual_col_to_char_col(line, 2, 4), 3);
    }

    #[test]
    fn combining_mark_is_zero_width() {
        // nvim `aéb` (a, e, U+0301, b): 0-based starts 0,1,1,2;
        // strdisplaywidth = 3. And `j` from `abcdefgh` col 3 (0-based visual
        // 2) onto this line lands on `b` — never on the combining mark.
        let line = "ae\u{301}b";
        assert_eq!(char_col_to_visual_col(line, 0, 4), 0); // a
        assert_eq!(char_col_to_visual_col(line, 1, 4), 1); // e
        assert_eq!(char_col_to_visual_col(line, 2, 4), 2); // U+0301 (zero width)
        assert_eq!(char_col_to_visual_col(line, 3, 4), 2); // b
        assert_eq!(char_col_to_visual_col(line, 4, 4), 3); // past EOL

        assert_eq!(visual_col_to_char_col(line, 0, 4), 0); // a
        assert_eq!(visual_col_to_char_col(line, 1, 4), 1); // e (the mark composes on it)
        assert_eq!(visual_col_to_char_col(line, 2, 4), 3); // b, NOT the mark at 2
        assert_eq!(visual_col_to_char_col(line, 3, 4), 4); // past EOL → char count
    }

    #[test]
    fn tab_stop_measured_from_visual_not_char_column() {
        // nvim `世\tx` with tabstop=4: 世 occupies cells 0-1, the tab
        // expands from visual 2 to the stop at 4 (so 2 cells), x is at 4.
        // Counting 世 as one char would have put the tab at visual 1 and x
        // at 4 by accident — but `世\t\tx` separates the two.
        let line = "世\tx";
        assert_eq!(char_col_to_visual_col(line, 0, 4), 0); // 世
        assert_eq!(char_col_to_visual_col(line, 1, 4), 2); // tab starts at 2
        assert_eq!(char_col_to_visual_col(line, 2, 4), 4); // x
        assert_eq!(char_col_to_visual_col(line, 3, 4), 5);

        assert_eq!(visual_col_to_char_col(line, 0, 4), 0); // 世 leading
        assert_eq!(visual_col_to_char_col(line, 1, 4), 0); // 世 trailing
        assert_eq!(visual_col_to_char_col(line, 2, 4), 1); // inside tab run
        assert_eq!(visual_col_to_char_col(line, 3, 4), 1); // inside tab run
        assert_eq!(visual_col_to_char_col(line, 4, 4), 2); // x
    }

    #[test]
    fn wide_char_before_tab_shifts_the_stop() {
        // `ab世\tx` tab_width=4: a=0 b=1 世=2..3, so the tab starts exactly
        // ON a stop and expands a full 4 cells to 8; x lands at 8.
        // The naive (1-cell-per-char) rule put the tab at visual 3 and x at
        // 4 — off by four.
        let line = "ab世\tx";
        assert_eq!(char_col_to_visual_col(line, 3, 4), 4); // tab
        assert_eq!(char_col_to_visual_col(line, 4, 4), 8); // x
        assert_eq!(visual_col_to_char_col(line, 8, 4), 4); // x
        assert_eq!(visual_col_to_char_col(line, 7, 4), 3); // inside the tab run
    }

    #[test]
    fn control_char_is_one_cell_matching_the_renderer() {
        // `paint_row` uses `ch.width().unwrap_or(1)` and substitutes a
        // single-width Control Pictures glyph (`sanitize_control`), so a C0
        // control occupies ONE cell here. nvim renders `^A` in two; we match
        // the renderer deliberately (see the doc comment).
        let line = "a\u{1}b";
        assert_eq!(char_col_to_visual_col(line, 1, 4), 1); // U+0001
        assert_eq!(char_col_to_visual_col(line, 2, 4), 2); // b
        assert_eq!(visual_col_to_char_col(line, 1, 4), 1);
        assert_eq!(visual_col_to_char_col(line, 2, 4), 2);
    }

    #[test]
    fn round_trip_char_to_visual_to_char() {
        // Every char index that starts a cell must survive the round trip.
        // Zero-width chars are excluded by construction: they share a cell
        // with the char before them, so they are not landing sites.
        for line in ["ab世界cd", "世\tx", "a🦀b", "\tab世", "abcd"] {
            let mut visual = 0usize;
            for (i, ch) in line.chars().enumerate() {
                let v = char_col_to_visual_col(line, i, 4);
                assert_eq!(v, visual, "start col for char {i} of {line:?}");
                let w = if ch == '\t' {
                    4 - (visual % 4)
                } else {
                    unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1)
                };
                if w > 0 {
                    assert_eq!(
                        visual_col_to_char_col(line, v, 4),
                        i,
                        "round trip for char {i} of {line:?}"
                    );
                }
                visual += w;
            }
        }
    }
}
