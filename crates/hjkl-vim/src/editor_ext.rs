//! [`VimEditorExt`] — vim-discipline accessor methods on the engine
//! [`Editor`], migrated out of `hjkl-engine` (#267 / #265 G3).
//!
//! These read the vim FSM state (`Editor::vim`) to answer render/selection
//! questions. They belong to the vim *discipline*, not the mode-agnostic
//! engine core, so they live here — a blanket trait impl on
//! `Editor<Buffer, H>`. As `VimState` finishes relocating into this crate,
//! more of the engine's vim accessors move onto this trait; call sites pick
//! them up with `use hjkl_vim::VimEditorExt`.

use hjkl_engine::types::{Highlight, HighlightKind, Host, Pos};
use hjkl_engine::vim::{Operator, RangeKind};
use hjkl_engine::{Editor, VimMode};

/// Move a position back by one character, wrapping to the end of the previous
/// line when at column 0. Clamps at the buffer start `(0, 0)`. Used to render
/// exclusive (VSCode) char selections via the inclusive buffer-tui paint path.
///
/// Was `Editor::dec_pos_one_char` in the engine; it exists only to serve
/// [`VimEditorExt::buffer_selection`], so it moved here with it.
fn dec_pos_one_char<H: Host>(
    ed: &Editor<hjkl_buffer::Buffer, H>,
    p: hjkl_buffer::Position,
) -> hjkl_buffer::Position {
    use hjkl_buffer::Position;
    if p.col > 0 {
        return Position::new(p.row, p.col - 1);
    }
    if p.row > 0 {
        let prev = p.row - 1;
        let len = ed.line(prev).map(|l| l.chars().count()).unwrap_or(0);
        return Position::new(prev, len);
    }
    Position::new(0, 0)
}

/// Vim-discipline read accessors layered onto every `Editor<Buffer, H>`.
///
/// Blanket-implemented below; bring it into scope with
/// `use hjkl_vim::VimEditorExt` to call these on an `Editor`.
pub trait VimEditorExt {
    /// VisualBlock selection bounds as `(top, bot, left, right)` — inclusive
    /// rows and inclusive columns, derived from the block anchor and the
    /// cursor's sticky column. Meaningful only while in VisualBlock mode;
    /// callers that need the "are we in block mode?" guard use
    /// [`VimEditorExt::block_highlight`] instead.
    fn visual_block_bounds(&self) -> (usize, usize, usize, usize);

    /// The VisualBlock highlight rectangle `(top, bot, left, right)`, or
    /// `None` when the editor is not in VisualBlock mode.
    fn block_highlight(&self) -> Option<(usize, usize, usize, usize)>;

    /// Start/end `(row, col)` of the active char-wise Visual selection,
    /// positionally ordered. `None` when not in Visual mode.
    ///
    /// When [`hjkl_engine::editor::Settings::selection_exclusive`] is `false`
    /// (default, vim behaviour): both endpoints are **inclusive** — the cells
    /// at `start` and `end` are both selected.
    ///
    /// When it is `true` (VSCode bar-cursor behaviour): the range is
    /// **half-open** — `start` is included but `end` is the first cell that is
    /// NOT selected (the caret sits before it). If the selection is empty
    /// (`anchor == cursor`) `None` is returned so callers do not need to check
    /// for zero-length ranges.
    fn char_highlight(&self) -> Option<((usize, usize), (usize, usize))>;

    /// Return the half-open exclusive char-visual range `(start, end)` where
    /// `end` is the first cell NOT selected (the caret position). `None`
    /// when not in Visual mode or the selection is empty.
    ///
    /// Convenience accessor for the VSCode dispatcher; avoids duplicating
    /// the anchor/cursor ordering logic at the call site.
    fn visual_char_range_exclusive(&self) -> Option<((usize, usize), (usize, usize))>;

    /// Top/bottom rows of the active VisualLine selection (inclusive).
    /// `None` when we're not in VisualLine mode.
    fn line_highlight(&self) -> Option<(usize, usize)>;

    /// Active selection in `hjkl_buffer::Selection` shape. `None` when not in
    /// a Visual mode. The host hands this straight to `BufferView`.
    fn buffer_selection(&self) -> Option<hjkl_buffer::Selection>;

    /// Active visual selection as a SPEC [`Highlight`] with
    /// [`HighlightKind::Selection`].
    ///
    /// Returns `None` when the editor isn't in a Visual mode. Visual-line and
    /// visual-block selections collapse to the bounding char range of the
    /// selection — the SPEC `Selection` kind doesn't carry sub-line info
    /// today; hosts that need full line / block geometry continue to read
    /// [`VimEditorExt::buffer_selection`] (the legacy `hjkl_buffer::Selection`
    /// shape).
    fn selection_highlight(&self) -> Option<Highlight>;

    /// Read-only view of the jumplist as `(jump_back, jump_fwd)` — positions
    /// pushed on "big" motions. Newest entry is at the back of each; `Ctrl-o`
    /// pops from `jump_back` and `Ctrl-i` from `jump_fwd`. Backs `:jumps`.
    #[allow(clippy::type_complexity)]
    fn jump_list(&self) -> (&[(usize, usize)], &[(usize, usize)]);

    /// Position the cursor was at when the user last jumped via `<C-o>` /
    /// `g;` / similar. `None` before any jump.
    fn last_jump_back(&self) -> Option<(usize, usize)>;

    // ─── Text-object resolution (hjkl#70) ──────────────────────────────────
    //
    // Pure functions — no cursor mutation, no mode change, no register write.
    // Each delegates to the `hjkl_engine::vim::text_object_*_bridge` resolvers,
    // which remain in the engine until vim.rs itself relocates (#267).
    //
    // Return value: `Some((start, end))` where both positions are `(row, col)`
    // char-column pairs and `end` is *exclusive* (one past the last char to act
    // on), matching the convention used by `delete_range` / `yank_range` / etc.
    //
    // Quote methods take the quote char itself (`'"'`, `'\''`, `` '`' ``).
    // Bracket methods take the OPEN bracket char (`'('`, `'{'`, `'['`, `'<'`);
    // close-bracket variants are NOT accepted — the grammar layer normalises
    // close→open before calling these.

    /// Resolve the range of `iw` (inner word) at the cursor.
    ///
    /// An inner word is the contiguous run of keyword characters (or
    /// punctuation characters if the cursor is on punctuation) under the
    /// cursor, without surrounding whitespace. Whitespace-only positions
    /// return `None`.
    fn text_object_inner_word(&self) -> Option<((usize, usize), (usize, usize))>;

    /// Resolve the range of `aw` (around word) at the cursor.
    ///
    /// Like `iw` but extends the range to include trailing whitespace after
    /// the word. If no trailing whitespace exists, leading whitespace before
    /// the word is absorbed instead (vim `:help text-objects` behaviour).
    fn text_object_around_word(&self) -> Option<((usize, usize), (usize, usize))>;

    /// Resolve the range of `iW` (inner WORD) at the cursor.
    ///
    /// A WORD is any contiguous run of non-whitespace characters — punctuation
    /// is not a word boundary.
    fn text_object_inner_big_word(&self) -> Option<((usize, usize), (usize, usize))>;

    /// Resolve the range of `aW` (around WORD) at the cursor.
    fn text_object_around_big_word(&self) -> Option<((usize, usize), (usize, usize))>;

    /// Resolve the range of `i<quote>` (inner quote) at the cursor.
    ///
    /// Excludes the quote characters themselves. `None` when the cursor's line
    /// contains fewer than two occurrences of `quote`, or no matching pair can
    /// be found around or ahead of the cursor.
    fn text_object_inner_quote(&self, quote: char) -> Option<((usize, usize), (usize, usize))>;

    /// Resolve the range of `a<quote>` (around quote) at the cursor.
    ///
    /// Like `i<quote>` but includes the quote characters plus surrounding
    /// whitespace on one side: trailing after the closing quote if any exists,
    /// otherwise leading before the opening quote.
    fn text_object_around_quote(&self, quote: char) -> Option<((usize, usize), (usize, usize))>;

    /// Resolve the range of `i<bracket>` (inner bracket pair) at the cursor.
    ///
    /// The cursor may be anywhere inside the pair or on a bracket character.
    /// When not inside any pair the resolver falls back to a forward scan
    /// (targets.vim-style: `ci(` works when the cursor is before `(`).
    /// Multi-line pairs are supported.
    fn text_object_inner_bracket(&self, open: char) -> Option<((usize, usize), (usize, usize))>;

    /// Resolve the range of `a<bracket>` (around bracket pair) at the cursor.
    ///
    /// Like `i<bracket>` but includes the bracket characters themselves.
    fn text_object_around_bracket(&self, open: char) -> Option<((usize, usize), (usize, usize))>;

    /// Resolve `is` (inner sentence) at the cursor.
    ///
    /// Excludes trailing whitespace. Sentence boundaries follow vim's `is`
    /// semantics (period / `?` / `!` followed by whitespace or
    /// end-of-paragraph).
    fn text_object_inner_sentence(&self) -> Option<((usize, usize), (usize, usize))>;

    /// Resolve `as` (around sentence) at the cursor.
    ///
    /// Like `is` but includes trailing whitespace after the terminator.
    fn text_object_around_sentence(&self) -> Option<((usize, usize), (usize, usize))>;

    /// Resolve `ip` (inner paragraph) at the cursor.
    ///
    /// A paragraph is a block of non-blank lines bounded by blank lines or
    /// buffer edges. `None` when the cursor is on a blank line.
    fn text_object_inner_paragraph(&self) -> Option<((usize, usize), (usize, usize))>;

    /// Resolve `ap` (around paragraph) at the cursor.
    ///
    /// Like `ip` but includes one trailing blank line when present.
    fn text_object_around_paragraph(&self) -> Option<((usize, usize), (usize, usize))>;

    /// Resolve `it` (inner tag) at the cursor.
    ///
    /// Matches XML/HTML-style `<tag>...</tag>` pairs, returning the content
    /// between the open and close tags (excluding the tags themselves).
    fn text_object_inner_tag(&self) -> Option<((usize, usize), (usize, usize))>;

    /// Resolve `at` (around tag) at the cursor.
    ///
    /// Like `it` but includes the open and close tag delimiters.
    fn text_object_around_tag(&self) -> Option<((usize, usize), (usize, usize))>;

    // ─── Range-mutation primitives (hjkl#70) ───────────────────────────────
    //
    // These do not consume input — the caller (the visual-mode operator path)
    // has already resolved the range from the visual selection before calling
    // in. Normal-mode op dispatch continues to use `apply_op_motion` /
    // `apply_op_double` / `apply_op_find` / `apply_op_text_obj`.

    /// Delete the region `[start, end)` and stash the removed text in
    /// `register`. `'"'` selects the unnamed register (vim default);
    /// `'a'`–`'z'` select named registers.
    fn delete_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        kind: RangeKind,
        register: char,
    );

    /// Yank (copy) the region `[start, end)` into `register` without mutating
    /// the buffer. `'"'` selects the unnamed register; `'0'` the yank-only
    /// register; `'a'`–`'z'` select named registers.
    fn yank_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        kind: RangeKind,
        register: char,
    );

    /// Delete the region `[start, end)` and transition to Insert mode (vim `c`
    /// operator). The deleted text is stashed in `register`. On return the
    /// editor is in Insert mode; the caller must not issue further normal-mode
    /// ops until the insert session ends.
    fn change_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        kind: RangeKind,
        register: char,
    );

    /// Indent (`count > 0`) or outdent (`count < 0`) the row span
    /// `[start.0, end.0]`. Column components are ignored — indent is always
    /// linewise. `shiftwidth` overrides the editor's configured shiftwidth for
    /// this call; pass `0` to use the current editor setting. `count == 0` is
    /// a no-op.
    fn indent_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        count: i32,
        shiftwidth: u32,
    );

    /// Apply a case transformation (`Operator::Uppercase` /
    /// `Operator::Lowercase` / `Operator::ToggleCase`) to the region
    /// `[start, end)`. Other `Operator` variants are silently ignored (no-op).
    /// Registers are left untouched — vim's case operators do not write to
    /// registers.
    fn case_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        kind: RangeKind,
        op: Operator,
    );

    // ─── Block-shape range-mutation primitives (hjkl#70) ───────────────────
    //
    // Rectangular VisualBlock operations. `top_row`/`bot_row` are inclusive
    // line indices; `left_col`/`right_col` are inclusive char-column bounds.
    // Ragged-edge handling (short lines not reaching `right_col`) matches the
    // engine FSM's `apply_block_operator` path — short lines lose only the
    // chars that exist. `register` is the target; `'"'` selects unnamed.

    /// Delete a rectangular VisualBlock selection.
    fn delete_block(
        &mut self,
        top_row: usize,
        bot_row: usize,
        left_col: usize,
        right_col: usize,
        register: char,
    );

    /// Yank a rectangular VisualBlock selection into `register` without
    /// mutating the buffer.
    fn yank_block(
        &mut self,
        top_row: usize,
        bot_row: usize,
        left_col: usize,
        right_col: usize,
        register: char,
    );

    /// Delete a rectangular VisualBlock selection and enter Insert mode (`c`
    /// operator). Mode is Insert on return.
    fn change_block(
        &mut self,
        top_row: usize,
        bot_row: usize,
        left_col: usize,
        right_col: usize,
        register: char,
    );

    /// Indent (`count > 0`) or outdent (`count < 0`) rows `top_row..=bot_row`.
    /// Column bounds are ignored — vim's block indent is always linewise.
    /// `count == 0` is a no-op.
    fn indent_block(
        &mut self,
        top_row: usize,
        bot_row: usize,
        left_col: usize,
        right_col: usize,
        count: i32,
    );

    /// Auto-indent (v1 dumb shiftwidth) the row span `[start.0, end.0]`.
    /// Column components are ignored — auto-indent is always linewise.
    ///
    /// The algorithm is a naive bracket-depth counter: it scans the buffer
    /// from row 0 to compute the correct depth at `start.0`, then for each
    /// line in the target range strips existing leading whitespace and
    /// prepends `depth × indent_unit`. Lines whose first non-whitespace
    /// character is a close bracket get one fewer indent level. Empty /
    /// whitespace-only lines are cleared. After the operation the cursor lands
    /// on the first non-whitespace character of `start_row` (vim parity `==`).
    ///
    /// **v1 limitation**: the bracket scan does not detect brackets inside
    /// string literals or comments.
    fn auto_indent_range(&mut self, start: (usize, usize), end: (usize, usize));
}

impl<H: Host> VimEditorExt for Editor<hjkl_buffer::Buffer, H> {
    fn visual_block_bounds(&self) -> (usize, usize, usize, usize) {
        let (ar, ac) = self.vim.block_anchor;
        let (cr, _) = self.cursor();
        let cc = self.vim.block_vcol;
        (ar.min(cr), ar.max(cr), ac.min(cc), ac.max(cc))
    }

    fn block_highlight(&self) -> Option<(usize, usize, usize, usize)> {
        if self.vim_mode() != VimMode::VisualBlock {
            return None;
        }
        let (ar, ac) = self.vim.block_anchor;
        let cr = self.cursor().0;
        let cc = self.vim.block_vcol;
        Some((ar.min(cr), ar.max(cr), ac.min(cc), ac.max(cc)))
    }

    fn char_highlight(&self) -> Option<((usize, usize), (usize, usize))> {
        if self.vim_mode() != VimMode::Visual {
            return None;
        }
        let anchor = self.vim.visual_anchor;
        let cursor = self.cursor();
        let (start, end) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        if self.settings().selection_exclusive {
            // Half-open: start..end (end excluded). Empty when start == end.
            if start == end {
                return None;
            }
            Some((start, end))
        } else {
            // Inclusive (vim default): both endpoints are selected.
            Some((start, end))
        }
    }

    fn visual_char_range_exclusive(&self) -> Option<((usize, usize), (usize, usize))> {
        if self.vim_mode() != VimMode::Visual {
            return None;
        }
        let anchor = self.vim.visual_anchor;
        let cursor = self.cursor();
        if anchor == cursor {
            return None;
        }
        let (start, end) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        Some((start, end))
    }

    fn line_highlight(&self) -> Option<(usize, usize)> {
        if self.vim_mode() != VimMode::VisualLine {
            return None;
        }
        let anchor = self.vim.visual_line_anchor;
        let cursor = self.cursor().0;
        Some((anchor.min(cursor), anchor.max(cursor)))
    }

    fn buffer_selection(&self) -> Option<hjkl_buffer::Selection> {
        use hjkl_buffer::{Position, Selection};
        let (cr, cc) = self.cursor();
        match self.vim_mode() {
            VimMode::Visual => {
                let (ar, ac) = self.vim.visual_anchor;
                let head = Position::new(cr, cc);
                if self.settings().selection_exclusive {
                    // Exclusive (VSCode bar-caret): render the half-open char set
                    // [start, end) so the cell under the caret is NOT highlighted.
                    // The buffer-tui renderer paints `row_span` inclusively, so
                    // drop one char off the max end. Empty selection → no
                    // highlight (caller is effectively in Insert).
                    let anchor_pos = Position::new(ar, ac);
                    if anchor_pos == head {
                        return None;
                    }
                    let (start, end) = if (ar, ac) <= (head.row, head.col) {
                        (anchor_pos, head)
                    } else {
                        (head, anchor_pos)
                    };
                    return Some(Selection::Char {
                        anchor: start,
                        head: dec_pos_one_char(self, end),
                    });
                }
                Some(Selection::Char {
                    anchor: Position::new(ar, ac),
                    head,
                })
            }
            VimMode::VisualLine => Some(Selection::Line {
                anchor_row: self.vim.visual_line_anchor,
                head_row: cr,
            }),
            VimMode::VisualBlock => {
                let (ar, ac) = self.vim.block_anchor;
                Some(Selection::Block {
                    anchor: Position::new(ar, ac),
                    head: Position::new(cr, self.vim.block_vcol),
                })
            }
            _ => None,
        }
    }

    fn selection_highlight(&self) -> Option<Highlight> {
        let sel = self.buffer_selection()?;
        let (start, end) = match sel {
            hjkl_buffer::Selection::Char { anchor, head } => {
                let a = (anchor.row, anchor.col);
                let h = (head.row, head.col);
                if a <= h { (a, h) } else { (h, a) }
            }
            hjkl_buffer::Selection::Line {
                anchor_row,
                head_row,
            } => {
                let (top, bot) = if anchor_row <= head_row {
                    (anchor_row, head_row)
                } else {
                    (head_row, anchor_row)
                };
                let last_col = self.line(bot).map(|l| l.len()).unwrap_or(0);
                ((top, 0), (bot, last_col))
            }
            hjkl_buffer::Selection::Block { anchor, head } => {
                let (top, bot) = if anchor.row <= head.row {
                    (anchor.row, head.row)
                } else {
                    (head.row, anchor.row)
                };
                let (left, right) = if anchor.col <= head.col {
                    (anchor.col, head.col)
                } else {
                    (head.col, anchor.col)
                };
                ((top, left), (bot, right))
            }
        };
        Some(Highlight {
            range: Pos {
                line: start.0 as u32,
                col: start.1 as u32,
            }..Pos {
                line: end.0 as u32,
                col: end.1 as u32,
            },
            kind: HighlightKind::Selection,
        })
    }

    fn jump_list(&self) -> (&[(usize, usize)], &[(usize, usize)]) {
        (&self.vim.jump_back, &self.vim.jump_fwd)
    }

    fn last_jump_back(&self) -> Option<(usize, usize)> {
        self.vim.jump_back.last().copied()
    }

    // ─── Text-object resolution ────────────────────────────────────────────

    fn text_object_inner_word(&self) -> Option<((usize, usize), (usize, usize))> {
        hjkl_engine::vim::text_object_inner_word_bridge(self)
    }

    fn text_object_around_word(&self) -> Option<((usize, usize), (usize, usize))> {
        hjkl_engine::vim::text_object_around_word_bridge(self)
    }

    fn text_object_inner_big_word(&self) -> Option<((usize, usize), (usize, usize))> {
        hjkl_engine::vim::text_object_inner_big_word_bridge(self)
    }

    fn text_object_around_big_word(&self) -> Option<((usize, usize), (usize, usize))> {
        hjkl_engine::vim::text_object_around_big_word_bridge(self)
    }

    fn text_object_inner_quote(&self, quote: char) -> Option<((usize, usize), (usize, usize))> {
        hjkl_engine::vim::text_object_inner_quote_bridge(self, quote)
    }

    fn text_object_around_quote(&self, quote: char) -> Option<((usize, usize), (usize, usize))> {
        hjkl_engine::vim::text_object_around_quote_bridge(self, quote)
    }

    fn text_object_inner_bracket(&self, open: char) -> Option<((usize, usize), (usize, usize))> {
        hjkl_engine::vim::text_object_inner_bracket_bridge(self, open)
    }

    fn text_object_around_bracket(&self, open: char) -> Option<((usize, usize), (usize, usize))> {
        hjkl_engine::vim::text_object_around_bracket_bridge(self, open)
    }

    fn text_object_inner_sentence(&self) -> Option<((usize, usize), (usize, usize))> {
        hjkl_engine::vim::text_object_inner_sentence_bridge(self)
    }

    fn text_object_around_sentence(&self) -> Option<((usize, usize), (usize, usize))> {
        hjkl_engine::vim::text_object_around_sentence_bridge(self)
    }

    fn text_object_inner_paragraph(&self) -> Option<((usize, usize), (usize, usize))> {
        hjkl_engine::vim::text_object_inner_paragraph_bridge(self)
    }

    fn text_object_around_paragraph(&self) -> Option<((usize, usize), (usize, usize))> {
        hjkl_engine::vim::text_object_around_paragraph_bridge(self)
    }

    fn text_object_inner_tag(&self) -> Option<((usize, usize), (usize, usize))> {
        hjkl_engine::vim::text_object_inner_tag_bridge(self)
    }

    fn text_object_around_tag(&self) -> Option<((usize, usize), (usize, usize))> {
        hjkl_engine::vim::text_object_around_tag_bridge(self)
    }

    // ─── Range-mutation primitives ─────────────────────────────────────────

    fn delete_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        kind: RangeKind,
        register: char,
    ) {
        hjkl_engine::vim::delete_range_bridge(self, start, end, kind, register);
    }

    fn yank_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        kind: RangeKind,
        register: char,
    ) {
        hjkl_engine::vim::yank_range_bridge(self, start, end, kind, register);
    }

    fn change_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        kind: RangeKind,
        register: char,
    ) {
        hjkl_engine::vim::change_range_bridge(self, start, end, kind, register);
    }

    fn indent_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        count: i32,
        shiftwidth: u32,
    ) {
        hjkl_engine::vim::indent_range_bridge(self, start, end, count, shiftwidth);
    }

    fn case_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        kind: RangeKind,
        op: Operator,
    ) {
        hjkl_engine::vim::case_range_bridge(self, start, end, kind, op);
    }

    // ─── Block-shape range-mutation primitives ─────────────────────────────

    fn delete_block(
        &mut self,
        top_row: usize,
        bot_row: usize,
        left_col: usize,
        right_col: usize,
        register: char,
    ) {
        hjkl_engine::vim::delete_block_bridge(
            self, top_row, bot_row, left_col, right_col, register,
        );
    }

    fn yank_block(
        &mut self,
        top_row: usize,
        bot_row: usize,
        left_col: usize,
        right_col: usize,
        register: char,
    ) {
        hjkl_engine::vim::yank_block_bridge(self, top_row, bot_row, left_col, right_col, register);
    }

    fn change_block(
        &mut self,
        top_row: usize,
        bot_row: usize,
        left_col: usize,
        right_col: usize,
        register: char,
    ) {
        hjkl_engine::vim::change_block_bridge(
            self, top_row, bot_row, left_col, right_col, register,
        );
    }

    fn indent_block(
        &mut self,
        top_row: usize,
        bot_row: usize,
        _left_col: usize,
        _right_col: usize,
        count: i32,
    ) {
        hjkl_engine::vim::indent_block_bridge(self, top_row, bot_row, count);
    }

    fn auto_indent_range(&mut self, start: (usize, usize), end: (usize, usize)) {
        hjkl_engine::vim::auto_indent_range_bridge(self, start, end);
    }
}
