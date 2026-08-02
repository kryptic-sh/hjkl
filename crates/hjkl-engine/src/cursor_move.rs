//! Cursor moves that carry their own `curswant` semantics — phase 1 of
//! `docs/backlog.md`.
//!
//! Today, moving the cursor and maintaining `sticky_col` (vim's `curswant`)
//! are two separate actions, and ~186 call sites do only the first. [`Move`]
//! makes "what does this do to `curswant`?" a required argument of moving
//! rather than a follow-up call that can be forgotten: a new motion must name
//! a variant to compile.
//!
//! This module adds the API only. It is implemented over the existing
//! primitives so behaviour is bit-identical, and nothing is migrated onto it
//! yet — phases 2–4 do that, phase 5 then seals the raw primitives
//! (`buf_set_cursor_rc`, `View::set_cursor`) behind it.

use crate::Editor;
use crate::buf_helpers::{buf_line, buf_line_chars};
use hjkl_buffer::{char_col_to_visual_col, visual_col_to_char_col};

/// How a cursor move relates to `sticky_col` (vim's `curswant`) — the column
/// `j` / `k` aim at.
///
/// Vim's rule has exactly two halves, and every variant here encodes one of
/// them (or, for [`Move::Raw`], deliberately opts out):
///
/// - **vertical** motions READ `curswant`, clamp to the target row's length,
///   and leave `curswant` alone, so a run of `j` through short lines returns
///   to the original column on the far side;
/// - **everything else** that moves the cursor SETS `curswant` to the column
///   it landed on, so the next `j` aims there.
///
/// Pick by asking what the move *is*, not by what is convenient: the whole
/// point of the enum is that the answer gets recorded at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    /// `j` / `k` and their screen-line equivalents (`<C-e>` / `<C-y>`).
    ///
    /// READS `sticky_col`, clamps the landing column to `row`'s length, and
    /// leaves `sticky_col` holding the *un-clamped* want — that is what lets
    /// the column survive a short line and reappear on the next long one.
    /// Bootstraps `sticky_col` from the current column when nothing has set
    /// it yet.
    Vertical { row: usize },

    /// An explicit jump to a position: a search hit, `gg` / `G`, a mark, a
    /// mouse click, a picker `<CR>`, `]d`, a jumplist entry.
    ///
    /// SETS `sticky_col` to `col`. Vim resets `curswant` on every explicit
    /// jump, so the next `j` aims at the landed column rather than at
    /// wherever the cursor sat beforehand — the bug fixed in `c022a3a4`.
    Jump { row: usize, col: usize },

    /// A move within the current row: `h`, `l`, `w`, `b`, `e`, `f` / `t`,
    /// `0`, `^`, `$`.
    ///
    /// SETS `sticky_col` to `col`, same half of the rule as [`Move::Jump`];
    /// the separate variant exists so horizontal call sites cannot silently
    /// pass the wrong row.
    Horizontal { col: usize },

    /// Place the cursor without touching `sticky_col` at all.
    ///
    /// This is the *conspicuous* variant on purpose. It is not the default
    /// landing zone for a site whose semantics are unclear — a mechanical
    /// migration that picked `Raw` everywhere would compile, pass, and
    /// preserve the entire bug class. It is legitimate only where some other
    /// code owns `curswant` for the duration, i.e.:
    ///
    /// - the clamped write inside a vertical motion itself
    ///   (`hjkl_vim::vim::motion::apply_sticky_col`), which must preserve the
    ///   un-clamped want it just stored;
    /// - restoring a position saved earlier in the same operation (operator
    ///   bodies that park the cursor, run an edit, then put it back), where
    ///   the cursor is semantically where it already was;
    /// - host-side state restores — viewport sync, snapshot replay — where
    ///   the host's own sticky tracking is authoritative.
    ///
    /// Any other use wants [`Move::Jump`]. Each `Raw` should carry a one-line
    /// reason at its call site, and it stays greppable for review.
    Raw { row: usize, col: usize },
}

impl<H: crate::types::Host> Editor<hjkl_buffer::View, H> {
    /// Move the cursor, applying the `curswant` rule the [`Move`] variant
    /// names. See [`Move`] for which variant a given motion is.
    pub fn move_cursor(&mut self, m: Move) {
        match m {
            Move::Vertical { row } => {
                // Bootstrap from the current column on the very first vertical
                // motion, exactly as `apply_sticky_col` does.
                let (current_row, current_col) = self.cursor();
                let current_line = buf_line(&self.buffer, current_row).unwrap_or_default();
                let want = self.sticky_col().unwrap_or_else(|| {
                    char_col_to_visual_col(&current_line, current_col, self.settings().tabstop)
                });
                // Record the un-clamped want first so the next vertical motion
                // still sees it even though this one may clamp short.
                self.set_sticky_col(Some(want));
                // Clamp to the last char on non-empty rows — vim normal mode
                // never parks one past end of line; empty rows collapse to 0.
                let line = buf_line(&self.buffer, row).unwrap_or_default();
                let char_col = visual_col_to_char_col(&line, want, self.settings().tabstop);
                let max_col = buf_line_chars(&self.buffer, row).saturating_sub(1);
                // Quiet write: `jump_cursor` would overwrite the want stored
                // above with the clamped column.
                self.set_cursor_quiet(row, char_col.min(max_col));
            }
            Move::Jump { row, col } => self.jump_cursor(row, col),
            Move::Horizontal { col } => {
                let row = self.cursor().0;
                self.jump_cursor(row, col);
            }
            Move::Raw { row, col } => self.set_cursor_quiet(row, col),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Move;
    use crate::Editor;
    use crate::types::{DefaultHost, Options};
    use hjkl_buffer::View;

    fn editor(text: &str) -> Editor<View, DefaultHost> {
        Editor::new(View::from_str(text), DefaultHost::new(), Options::default())
    }

    /// `Vertical` reads `sticky_col`, clamps to the row, and leaves the
    /// un-clamped want in place so a later long row can restore it.
    #[test]
    fn vertical_clamps_but_preserves_the_unclamped_want() {
        // row 0 is long, row 1 is short, row 2 is long again.
        let mut e = editor("abcdefghij\nab\nabcdefghij");
        e.move_cursor(Move::Horizontal { col: 7 });
        assert_eq!(e.cursor(), (0, 7));
        assert_eq!(e.sticky_col(), Some(7));

        // Down onto the short row: the cursor clamps to its last char...
        e.move_cursor(Move::Vertical { row: 1 });
        assert_eq!(e.cursor(), (1, 1));
        // ...but the want survives un-clamped.
        assert_eq!(e.sticky_col(), Some(7));

        // Down again onto a long row: the original column comes back.
        e.move_cursor(Move::Vertical { row: 2 });
        assert_eq!(e.cursor(), (2, 7));
        assert_eq!(e.sticky_col(), Some(7));
    }

    /// An empty row collapses to column 0 and still keeps the want.
    #[test]
    fn vertical_over_an_empty_row_collapses_to_col_zero() {
        let mut e = editor("abcdef\n\nabcdef");
        e.move_cursor(Move::Horizontal { col: 4 });
        e.move_cursor(Move::Vertical { row: 1 });
        assert_eq!(e.cursor(), (1, 0));
        assert_eq!(e.sticky_col(), Some(4));
        e.move_cursor(Move::Vertical { row: 2 });
        assert_eq!(e.cursor(), (2, 4));
    }

    /// With no `sticky_col` yet, `Vertical` bootstraps from the current column.
    #[test]
    fn vertical_bootstraps_the_want_from_the_current_column() {
        let mut e = editor("abcdef\nabcdef");
        e.move_cursor(Move::Raw { row: 0, col: 3 });
        assert_eq!(e.sticky_col(), None);
        e.move_cursor(Move::Vertical { row: 1 });
        assert_eq!(e.cursor(), (1, 3));
        assert_eq!(e.sticky_col(), Some(3));
    }

    /// `Jump` sets the want to the column it landed on — the search-hit /
    /// `gg` / mark rule, and the fix `c022a3a4` made structural.
    #[test]
    fn jump_sets_the_want_to_the_landed_column() {
        let mut e = editor("abcdefghij\nabcdefghij");
        e.move_cursor(Move::Horizontal { col: 9 });
        assert_eq!(e.sticky_col(), Some(9));

        e.move_cursor(Move::Jump { row: 1, col: 2 });
        assert_eq!(e.cursor(), (1, 2));
        assert_eq!(e.sticky_col(), Some(2), "a jump resets curswant");
    }

    /// `Horizontal` keeps the row and sets the want to the new column.
    #[test]
    fn horizontal_keeps_the_row_and_sets_the_want() {
        let mut e = editor("abcdefghij\nabcdefghij");
        e.move_cursor(Move::Jump { row: 1, col: 8 });
        e.move_cursor(Move::Horizontal { col: 3 });
        assert_eq!(e.cursor(), (1, 3));
        assert_eq!(e.sticky_col(), Some(3));
    }

    /// `Raw` moves the cursor and leaves `sticky_col` exactly as it was —
    /// including leaving it `None`.
    #[test]
    fn raw_leaves_the_want_untouched() {
        let mut e = editor("abcdefghij\nabcdefghij");
        e.move_cursor(Move::Horizontal { col: 6 });
        assert_eq!(e.sticky_col(), Some(6));

        e.move_cursor(Move::Raw { row: 1, col: 1 });
        assert_eq!(e.cursor(), (1, 1));
        assert_eq!(e.sticky_col(), Some(6), "Raw must not disturb curswant");

        // And a `Raw` before anything has set a want leaves it unset.
        let mut fresh = editor("abcdefghij");
        fresh.move_cursor(Move::Raw { row: 0, col: 4 });
        assert_eq!(fresh.cursor(), (0, 4));
        assert_eq!(fresh.sticky_col(), None);
    }

    #[test]
    fn vertical_bootstraps_display_want_from_the_current_column() {
        let mut e = editor("\tabcdef\n\txyz");
        e.move_cursor(Move::Raw { row: 0, col: 2 });
        assert_eq!(e.sticky_col(), None);

        e.move_cursor(Move::Vertical { row: 1 });
        assert_eq!(e.cursor(), (1, 2));
        assert_eq!(e.sticky_col(), Some(5));
    }

    /// Display columns are converted back to character columns before clamping,
    /// so tabs do not make the cursor land too far right.
    #[test]
    fn vertical_converts_display_want_to_a_character_column() {
        let mut e = editor("\tabcdef\n\txyz");
        e.move_cursor(Move::Horizontal { col: 2 });
        assert_eq!(e.sticky_col(), Some(5));

        e.move_cursor(Move::Vertical { row: 1 });
        assert_eq!(e.cursor(), (1, 2));
        assert_eq!(e.sticky_col(), Some(5));
    }

    /// `Vertical` reproduces `apply_sticky_col`'s vertical branch exactly —
    /// the property phases 2–4 rely on when they swap call sites over.
    #[test]
    fn vertical_matches_the_apply_sticky_col_clamp() {
        for (want, row, expected) in [(7usize, 1usize, 1usize), (0, 1, 0), (1, 1, 1), (99, 0, 9)] {
            let mut e = editor("abcdefghij\nab");
            e.move_cursor(Move::Horizontal { col: 0 });
            e.set_sticky_col(Some(want));
            e.move_cursor(Move::Vertical { row });
            assert_eq!(e.cursor(), (row, expected), "want {want} onto row {row}");
            assert_eq!(e.sticky_col(), Some(want), "want {want} must survive");
        }
    }

    /// `Vertical` onto a line containing double-width chars must land on the
    /// char whose PAINTED cells contain the wanted column, not on the Nth
    /// char. Oracle: neovim 0.12.4, buffer `abcdefgh` / `ab世界cd`, `j` from
    /// each byte column of row 0 (1-based cols → 0-based here):
    ///
    /// ```text
    ///   col 1 -> bytecol 1 (a)    col 5 -> bytecol 6 (界)
    ///   col 2 -> bytecol 2 (b)    col 6 -> bytecol 6 (界)
    ///   col 3 -> bytecol 3 (世)   col 7 -> bytecol 9 (c)
    ///   col 4 -> bytecol 3 (世)   col 8 -> bytecol 10 (d)
    /// ```
    #[test]
    fn vertical_onto_wide_chars_matches_nvim() {
        // 0-based (want, char index on `ab世界cd`)
        for (want, expected) in [
            (0usize, 0usize), // a
            (1, 1),           // b
            (2, 2),           // 世, leading cell
            (3, 2),           // 世, trailing cell
            (4, 3),           // 界, leading cell
            (5, 3),           // 界, trailing cell
            (6, 4),           // c
            (7, 5),           // d
        ] {
            let mut e = editor("abcdefgh\nab世界cd");
            e.move_cursor(Move::Horizontal { col: want });
            assert_eq!(e.sticky_col(), Some(want));
            e.move_cursor(Move::Vertical { row: 1 });
            assert_eq!(e.cursor(), (1, expected), "want {want} onto the wide row");
        }
    }

    /// The other direction: leaving a wide line carries the char's LEADING
    /// visual cell as `curswant`. nvim, `ab世界cd` / `abcdefgh`: with the
    /// cursor on `界` (bytecol 6) `getcurpos()[4]` is 5 and `j` lands on
    /// bytecol 5 of the ASCII row — 0-based column 4.
    #[test]
    fn vertical_off_a_wide_line_uses_the_leading_cell() {
        for (start_char, expected_ascii) in [(2usize, 2usize), (3, 4), (4, 6), (5, 7)] {
            let mut e = editor("ab世界cd\nabcdefgh");
            e.move_cursor(Move::Horizontal { col: start_char });
            e.move_cursor(Move::Vertical { row: 1 });
            assert_eq!(
                e.cursor(),
                (1, expected_ascii),
                "from wide char {start_char}"
            );
        }
    }

    /// A combining mark is never a landing site. nvim, `abcdefgh` /
    /// `ae\u{301}b`: `j` from col 3 (0-based 2) lands on bytecol 5 — the `b`,
    /// char index 3 — skipping the mark at char index 2.
    #[test]
    fn vertical_onto_a_combining_mark_lands_on_the_next_char() {
        let mut e = editor("abcdefgh\nae\u{301}b");
        e.move_cursor(Move::Horizontal { col: 2 });
        e.move_cursor(Move::Vertical { row: 1 });
        assert_eq!(e.cursor(), (1, 3));
    }
}
