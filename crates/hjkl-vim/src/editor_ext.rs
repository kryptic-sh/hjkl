//! [`VimEditorExt`] — vim-discipline accessor methods on the engine
//! [`Editor`], migrated out of `hjkl-engine` (#267 / #265 G3).
//!
//! These read the vim FSM state (`Editor::vim`) to answer render/selection
//! questions. They belong to the vim *discipline*, not the mode-agnostic
//! engine core, so they live here — a blanket trait impl on
//! `Editor<Buffer, H>`. As `VimState` finishes relocating into this crate,
//! more of the engine's vim accessors move onto this trait; call sites pick
//! them up with `use hjkl_vim::VimEditorExt`.

use hjkl_engine::input::Input;
use hjkl_engine::types::{Highlight, HighlightKind, Host, Pos};
use hjkl_engine::vim::{LastVisual, Operator, RangeKind, ScrollDir, SearchPrompt};
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

    // ─── Paste ─────────────────────────────────────────────────────────────

    /// `p` — paste the unnamed register (or the register selected via `"r`)
    /// after the cursor. Linewise content opens a new line below; charwise
    /// content is inserted inline. Records `Paste { before: false }` for `.`.
    fn paste_after(&mut self, count: usize);

    /// `P` — paste the unnamed register (or the `"r` register) before the
    /// cursor. Linewise content opens a new line above; charwise is inline.
    /// Records `Paste { before: true }` for dot-repeat.
    fn paste_before(&mut self, count: usize);

    /// `gp` / `gP` — paste like `p`/`P` but leave the cursor just after the
    /// pasted text. `before = true` for `gP`.
    fn paste_cursor_after(&mut self, before: bool, count: usize);

    /// `]p` / `[p` — linewise paste with the pasted block reindented to match
    /// the current line. `before = true` for `[p`.
    fn paste_reindent(&mut self, before: bool, count: usize);

    /// Visual-mode `p` / `P` — replace the active selection with the register.
    /// `before = true` for `P` (preserves the source register).
    fn visual_paste(&mut self, before: bool);

    // ─── Visual-mode operators ─────────────────────────────────────────────

    /// Visual-mode `<C-a>`/`<C-x>` (uniform) and `g<C-a>`/`g<C-x>`
    /// (`sequential`) — adjust the first number on each selected line.
    fn adjust_number_visual(&mut self, delta: i64, sequential: bool);

    /// Normal-mode `&` — repeat the last `:s` on the current line (no flags).
    fn ampersand_repeat(&mut self);

    /// Visual-mode `J` (`with_space = true`) / `gJ` (`false`) — join the
    /// selected lines into one.
    fn visual_join(&mut self, with_space: bool);

    /// `[count]%` — jump to the line at `count` percent of the file.
    fn goto_percent(&mut self, count: usize);

    // ─── Jumplist motion ───────────────────────────────────────────────────

    /// `<C-o>` — jump back `count` entries in the jumplist, saving the current
    /// position on the forward stack so `<C-i>` can return.
    fn jump_back(&mut self, count: usize);

    /// `<C-i>` / `Tab` — redo `count` entries on the forward jumplist stack,
    /// saving the current position on the backward stack.
    fn jump_forward(&mut self, count: usize);

    // ─── Scrolling ─────────────────────────────────────────────────────────

    /// `<C-f>` / `<C-b>` — scroll the cursor by one full viewport height
    /// (height − 2 rows, preserving two-line overlap). `count` multiplies.
    /// `dir = Down` for `<C-f>`, `Up` for `<C-b>`.
    fn scroll_full_page(&mut self, dir: ScrollDir, count: usize);

    /// `<C-d>` / `<C-u>` — scroll the cursor by half the viewport height.
    /// `count` multiplies the step. `dir = Down` for `<C-d>`, `Up` for `<C-u>`.
    fn scroll_half_page(&mut self, dir: ScrollDir, count: usize);

    /// `<C-e>` / `<C-y>` — scroll the viewport `count` lines without moving the
    /// cursor (cursor is clamped to the new visible region if necessary).
    /// `dir = Down` for `<C-e>` (scroll text up), `Up` for `<C-y>`.
    fn scroll_line(&mut self, dir: ScrollDir, count: usize);

    // ─── Search ────────────────────────────────────────────────────────────

    /// `n` — repeat the last `/` or `?` search `count` times in its original
    /// direction. `forward = true` keeps the direction; `false` inverts (`N`).
    fn search_repeat(&mut self, forward: bool, count: usize);

    /// `*` / `#` / `g*` / `g#` — search for the word under the cursor.
    /// `forward` chooses direction; `whole_word` wraps the pattern in `\b`
    /// anchors (true for `*` / `#`, false for `g*` / `g#`). `count` repeats.
    fn word_search(&mut self, forward: bool, whole_word: bool, count: usize);

    // ─── Chord appliers ────────────────────────────────────────────────────
    //
    // Each applies a completed chord with a pre-captured count, so the
    // pending-state reducers can dispatch without re-entering the engine FSM.

    /// `r<x>` — replace the char under the cursor with `ch`, `count` times.
    /// Cursor ends on the last replaced char; one undo snapshot at start.
    fn replace_char_at(&mut self, ch: char, count: usize);

    /// `f`/`F`/`t`/`T` — find `ch` on the current line. `forward` chooses
    /// direction, `till` stops one char short. Records `last_find` for `;`/`,`.
    fn find_char(&mut self, ch: char, forward: bool, till: bool, count: usize);

    /// Apply the g-chord effect for `g<ch>` with a pre-captured `count`.
    fn after_g(&mut self, ch: char, count: usize);

    /// Apply the z-chord effect for `z<ch>` with a pre-captured `count` —
    /// `zz`/`zt`/`zb` (scroll-cursor), the fold ops, and `zf`.
    fn after_z(&mut self, ch: char, count: usize);

    // ─── Operator dispatch ─────────────────────────────────────────────────

    /// Apply an operator over a single-key motion (e.g. `dw`, `d$`, `dG`).
    /// The engine resolves `motion_key` to a `Motion` via `parse_motion`.
    /// `total_count` is the folded product of prefix and inner counts. No-op
    /// when `motion_key` is not a known motion (vim cancels the operator).
    fn apply_op_motion(&mut self, op: Operator, motion_key: char, total_count: usize);

    /// Apply a doubled-letter line op (`dd` / `yy` / `cc` / `>>` / `<<`).
    fn apply_op_double(&mut self, op: Operator, total_count: usize);

    /// Apply an operator over a find motion (`df<x>` / `dF<x>` / `dt<x>` /
    /// `dT<x>`). Records `last_find` for `;` / `,` repeat and updates
    /// `last_change` when `op` is Change (dot-repeat).
    fn apply_op_find(&mut self, op: Operator, ch: char, forward: bool, till: bool, count: usize);

    /// Apply an operator over a text-object range (`diw` / `daw` / `di"` …).
    /// Unknown `ch` values are silently ignored, matching the FSM.
    fn apply_op_text_obj(&mut self, op: Operator, ch: char, inner: bool, total_count: usize);

    /// Apply an operator over a g-chord motion or case-op linewise form
    /// (`dgg` / `dge` / `dgE` / `dgj` / `dgk` / `gUgU` …).
    fn apply_op_g(&mut self, op: Operator, ch: char, total_count: usize);

    // ─── Mode transitions ──────────────────────────────────────────────────
    //
    // Both the FSM and these wrappers write `current_mode`, so `vim_mode()`
    // returns correct values regardless of which path performed the
    // transition.

    /// `v` from Normal — enter charwise Visual mode, anchoring the selection
    /// at the current cursor position.
    fn enter_visual_char(&mut self);

    /// `V` from Normal — enter linewise Visual mode, anchoring on the current
    /// line. Motions extend the selection by whole lines.
    fn enter_visual_line(&mut self);

    /// `<C-v>` from Normal — enter Visual-block mode. The selection is a
    /// rectangle whose corners are the anchor and the live cursor.
    fn enter_visual_block(&mut self);

    /// Esc from any visual mode — set `<` / `>` marks, stash the selection for
    /// `gv` re-entry, then return to Normal mode.
    fn exit_visual_to_normal(&mut self);

    /// `o` in Visual / VisualLine / VisualBlock — swap the cursor and anchor so
    /// the user can extend the other end of the selection. Does NOT mutate the
    /// selection range; only the active endpoint changes.
    fn visual_o_toggle(&mut self);

    /// `gv` — restore the last visual selection (mode + anchor + cursor
    /// position). No-op when no visual selection has been exited yet.
    fn reenter_last_visual(&mut self);

    /// Direct mode-transition entry point. Sets both the internal FSM mode and
    /// the stable `current_mode` field read by `vim_mode()`.
    ///
    /// Prefer the semantic primitives (`enter_visual_char`, `enter_insert_i`,
    /// …) which also set up required bookkeeping (anchors, sessions, …). Use
    /// `set_mode` only when you need a raw mode flip without side-effects.
    fn set_mode(&mut self, mode: VimMode);

    // ─── Visual anchors ────────────────────────────────────────────────────

    /// The charwise Visual-mode anchor `(row, col)`.
    fn visual_anchor(&self) -> (usize, usize);
    /// Set the charwise Visual-mode anchor.
    fn set_visual_anchor(&mut self, anchor: (usize, usize));
    /// The linewise Visual-mode anchor row.
    fn visual_line_anchor(&self) -> usize;
    /// Set the linewise Visual-mode anchor row.
    fn set_visual_line_anchor(&mut self, row: usize);
    /// The VisualBlock anchor `(row, col)`.
    fn block_anchor(&self) -> (usize, usize);
    /// Set the VisualBlock anchor.
    fn set_block_anchor(&mut self, anchor: (usize, usize));
    /// The VisualBlock sticky (virtual) column.
    fn block_vcol(&self) -> usize;
    /// Set the VisualBlock sticky (virtual) column.
    fn set_block_vcol(&mut self, vcol: usize);

    // ─── Yank / register staging ───────────────────────────────────────────

    /// Whether the last yank/delete was linewise.
    fn yank_linewise(&self) -> bool;
    /// Set the linewise flag for the next register write.
    fn set_yank_linewise(&mut self, v: bool);
    /// Set the pending `"r` register selector without consuming it.
    fn set_pending_register_raw(&mut self, reg: Option<char>);
    /// Take (and clear) the pending `"r` register selector.
    fn take_pending_register_raw(&mut self) -> Option<char>;

    // ─── Macro recording / replay ──────────────────────────────────────────

    /// Register currently being recorded into via `q{reg}`, if any.
    fn recording_macro(&self) -> Option<char>;
    /// Set (or clear) the register being recorded into.
    fn set_recording_macro(&mut self, reg: Option<char>);
    /// Append an input to the in-flight macro recording.
    fn push_recording_key(&mut self, input: Input);
    /// Take (and clear) the recorded macro keys.
    fn take_recording_keys(&mut self) -> Vec<Input>;
    /// Replace the recorded macro keys wholesale.
    fn set_recording_keys(&mut self, keys: Vec<Input>);
    /// Number of keys recorded so far.
    fn recording_keys_len(&self) -> usize;
    /// Whether a macro is currently being replayed.
    fn is_replaying_macro_raw(&self) -> bool;
    /// Set the macro-replay flag.
    fn set_replaying_macro_raw(&mut self, v: bool);
    /// The last macro register played, for `@@`.
    fn last_macro(&self) -> Option<char>;
    /// Set the last macro register played.
    fn set_last_macro(&mut self, reg: Option<char>);

    // ─── Last insert / visual / viewport ───────────────────────────────────

    /// Position where the last insert session ended (`gi`).
    fn last_insert_pos(&self) -> Option<(usize, usize)>;
    /// Set the last insert-session end position.
    fn set_last_insert_pos(&mut self, pos: Option<(usize, usize)>);
    /// Snapshot of the last visual selection, for `gv`.
    fn last_visual(&self) -> Option<LastVisual>;
    /// Set the last-visual snapshot.
    fn set_last_visual(&mut self, snap: Option<LastVisual>);
    /// Whether the viewport is pinned (suppresses scroll-follow).
    fn viewport_pinned(&self) -> bool;
    /// Set the viewport-pinned flag.
    fn set_viewport_pinned(&mut self, v: bool);
    /// Whether `Ctrl-R` is armed and awaiting a register name.
    fn insert_pending_register(&self) -> bool;
    /// Set the `Ctrl-R` pending-register flag.
    fn set_insert_pending_register(&mut self, v: bool);

    // ─── Change-mark start ─────────────────────────────────────────────────

    /// The stashed `[` mark start for a Change operation, or `None`.
    fn change_mark_start(&self) -> Option<(usize, usize)>;
    /// Take (and clear) the stashed `[` mark start.
    fn take_change_mark_start(&mut self) -> Option<(usize, usize)>;
    /// Set the stashed `[` mark start.
    fn set_change_mark_start(&mut self, pos: Option<(usize, usize)>);

    // ─── Input timing (for `timeoutlen` chord resolution) ──────────────────

    /// Instant of the last input, when the host supplies a monotonic clock.
    fn last_input_at(&self) -> Option<std::time::Instant>;
    /// Set the instant of the last input.
    fn set_last_input_at(&mut self, t: Option<std::time::Instant>);
    /// Host-supplied elapsed time at the last input (no_std hosts).
    fn last_input_host_at(&self) -> Option<core::time::Duration>;
    /// Set the host-supplied elapsed time at the last input.
    fn set_last_input_host_at(&mut self, d: Option<core::time::Duration>);

    // ─── Search prompt / history ───────────────────────────────────────────

    /// The live `/` or `?` search-prompt state, if a prompt is open.
    fn search_prompt_state(&self) -> Option<&SearchPrompt>;
    /// Mutable access to the live search-prompt state.
    fn search_prompt_state_mut(&mut self) -> Option<&mut SearchPrompt>;
    /// Take (and close) the search-prompt state.
    fn take_search_prompt_state(&mut self) -> Option<SearchPrompt>;
    /// Install (or clear) the search-prompt state.
    fn set_search_prompt_state(&mut self, prompt: Option<SearchPrompt>);
    /// The last search pattern, for `n` / `N`.
    fn last_search_pattern(&self) -> Option<&str>;
    /// Set the last search pattern without touching direction or highlight.
    fn set_last_search_pattern_only(&mut self, pattern: Option<String>);
    /// Set the last search direction without touching the pattern.
    fn set_last_search_forward_only(&mut self, forward: bool);
    /// Read-only view of the `/` search history (oldest first).
    fn search_history(&self) -> &[String];
    /// Mutable access to the search history.
    fn search_history_mut(&mut self) -> &mut Vec<String>;
    /// Cursor position while walking search history with Up/Down.
    fn search_history_cursor(&self) -> Option<usize>;
    /// Set the search-history walk cursor.
    fn set_search_history_cursor(&mut self, idx: Option<usize>);

    // ─── Jumplist storage ──────────────────────────────────────────────────

    /// Read-only view of the jump-back stack.
    fn jump_back_list(&self) -> &[(usize, usize)];
    /// Mutable access to the jump-back stack.
    fn jump_back_list_mut(&mut self) -> &mut Vec<(usize, usize)>;
    /// Read-only view of the jump-forward stack.
    fn jump_fwd_list(&self) -> &[(usize, usize)];
    /// Mutable access to the jump-forward stack.
    fn jump_fwd_list_mut(&mut self) -> &mut Vec<(usize, usize)>;
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

    // ─── Paste ─────────────────────────────────────────────────────────────

    fn paste_after(&mut self, count: usize) {
        hjkl_engine::vim::paste_after_bridge(self, count);
    }

    fn paste_before(&mut self, count: usize) {
        hjkl_engine::vim::paste_before_bridge(self, count);
    }

    fn paste_cursor_after(&mut self, before: bool, count: usize) {
        hjkl_engine::vim::paste_bridge(self, before, count, true, false);
    }

    fn paste_reindent(&mut self, before: bool, count: usize) {
        hjkl_engine::vim::paste_bridge(self, before, count, false, true);
    }

    fn visual_paste(&mut self, before: bool) {
        hjkl_engine::vim::visual_paste(self, before);
    }

    // ─── Visual-mode operators ─────────────────────────────────────────────

    fn adjust_number_visual(&mut self, delta: i64, sequential: bool) {
        hjkl_engine::vim::adjust_number_visual(self, delta, sequential);
    }

    fn ampersand_repeat(&mut self) {
        hjkl_engine::vim::ampersand_repeat(self);
    }

    fn visual_join(&mut self, with_space: bool) {
        hjkl_engine::vim::visual_join(self, with_space);
    }

    fn goto_percent(&mut self, count: usize) {
        hjkl_engine::vim::goto_percent(self, count);
    }

    // ─── Jumplist motion ───────────────────────────────────────────────────

    fn jump_back(&mut self, count: usize) {
        hjkl_engine::vim::jump_back_bridge(self, count);
    }

    fn jump_forward(&mut self, count: usize) {
        hjkl_engine::vim::jump_forward_bridge(self, count);
    }

    // ─── Scrolling ─────────────────────────────────────────────────────────

    fn scroll_full_page(&mut self, dir: ScrollDir, count: usize) {
        hjkl_engine::vim::scroll_full_page_bridge(self, dir, count);
    }

    fn scroll_half_page(&mut self, dir: ScrollDir, count: usize) {
        hjkl_engine::vim::scroll_half_page_bridge(self, dir, count);
    }

    fn scroll_line(&mut self, dir: ScrollDir, count: usize) {
        hjkl_engine::vim::scroll_line_bridge(self, dir, count);
    }

    // ─── Search ────────────────────────────────────────────────────────────

    fn search_repeat(&mut self, forward: bool, count: usize) {
        hjkl_engine::vim::search_repeat_bridge(self, forward, count);
    }

    fn word_search(&mut self, forward: bool, whole_word: bool, count: usize) {
        hjkl_engine::vim::word_search_bridge(self, forward, whole_word, count);
    }

    // ─── Chord appliers ────────────────────────────────────────────────────

    fn replace_char_at(&mut self, ch: char, count: usize) {
        hjkl_engine::vim::replace_char(self, ch, count);
    }

    fn find_char(&mut self, ch: char, forward: bool, till: bool, count: usize) {
        hjkl_engine::vim::apply_find_char(self, ch, forward, till, count.max(1));
    }

    fn after_g(&mut self, ch: char, count: usize) {
        hjkl_engine::vim::apply_after_g(self, ch, count);
    }

    fn after_z(&mut self, ch: char, count: usize) {
        hjkl_engine::vim::apply_after_z(self, ch, count);
    }

    // ─── Operator dispatch ─────────────────────────────────────────────────

    fn apply_op_motion(&mut self, op: Operator, motion_key: char, total_count: usize) {
        hjkl_engine::vim::apply_op_motion_key(self, op, motion_key, total_count);
    }

    fn apply_op_double(&mut self, op: Operator, total_count: usize) {
        hjkl_engine::vim::apply_op_double(self, op, total_count);
    }

    fn apply_op_find(&mut self, op: Operator, ch: char, forward: bool, till: bool, count: usize) {
        hjkl_engine::vim::apply_op_find_motion(self, op, ch, forward, till, count);
    }

    fn apply_op_text_obj(&mut self, op: Operator, ch: char, inner: bool, total_count: usize) {
        hjkl_engine::vim::apply_op_text_obj_inner(self, op, ch, inner, total_count);
    }

    fn apply_op_g(&mut self, op: Operator, ch: char, total_count: usize) {
        hjkl_engine::vim::apply_op_g_inner(self, op, ch, total_count);
    }

    // ─── Mode transitions ──────────────────────────────────────────────────

    fn enter_visual_char(&mut self) {
        hjkl_engine::vim::enter_visual_char_bridge(self);
    }

    fn enter_visual_line(&mut self) {
        hjkl_engine::vim::enter_visual_line_bridge(self);
    }

    fn enter_visual_block(&mut self) {
        hjkl_engine::vim::enter_visual_block_bridge(self);
    }

    fn exit_visual_to_normal(&mut self) {
        hjkl_engine::vim::exit_visual_to_normal_bridge(self);
    }

    fn visual_o_toggle(&mut self) {
        hjkl_engine::vim::visual_o_toggle_bridge(self);
    }

    fn reenter_last_visual(&mut self) {
        hjkl_engine::vim::reenter_last_visual_bridge(self);
    }

    fn set_mode(&mut self, mode: VimMode) {
        hjkl_engine::vim::set_mode_bridge(self, mode);
    }

    // ─── Visual anchors ────────────────────────────────────────────────────

    fn visual_anchor(&self) -> (usize, usize) {
        self.vim.visual_anchor
    }
    fn set_visual_anchor(&mut self, anchor: (usize, usize)) {
        self.vim.visual_anchor = anchor;
    }
    fn visual_line_anchor(&self) -> usize {
        self.vim.visual_line_anchor
    }
    fn set_visual_line_anchor(&mut self, row: usize) {
        self.vim.visual_line_anchor = row;
    }
    fn block_anchor(&self) -> (usize, usize) {
        self.vim.block_anchor
    }
    fn set_block_anchor(&mut self, anchor: (usize, usize)) {
        self.vim.block_anchor = anchor;
    }
    fn block_vcol(&self) -> usize {
        self.vim.block_vcol
    }
    fn set_block_vcol(&mut self, vcol: usize) {
        self.vim.block_vcol = vcol;
    }

    // ─── Yank / register staging ───────────────────────────────────────────

    fn yank_linewise(&self) -> bool {
        self.vim.yank_linewise
    }
    fn set_yank_linewise(&mut self, v: bool) {
        self.vim.yank_linewise = v;
    }
    fn set_pending_register_raw(&mut self, reg: Option<char>) {
        self.vim.pending_register = reg;
    }
    fn take_pending_register_raw(&mut self) -> Option<char> {
        self.vim.pending_register.take()
    }

    // ─── Macro recording / replay ──────────────────────────────────────────

    fn recording_macro(&self) -> Option<char> {
        self.vim.recording_macro
    }
    fn set_recording_macro(&mut self, reg: Option<char>) {
        self.vim.recording_macro = reg;
    }
    fn push_recording_key(&mut self, input: Input) {
        self.vim.recording_keys.push(input);
    }
    fn take_recording_keys(&mut self) -> Vec<Input> {
        std::mem::take(&mut self.vim.recording_keys)
    }
    fn set_recording_keys(&mut self, keys: Vec<Input>) {
        self.vim.recording_keys = keys;
    }
    fn recording_keys_len(&self) -> usize {
        self.vim.recording_keys.len()
    }
    fn is_replaying_macro_raw(&self) -> bool {
        self.vim.replaying_macro
    }
    fn set_replaying_macro_raw(&mut self, v: bool) {
        self.vim.replaying_macro = v;
    }
    fn last_macro(&self) -> Option<char> {
        self.vim.last_macro
    }
    fn set_last_macro(&mut self, reg: Option<char>) {
        self.vim.last_macro = reg;
    }

    // ─── Last insert / visual / viewport ───────────────────────────────────

    fn last_insert_pos(&self) -> Option<(usize, usize)> {
        self.vim.last_insert_pos
    }
    fn set_last_insert_pos(&mut self, pos: Option<(usize, usize)>) {
        self.vim.last_insert_pos = pos;
    }
    fn last_visual(&self) -> Option<LastVisual> {
        self.vim.last_visual
    }
    fn set_last_visual(&mut self, snap: Option<LastVisual>) {
        self.vim.last_visual = snap;
    }
    fn viewport_pinned(&self) -> bool {
        self.vim.viewport_pinned
    }
    fn set_viewport_pinned(&mut self, v: bool) {
        self.vim.viewport_pinned = v;
    }
    fn insert_pending_register(&self) -> bool {
        self.vim.insert_pending_register
    }
    fn set_insert_pending_register(&mut self, v: bool) {
        self.vim.insert_pending_register = v;
    }

    // ─── Change-mark start ─────────────────────────────────────────────────

    fn change_mark_start(&self) -> Option<(usize, usize)> {
        self.vim.change_mark_start
    }
    fn take_change_mark_start(&mut self) -> Option<(usize, usize)> {
        self.vim.change_mark_start.take()
    }
    fn set_change_mark_start(&mut self, pos: Option<(usize, usize)>) {
        self.vim.change_mark_start = pos;
    }

    // ─── Input timing ──────────────────────────────────────────────────────

    fn last_input_at(&self) -> Option<std::time::Instant> {
        self.vim.last_input_at
    }
    fn set_last_input_at(&mut self, t: Option<std::time::Instant>) {
        self.vim.last_input_at = t;
    }
    fn last_input_host_at(&self) -> Option<core::time::Duration> {
        self.vim.last_input_host_at
    }
    fn set_last_input_host_at(&mut self, d: Option<core::time::Duration>) {
        self.vim.last_input_host_at = d;
    }

    // ─── Search prompt / history ───────────────────────────────────────────

    fn search_prompt_state(&self) -> Option<&SearchPrompt> {
        self.vim.search_prompt.as_ref()
    }
    fn search_prompt_state_mut(&mut self) -> Option<&mut SearchPrompt> {
        self.vim.search_prompt.as_mut()
    }
    fn take_search_prompt_state(&mut self) -> Option<SearchPrompt> {
        self.vim.search_prompt.take()
    }
    fn set_search_prompt_state(&mut self, prompt: Option<SearchPrompt>) {
        self.vim.search_prompt = prompt;
    }
    fn last_search_pattern(&self) -> Option<&str> {
        self.vim.last_search.as_deref()
    }
    fn set_last_search_pattern_only(&mut self, pattern: Option<String>) {
        self.vim.last_search = pattern;
    }
    fn set_last_search_forward_only(&mut self, forward: bool) {
        self.vim.last_search_forward = forward;
    }
    fn search_history(&self) -> &[String] {
        &self.vim.search_history
    }
    fn search_history_mut(&mut self) -> &mut Vec<String> {
        &mut self.vim.search_history
    }
    fn search_history_cursor(&self) -> Option<usize> {
        self.vim.search_history_cursor
    }
    fn set_search_history_cursor(&mut self, idx: Option<usize>) {
        self.vim.search_history_cursor = idx;
    }

    // ─── Jumplist storage ──────────────────────────────────────────────────

    fn jump_back_list(&self) -> &[(usize, usize)] {
        &self.vim.jump_back
    }
    fn jump_back_list_mut(&mut self) -> &mut Vec<(usize, usize)> {
        &mut self.vim.jump_back
    }
    fn jump_fwd_list(&self) -> &[(usize, usize)] {
        &self.vim.jump_fwd
    }
    fn jump_fwd_list_mut(&mut self) -> &mut Vec<(usize, usize)> {
        &mut self.vim.jump_fwd
    }
}
