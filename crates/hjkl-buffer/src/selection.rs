use crate::Position;

/// First-class vim selection. Each variant carries the kind directly
/// rather than relying on a single char-range primitive with separate
/// "treat as line / block" overlays — that's the whole point of
/// owning the buffer model. Anchor is where the user pressed
/// `v` / `V` / `Ctrl-V`; head moves with the cursor and is updated
/// via [`Selection::extend_to`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// `v` — character-wise. Covers `anchor..=head` inclusive,
    /// row-major. Empty rows in the middle of a multi-row span are
    /// treated as having one virtual cell so the highlight is
    /// visible (matches vim).
    Char { anchor: Position, head: Position },
    /// `V` — line-wise. Both endpoints are pure row indices; column
    /// is irrelevant (the whole row is always covered).
    Line { anchor_row: usize, head_row: usize },
    /// `Ctrl-V` — block-wise. Covers the inclusive rectangle whose
    /// corners are anchor and head. Row range is `min..=max`; column
    /// range is `min..=max` independently of the corner diagonals. The
    /// columns are display columns, which [`Selection::row_span_on`] turns
    /// into each row's characters (the same on a line without tabs or wide
    /// characters).
    Block { anchor: Position, head: Position },
}

/// Bounds of a selection on a particular row, expressed as inclusive
/// char-column range. `None` means the row is outside the selection.
/// `Some((0, usize::MAX))` is the convention for "whole row" — the
/// renderer caps it at the row's actual length.
pub type RowSpan = Option<(usize, usize)>;

impl Selection {
    /// Where the cursor end of the selection lives. After
    /// [`Selection::extend_to`] this is the freshly-set value.
    pub fn head(self) -> Position {
        match self {
            Self::Char { head, .. } => head,
            Self::Line { head_row, .. } => Position::new(head_row, 0),
            Self::Block { head, .. } => head,
        }
    }

    /// The opposite end of the selection — fixed when the user
    /// entered visual mode.
    pub fn anchor(self) -> Position {
        match self {
            Self::Char { anchor, .. } => anchor,
            Self::Line { anchor_row, .. } => Position::new(anchor_row, 0),
            Self::Block { anchor, .. } => anchor,
        }
    }

    /// Move the cursor end of the selection to `pos`. Anchor stays
    /// put; for `Line` we drop the column since rows are all that
    /// matter.
    pub fn extend_to(&mut self, pos: Position) {
        match self {
            Self::Char { head, .. } => *head = pos,
            Self::Line { head_row, .. } => *head_row = pos.row,
            Self::Block { head, .. } => *head = pos,
        }
    }

    /// What columns of `row` the selection covers. Used by the
    /// render layer to paint the selection bg without having to
    /// know each variant's quirks.
    ///
    /// - `Char` on a single row: `[min_col, max_col]`.
    /// - `Char` spanning rows: from `head/anchor.col` on the start
    ///   row to end-of-line, then full rows in between, then
    ///   `0..=end.col` on the last row.
    /// - `Line`: `(0, usize::MAX)` for every row in range.
    /// - `Block`: `[min_col, max_col]` regardless of which row.
    pub fn row_span(self, row: usize) -> RowSpan {
        match self {
            Self::Char { anchor, head } => {
                let (start, end) = order(anchor, head);
                if row < start.row || row > end.row {
                    return None;
                }
                let lo = if row == start.row { start.col } else { 0 };
                let hi = if row == end.row { end.col } else { usize::MAX };
                Some((lo, hi))
            }
            Self::Line {
                anchor_row,
                head_row,
            } => {
                let (lo, hi) = if anchor_row <= head_row {
                    (anchor_row, head_row)
                } else {
                    (head_row, anchor_row)
                };
                if row < lo || row > hi {
                    None
                } else {
                    Some((0, usize::MAX))
                }
            }
            Self::Block { anchor, head } => {
                let (top, bot) = (anchor.row.min(head.row), anchor.row.max(head.row));
                if row < top || row > bot {
                    return None;
                }
                let (left, right) = (anchor.col.min(head.col), anchor.col.max(head.col));
                Some((left, right))
            }
        }
    }

    /// [`Selection::row_span`] as characters of `line`, the text of `row`.
    /// A `Block`'s columns are display columns (vim's blockwise Visual covers
    /// the same screen columns on every row), so on `line` it covers every
    /// character with a cell between them, a tab or wide character cut by an
    /// edge included; past the line's end it counts one character per cell,
    /// as the other variants' columns do there. `Char` and `Line` are
    /// [`Selection::row_span`] unchanged.
    pub fn row_span_on(self, row: usize, line: &str, tab_width: usize) -> RowSpan {
        let Self::Block { .. } = self else {
            return self.row_span(row);
        };
        let (left, right) = self.row_span(row)?;
        let mut first = None;
        let mut last = None;
        let mut vcol = 0;
        let mut count = 0;
        let tab_width = tab_width.max(1);
        for (i, ch) in line.chars().enumerate() {
            let next = if ch == '\t' {
                vcol + tab_width - vcol % tab_width
            } else {
                vcol + crate::geom::char_col_to_visual_col(ch.encode_utf8(&mut [0; 4]), 1, 1)
            };
            if vcol <= right && next > left {
                first.get_or_insert(i);
                last = Some(i);
            }
            vcol = next;
            count = i + 1;
        }
        let past = |col: usize| count + col.saturating_sub(vcol);
        let lo = first.unwrap_or_else(|| past(left));
        let hi = if right == usize::MAX {
            usize::MAX
        } else {
            match last {
                Some(i) if right < vcol => i,
                _ => past(right),
            }
        };
        Some((lo, hi))
    }

    /// Inclusive `(top_row, bottom_row)` covered by the selection.
    pub fn row_bounds(self) -> (usize, usize) {
        match self {
            Self::Char { anchor, head } => {
                let (s, e) = order(anchor, head);
                (s.row, e.row)
            }
            Self::Line {
                anchor_row,
                head_row,
            } => (anchor_row.min(head_row), anchor_row.max(head_row)),
            Self::Block { anchor, head } => (anchor.row.min(head.row), anchor.row.max(head.row)),
        }
    }
}

/// Order a pair of positions row-major.
fn order(a: Position, b: Position) -> (Position, Position) {
    if a <= b { (a, b) } else { (b, a) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(top: usize, left: usize, bot: usize, right: usize) -> Selection {
        Selection::Block {
            anchor: Position::new(top, left),
            head: Position::new(bot, right),
        }
    }

    #[test]
    fn block_row_span_on_plain_text_is_row_span() {
        let sel = block(0, 1, 1, 3);
        assert_eq!(sel.row_span_on(0, "abcdef", 4), sel.row_span(0));
    }

    #[test]
    fn block_row_span_on_maps_display_columns_through_a_tab() {
        // `\tab` at tab width 4: the tab covers cells 0-3, `a` 4, `b` 5.
        let sel = block(0, 2, 1, 4);
        assert_eq!(sel.row_span_on(0, "\tab", 4), Some((0, 1)));
        assert_eq!(sel.row_span_on(1, "abcdefgh", 4), Some((2, 4)));
    }

    #[test]
    fn block_row_span_on_past_the_line_counts_cells() {
        // Cells 6-9 lie past the end of `\tab` (width 6).
        let sel = block(0, 6, 1, 9);
        assert_eq!(sel.row_span_on(0, "\tab", 4), Some((3, 6)));
    }

    #[test]
    fn block_row_span_on_dollar_runs_to_the_end() {
        let sel = block(0, 1, 0, usize::MAX);
        assert_eq!(sel.row_span_on(0, "\tab", 4), Some((0, usize::MAX)));
    }

    #[test]
    fn char_single_row_inclusive() {
        let sel = Selection::Char {
            anchor: Position::new(0, 2),
            head: Position::new(0, 5),
        };
        assert_eq!(sel.row_span(0), Some((2, 5)));
        assert_eq!(sel.row_span(1), None);
    }

    #[test]
    fn char_multi_row_clips_endpoints() {
        let sel = Selection::Char {
            anchor: Position::new(1, 3),
            head: Position::new(3, 7),
        };
        assert_eq!(sel.row_span(0), None);
        assert_eq!(sel.row_span(1), Some((3, usize::MAX)));
        assert_eq!(sel.row_span(2), Some((0, usize::MAX)));
        assert_eq!(sel.row_span(3), Some((0, 7)));
        assert_eq!(sel.row_span(4), None);
    }

    #[test]
    fn char_handles_reversed_endpoints() {
        // Cursor moved up-left of anchor.
        let sel = Selection::Char {
            anchor: Position::new(3, 7),
            head: Position::new(1, 3),
        };
        assert_eq!(sel.row_span(1), Some((3, usize::MAX)));
        assert_eq!(sel.row_span(3), Some((0, 7)));
    }

    #[test]
    fn line_covers_whole_rows_only() {
        let sel = Selection::Line {
            anchor_row: 5,
            head_row: 7,
        };
        assert_eq!(sel.row_span(4), None);
        assert_eq!(sel.row_span(5), Some((0, usize::MAX)));
        assert_eq!(sel.row_span(6), Some((0, usize::MAX)));
        assert_eq!(sel.row_span(7), Some((0, usize::MAX)));
        assert_eq!(sel.row_span(8), None);
    }

    #[test]
    fn block_inclusive_rect() {
        let sel = Selection::Block {
            anchor: Position::new(2, 4),
            head: Position::new(5, 8),
        };
        for row in 2..=5 {
            assert_eq!(sel.row_span(row), Some((4, 8)));
        }
        assert_eq!(sel.row_span(1), None);
        assert_eq!(sel.row_span(6), None);
    }

    #[test]
    fn block_normalises_corners() {
        // Anchor bottom-right, head top-left.
        let sel = Selection::Block {
            anchor: Position::new(5, 8),
            head: Position::new(2, 4),
        };
        for row in 2..=5 {
            assert_eq!(sel.row_span(row), Some((4, 8)));
        }
    }

    #[test]
    fn extend_to_updates_head() {
        let mut sel = Selection::Char {
            anchor: Position::new(0, 0),
            head: Position::new(0, 3),
        };
        sel.extend_to(Position::new(2, 9));
        assert_eq!(sel.head(), Position::new(2, 9));
        assert_eq!(sel.anchor(), Position::new(0, 0));
    }

    #[test]
    fn line_extend_to_drops_column() {
        let mut sel = Selection::Line {
            anchor_row: 1,
            head_row: 1,
        };
        sel.extend_to(Position::new(4, 50));
        assert_eq!(sel.head(), Position::new(4, 0));
    }

    #[test]
    fn row_bounds_each_kind() {
        let c = Selection::Char {
            anchor: Position::new(2, 0),
            head: Position::new(5, 0),
        };
        assert_eq!(c.row_bounds(), (2, 5));
        let l = Selection::Line {
            anchor_row: 7,
            head_row: 3,
        };
        assert_eq!(l.row_bounds(), (3, 7));
        let b = Selection::Block {
            anchor: Position::new(8, 1),
            head: Position::new(2, 9),
        };
        assert_eq!(b.row_bounds(), (2, 8));
    }
}
