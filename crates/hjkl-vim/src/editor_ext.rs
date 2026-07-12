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
use hjkl_engine::vim::{
    AbbrevTrigger, InsertDir, InsertReason, LastVisual, Motion, Operator, RangeKind, TextObject,
};
use hjkl_engine::{Editor, FsmMode, MarkJump, VimMode};

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

/// Common post-mutation sync for the `insert_*` primitives.
///
/// The vim FSM's `step` runs `ensure_cursor_in_scrolloff` at the end of every
/// normal/visual motion; insert-mode primitives bypass `step` and must
/// self-correct or the cursor scrolls off the viewport (held Enter, multi-line
/// backspace at BOL, arrow keys at edge, etc.).
///
/// Marks the content dirty, widens the insert row's autoindent tracking, and
/// re-checks scrolloff. Was `Editor::after_insert_mutation` (#267) — it exists
/// only to serve the insert primitives, so it moved here with them.
fn after_insert_mutation<H: Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    ed.mark_content_dirty();
    let (row, _) = ed.cursor();
    ed.vim.widen_insert_row(row);
    ed.ensure_cursor_in_scrolloff();
}

/// Like [`after_insert_mutation`] but for cursor-only insert ops that do not
/// change content (arrows, Home/End, PageUp/Down). Skips the dirty mark.
fn after_insert_motion<H: Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    let (row, _) = ed.cursor();
    ed.vim.widen_insert_row(row);
    ed.ensure_cursor_in_scrolloff();
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

    /// The current vim mode (Normal / Insert / Visual / VisualLine / VisualBlock).
    fn vim_mode(&self) -> VimMode;

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

    // ─── Visual / motion / search primitives ───────────────────────────────
    //
    // Vim *semantics* — motions, operators over selections, block-edge insert,
    // search entry. These do not belong on a mode-agnostic rope editor; the
    // engine keeps the raw buffer primitives (cursor, line reads, edits) and
    // the vim discipline layers meaning on top (#265 / #267).

    /// `true` when the editor is in any visual mode (Visual / VisualLine /
    /// VisualBlock).
    fn is_visual(&self) -> bool;

    /// Apply `op` over `motion` with `count` repetitions, taking the full
    /// vim-quirks path (operator context for `l`, clamping, etc.).
    fn apply_op_with_motion_direct(&mut self, op: Operator, motion: &Motion, count: usize);

    /// `Ctrl-a` / `Ctrl-x` — adjust the number under or after the cursor.
    /// `delta = 1` increments, `-1` decrements; larger deltas multiply as in
    /// vim's `5<C-a>`.
    fn adjust_number(&mut self, delta: i64);

    /// Open the `/` or `?` search prompt. `forward = true` for `/`.
    fn enter_search(&mut self, forward: bool);

    /// `d/pat` / `c/pat` / `y/pat` — open the search prompt in operator-pending
    /// mode so the operator applies over the range to the match on commit.
    fn enter_search_op(&mut self, forward: bool, op: Operator, count: usize);

    /// Apply a pending operator-search over the exclusive charwise range from
    /// `origin` to the current cursor (the just-found match position).
    fn apply_op_search_range(&mut self, op: Operator, origin: (usize, usize));

    /// VisualBlock `I` — enter Insert at the left edge of the block.
    fn visual_block_insert_at_left(&mut self, top: usize, bot: usize, col: usize);

    /// VisualBlock `A` — enter Insert at the right edge of the block.
    fn visual_block_append_at_right(&mut self, top: usize, bot: usize, col: usize);

    /// Execute a motion, pushing to the jumplist for big jumps and updating the
    /// sticky column.
    fn execute_motion(&mut self, motion: Motion, count: usize);

    /// Update the VisualBlock virtual column after a motion. Horizontal motions
    /// sync `block_vcol` to the cursor column; vertical motions leave it alone
    /// so the intended column survives clamping to shorter rows.
    fn update_block_vcol(&mut self, motion: &Motion);

    /// Apply `op` over the current visual selection (char-wise, linewise, or
    /// block).
    fn apply_visual_operator(&mut self, op: Operator, count: usize);

    /// VisualBlock `r<ch>` — replace every character cell in the block with
    /// `ch`.
    fn replace_block_char(&mut self, ch: char);

    /// Visual-mode `i<ch>` / `a<ch>` — extend the selection to cover the text
    /// object identified by `ch`.
    fn visual_text_obj_extend(&mut self, ch: char, inner: bool);

    // ─── Insert-mode primitives ────────────────────────────────────────────
    //
    // Each wraps a `hjkl_engine::vim::insert_*_bridge` and, when the bridge
    // reports a mutation, runs the post-mutation sync (dirty mark, insert-row
    // widening, scrolloff correction). Callers must ensure the editor is in
    // Insert (or Replace) mode first.

    /// Insert `ch` at the cursor. In Replace mode, overstrike the cell under
    /// the cursor instead; at end-of-line, always appends. With `smartindent`,
    /// closing brackets trigger a one-unit dedent on an otherwise-whitespace
    /// line.
    fn insert_char(&mut self, ch: char);
    /// Insert a newline, applying autoindent / smartindent.
    fn insert_newline(&mut self);
    /// Insert a tab (or spaces to the next `softtabstop` boundary under
    /// `expandtab`).
    fn insert_tab(&mut self);
    /// Backspace. Deletes a whole soft-tab run at an aligned boundary under
    /// `softtabstop`; joins with the previous line at column 0.
    fn insert_backspace(&mut self);
    /// Delete the char under the cursor; joins with the next line at EOL.
    fn insert_delete(&mut self);
    /// Arrow-key motion in Insert, breaking the undo group per
    /// `undo_break_on_motion`.
    fn insert_arrow(&mut self, dir: InsertDir);
    /// Home in Insert.
    fn insert_home(&mut self);
    /// End in Insert.
    fn insert_end(&mut self);
    /// PageUp in Insert.
    fn insert_pageup(&mut self, viewport_h: u16);
    /// PageDown in Insert.
    fn insert_pagedown(&mut self, viewport_h: u16);
    /// `Ctrl-W` — delete the word before the cursor.
    fn insert_ctrl_w(&mut self);
    /// `Ctrl-U` — delete to the start of the line.
    fn insert_ctrl_u(&mut self);
    /// `Ctrl-H` — backspace equivalent.
    fn insert_ctrl_h(&mut self);
    /// `Ctrl-O` — arm a one-shot Normal-mode command.
    fn insert_ctrl_o_arm(&mut self);
    /// `Ctrl-R` — arm register paste; the next char names the register.
    fn insert_ctrl_r_arm(&mut self);
    /// `Ctrl-T` — indent the current line one `shiftwidth`.
    fn insert_ctrl_t(&mut self);
    /// `Ctrl-D` — dedent the current line one `shiftwidth`.
    fn insert_ctrl_d(&mut self);
    /// Paste register `reg` at the cursor (the `Ctrl-R` follow-up).
    fn insert_paste_register(&mut self, reg: char);
    /// `Ctrl-[` — expand any pending abbreviation (Esc-equivalent trigger).
    fn insert_ctrl_bracket(&mut self);
    /// Esc from Insert — end the insert session and return to Normal.
    fn leave_insert_to_normal(&mut self);

    // ─── Insert-mode entry ─────────────────────────────────────────────────

    /// `i` — insert before the cursor, `count` times on commit.
    fn enter_insert_i(&mut self, count: usize);
    /// `I` — insert at the first non-blank of the line.
    fn enter_insert_shift_i(&mut self, count: usize);
    /// `a` — append after the cursor.
    fn enter_insert_a(&mut self, count: usize);
    /// `A` — append at end-of-line.
    fn enter_insert_shift_a(&mut self, count: usize);
    /// `o` — open a new line below and insert.
    fn open_line_below(&mut self, count: usize);
    /// `O` — open a new line above and insert.
    fn open_line_above(&mut self, count: usize);
    /// `R` — enter Replace mode.
    fn enter_replace_mode(&mut self, count: usize);

    // ─── Normal-mode edit primitives ───────────────────────────────────────

    /// `x` — delete `count` chars forward.
    fn delete_char_forward(&mut self, count: usize);
    /// `X` — delete `count` chars backward.
    fn delete_char_backward(&mut self, count: usize);
    /// `s` — substitute `count` chars (delete then insert).
    fn substitute_char(&mut self, count: usize);
    /// `S` — substitute whole lines.
    fn substitute_line(&mut self, count: usize);
    /// `D` — delete to end-of-line.
    fn delete_to_eol(&mut self);
    /// `C` — change to end-of-line.
    fn change_to_eol(&mut self);
    /// `Y` — yank to end-of-line.
    fn yank_to_eol(&mut self, count: usize);
    /// `J` — join `count` lines.
    fn join_line(&mut self, count: usize);
    /// `~` — toggle case of `count` chars, advancing right.
    fn toggle_case_at_cursor(&mut self, count: usize);

    // ─── Vim mark commands ─────────────────────────────────────────────────
    //
    // Mark *storage* (`Editor::mark` / `set_mark` / `marks()` / `file_marks()`
    // / the global-mark map) stays on the engine: a mark is a positional
    // bookmark, which is an editor concern that other seams already consume
    // (hjkl-ex backs `:marks` and `'a` line addressing with it, and LSP /
    // quickfix / bookmark features could too).
    //
    // What lives here is the vim *command* layer on top of that storage — the
    // `m` / `'` / `` ` `` keybindings, which decide linewise vs charwise jump
    // and push the jumplist. That is vim semantics, not bookmark storage.

    /// `.` — dot-repeat: replay the last buffered change at the cursor. A
    /// non-zero `count` *replaces* the change's stored count (`:h .` — `3x`
    /// then `2.` deletes 2, not 6); `count == 0` means no explicit count.
    fn replay_last_change(&mut self, count: usize);

    /// `m{ch}` — record a mark named `ch` at the current cursor position.
    /// Invalid chars are silently ignored.
    fn set_mark_at_cursor(&mut self, ch: char);

    /// `'{ch}` — jump to mark `ch`, linewise (row, first non-blank). Pushes the
    /// pre-jump position onto the jumplist if the cursor actually moved.
    fn goto_mark_line(&mut self, ch: char);

    /// `` `{ch} `` — jump to mark `ch`, charwise (exact row + col). Pushes the
    /// pre-jump position onto the jumplist if the cursor actually moved.
    fn goto_mark_char(&mut self, ch: char);

    /// Like [`VimEditorExt::goto_mark_line`], but reports cross-buffer jumps:
    /// uppercase marks (`'A'`–`'Z'`) living in another buffer return
    /// [`MarkJump::CrossBuffer`] so the app can switch slots first.
    fn try_goto_mark_line(&mut self, ch: char) -> MarkJump;

    /// Charwise counterpart of [`VimEditorExt::try_goto_mark_line`].
    fn try_goto_mark_char(&mut self, ch: char) -> MarkJump;

    // ─── Vim FSM state accessors (pending chord, count, mode, macros) ──────
    //
    // The FSM in this crate reads and writes VimState through these. They are
    // pure vim state, so they belong here rather than on the mode-agnostic
    // engine core (#267).

    /// Return a clone of the current pending chord state.
    fn pending(&self) -> hjkl_engine::vim::Pending;

    /// Overwrite the pending chord state.
    fn set_pending(&mut self, p: hjkl_engine::vim::Pending);

    /// Atomically take the pending chord, replacing it with `Pending::None`.
    fn take_pending(&mut self) -> hjkl_engine::vim::Pending;

    /// Return the raw digit-prefix count (`0` = no prefix typed yet).
    fn count(&self) -> usize;

    /// Overwrite the digit-prefix count directly. Clamped at
    /// [`hjkl_engine::vim::MAX_COUNT`] (vim's documented count ceiling, `:h count`).
    fn set_count(&mut self, c: usize);

    /// Accumulate one more digit into the count prefix (mirrors `count * 10 + digit`).
    fn accumulate_count_digit(&mut self, digit: usize);

    /// Reset the count prefix to zero (no pending count).
    fn reset_count(&mut self);

    /// Consume the count and return it; resets to zero. Returns `1` when no
    /// prefix was typed (mirrors `take_count` in vim.rs).
    fn take_count(&mut self) -> usize;

    /// Return the FSM-internal mode (Normal / Insert / Visual / …).
    fn fsm_mode(&self) -> hjkl_engine::vim::Mode;

    /// Overwrite the FSM-internal mode without side-effects. Prefer the
    /// semantic primitives (`enter_insert_i`, `enter_visual_char`, …).
    fn set_fsm_mode(&mut self, m: hjkl_engine::vim::Mode);

    /// `true` while the `.` dot-repeat replay is running.
    fn is_replaying(&self) -> bool;

    /// Set or clear the dot-replay flag.
    fn set_replaying(&mut self, v: bool);

    /// `true` when we entered Normal from Insert via `Ctrl-o` and will return
    /// to Insert after the next complete command.
    fn is_one_shot_normal(&self) -> bool;

    /// Set or clear the Ctrl-o one-shot-normal flag.
    fn set_one_shot_normal(&mut self, v: bool);

    /// Return the last `f`/`F`/`t`/`T` target as `(char, forward, till)`, or
    /// `None` before any find command was executed.
    fn last_find(&self) -> Option<(char, bool, bool)>;

    /// Overwrite the stored last-find target.
    fn set_last_find(&mut self, target: Option<(char, bool, bool)>);

    /// Perform a vim-sneak style two-char digraph jump. Scans the buffer
    /// from the current cursor for the `count`-th occurrence of `c1+c2`.
    /// `forward=true` searches ahead; `forward=false` searches backward.
    /// Respects `Settings::motion_sneak` — callers (hjkl-vim FSM) should
    /// already gate on the setting; this method always executes the sneak.
    fn sneak(&mut self, c1: char, c2: char, forward: bool, count: usize);

    /// Apply an operator over a sneak digraph range. Charwise exclusive —
    /// deletes from cursor up to (not including) the first char of the match.
    fn apply_op_sneak(
        &mut self,
        op: hjkl_engine::vim::Operator,
        c1: char,
        c2: char,
        forward: bool,
        total_count: usize,
    );

    /// Return the last sneak digraph and direction stored after a sneak motion.
    /// `Some(((c1, c2), forward))` when a sneak has been performed this session;
    /// `None` before any sneak. Used by `;`/`,` repeat and tests.
    fn last_sneak(&self) -> Option<((char, char), bool)>;

    /// Return a clone of the last recorded mutating change, or `None` before
    /// any change has been made.
    fn last_change(&self) -> Option<hjkl_engine::vim::LastChange>;

    /// Overwrite the stored last-change record.
    fn set_last_change(&mut self, lc: Option<hjkl_engine::vim::LastChange>);

    /// Borrow the last-change record mutably (e.g. to fill in an `inserted`
    /// field after the insert session completes).
    fn last_change_mut(&mut self) -> Option<&mut hjkl_engine::vim::LastChange>;

    /// Borrow the active insert session, or `None` when not in Insert mode.
    fn insert_session(&self) -> Option<&hjkl_engine::vim::InsertSession>;

    /// Borrow the active insert session mutably.
    fn insert_session_mut(&mut self) -> Option<&mut hjkl_engine::vim::InsertSession>;

    /// Atomically take the insert session out, leaving `None`.
    fn take_insert_session(&mut self) -> Option<hjkl_engine::vim::InsertSession>;

    /// Install a new insert session, replacing any existing one.
    fn set_insert_session(&mut self, s: Option<hjkl_engine::vim::InsertSession>);

    // ─── Register selection / chord status / macro controller ──────────────

    /// Return the user's pending register selection (set via `"<reg>` chord
    /// before an operator). `None` if no register was selected — caller should
    /// use the unnamed register `"`.
    ///
    /// Read-only — does not consume / clear the pending selection. The
    /// register is cleared by the engine after the next operator fires.
    ///
    /// Promoted in 0.6.X for Phase 4e to let the App's visual-op dispatch arm
    /// honor `"a` + visual op chord sequences.
    fn pending_register(&self) -> Option<char>;

    /// True when the user's pending register selector is `+` or `*`.
    /// the host peeks this so it can refresh `sync_clipboard_register`
    /// only when a clipboard read is actually about to happen.
    fn pending_register_is_clipboard(&self) -> bool;

    /// Register currently being recorded into via `q{reg}`. `None` when
    /// no recording is active. Hosts use this to surface a "recording @r"
    /// indicator in the status line.
    fn recording_register(&self) -> Option<char>;

    /// Pending repeat count the user has typed but not yet resolved
    /// (e.g. pressing `5` before `d`). `None` when nothing is pending.
    /// Hosts surface this in a "showcmd" area.
    fn pending_count(&self) -> Option<u32>;

    /// The operator character for any in-flight operator that is waiting
    /// for a motion (e.g. `d` after the user types `d` but before a
    /// motion). Returns `None` when no operator is pending.
    fn pending_op(&self) -> Option<char>;

    /// `true` when the engine is in any pending chord state — waiting for
    /// the next key to complete a command (e.g. `r<char>` replace,
    /// `f<char>` find, `m<a>` set-mark, `'<a>` goto-mark, operator-pending
    /// after `d` / `c` / `y`, `g`-prefix continuation, `z`-prefix continuation,
    /// register selection `"<reg>`, macro recording target, etc).
    ///
    /// Hosts use this to bypass their own chord dispatch (keymap tries, etc.)
    /// and forward keys directly to the engine so in-flight commands can
    /// complete without the host eating their continuation keys.
    fn is_chord_pending(&self) -> bool;

    /// `true` when `insert_ctrl_r_arm()` has been called and the dispatcher
    /// is waiting for the next typed character to name the register to paste.
    /// The dispatcher should call `insert_paste_register(c)` instead of
    /// `insert_char(c)` for the next printable key, then the flag auto-clears.
    ///
    /// Phase 6.5: exposed so the app-level `dispatch_insert_key` can branch
    /// without having to drive the full FSM.
    fn is_insert_register_pending(&self) -> bool;

    /// Clear the `Ctrl-R` register-paste pending flag. Call this immediately
    /// before `insert_paste_register(c)` in app-level dispatchers so that the
    /// flag does not persist into the next key. Call before
    /// `insert_paste_register_bridge` (which `hjkl_vim::insert` does).
    ///
    /// Phase 6.5: used by `dispatch_insert_key` in the app crate.
    fn clear_insert_register_pending(&mut self);

    /// Set `vim.pending_register` to `Some(reg)` if `reg` is a valid register
    /// selector (`a`–`z`, `A`–`Z`, `0`–`9`, `"`, `+`, `*`, `_`). Invalid
    /// chars are silently ignored (no-op), matching the engine FSM's
    /// `handle_select_register` behaviour.
    ///
    /// Promoted to the public surface in 0.5.17 so the hjkl-vim
    /// `PendingState::SelectRegister` reducer can dispatch `SetPendingRegister`
    /// without re-entering the engine FSM. `handle_select_register` (engine FSM
    /// path for macro-replay / defensive coverage) delegates here to avoid
    /// logic duplication.
    fn set_pending_register(&mut self, reg: char);

    /// Begin recording keystrokes into register `reg`. The caller (app) is
    /// responsible for stopping the recording via `stop_macro_record` when the
    /// user presses bare `q`.
    ///
    /// - Uppercase `reg` (e.g. `'A'`) appends to the existing lowercase
    ///   recording by pre-seeding `recording_keys` with the decoded text of the
    ///   matching lowercase register, matching vim's capital-register append
    ///   semantics.
    /// - Lowercase `reg` clears `recording_keys` (fresh recording).
    /// - Invalid chars (non-alphabetic, non-digit) are silently ignored.
    ///
    /// Promoted to the public surface in Phase 5b so the app's
    /// `route_chord_key` can start a recording without re-entering the engine
    /// FSM. `handle_record_macro_target` (engine FSM path for macro-replay
    /// defensive coverage) continues to use the same logic via delegation.
    fn start_macro_record(&mut self, reg: char);

    /// Finalize the active recording: encode `recording_keys` as text and write
    /// to the matching (lowercase) named register. Clears both `recording_macro`
    /// and `recording_keys`. No-ops if no recording is active.
    ///
    /// Promoted to the public surface in Phase 5b so the app's `QChord` action
    /// can stop a recording when the user presses bare `q` without re-entering
    /// the engine FSM.
    fn stop_macro_record(&mut self);

    /// Returns `true` while a `q{reg}` recording is in progress.
    /// Hosts use this to show a "recording @r" status indicator and to decide
    /// whether bare `q` should stop the recording or open the `RecordMacroTarget`
    /// chord.
    fn is_recording_macro(&self) -> bool;

    /// Returns `true` while a macro is being replayed. The app sets this flag
    /// (via `play_macro`) and clears it (via `end_macro_replay`) around the
    /// re-feed loop so the recorder hook can skip double-capture.
    fn is_replaying_macro(&self) -> bool;

    /// Decode the named register `reg` into a `Vec<hjkl_engine::input::Input>` and
    /// prepare for replay, returning the inputs the app should re-feed through
    /// `route_chord_key`.
    ///
    /// Resolves `reg`:
    /// - `'@'` → use `vim.last_macro`; returns empty vec if none.
    /// - Any other char → lowercase it, read the register, decode.
    ///
    /// Side-effects:
    /// - Sets `vim.last_macro` to the resolved register.
    /// - Sets `vim.replaying_macro = true` so the recorder hook skips during
    ///   replay. The app calls `end_macro_replay` after the loop finishes.
    ///
    /// Returns an empty vec (and no side-effects for `'@'`) if the register is
    /// unset or empty.
    fn play_macro(&mut self, reg: char, count: usize) -> Vec<hjkl_engine::input::Input>;

    /// Clear the `replaying_macro` flag. Called by the app after the
    /// re-feed loop in the `PlayMacro` commit arm completes (or aborts).
    fn end_macro_replay(&mut self);

    /// Append `input` to the active recording (`recording_keys`) if and only
    /// if a recording is in progress AND we are not currently replaying.
    /// Called by the app's `route_chord_key` recorder hook so that user
    /// keystrokes captured through the app-level chord path are recorded
    /// (rather than relying solely on the engine FSM's in-step hook).
    fn record_input(&mut self, input: hjkl_engine::input::Input);

    // ─── Mode reset / mouse-driven selection / operator range probe ────────

    /// Force back to Normal mode (used when dismissing completions etc.).
    fn force_normal(&mut self);

    /// Handle a left-button click at doc-space `(row, col)`. Exits Visual mode
    /// if active, breaks the insert-mode undo group (vim parity for
    /// `undo_break_on_motion`), then moves the cursor. The EOL clamp is
    /// mode-aware (neovim parity): Normal/Visual cap at `len - 1`, Insert
    /// allows the one-past-EOL position.
    fn mouse_click_doc(&mut self, row: usize, col: usize);

    /// Begin a mouse-drag selection: anchor at the cursor and enter
    /// Visual-char mode. Idempotent if already in Visual-char.
    fn mouse_begin_drag(&mut self);

    /// Dry-run `motion_key` and return the `(min_row, max_row)` span between
    /// the cursor row and the motion's target row, restoring the cursor
    /// afterwards. `None` when `motion_key` is not a known motion.
    fn range_for_op_motion(
        &mut self,
        motion_key: char,
        total_count: usize,
    ) -> Option<(usize, usize)>;
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

    fn vim_mode(&self) -> VimMode {
        self.vim.current_mode
    }

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

    // ─── Visual / motion / search primitives ───────────────────────────────

    fn is_visual(&self) -> bool {
        matches!(
            self.vim.mode,
            FsmMode::Visual | FsmMode::VisualLine | FsmMode::VisualBlock
        )
    }

    fn apply_op_with_motion_direct(&mut self, op: Operator, motion: &Motion, count: usize) {
        hjkl_engine::vim::apply_op_with_motion(self, op, motion, count);
    }

    fn adjust_number(&mut self, delta: i64) {
        hjkl_engine::vim::adjust_number(self, delta);
    }

    fn enter_search(&mut self, forward: bool) {
        hjkl_engine::vim::enter_search(self, forward);
    }

    fn enter_search_op(&mut self, forward: bool, op: Operator, count: usize) {
        hjkl_engine::vim::enter_search_op(self, forward, op, count);
    }

    fn apply_op_search_range(&mut self, op: Operator, origin: (usize, usize)) {
        hjkl_engine::vim::apply_op_search_range(self, op, origin);
    }

    fn visual_block_insert_at_left(&mut self, top: usize, bot: usize, col: usize) {
        self.jump_cursor(top, col);
        self.vim.mode = FsmMode::Normal;
        hjkl_engine::vim::begin_insert(self, 1, InsertReason::BlockEdge { top, bot, col });
    }

    fn visual_block_append_at_right(&mut self, top: usize, bot: usize, col: usize) {
        self.jump_cursor(top, col);
        self.vim.mode = FsmMode::Normal;
        hjkl_engine::vim::begin_insert(self, 1, InsertReason::BlockEdge { top, bot, col });
    }

    fn execute_motion(&mut self, motion: Motion, count: usize) {
        hjkl_engine::vim::execute_motion(self, motion, count);
    }

    fn update_block_vcol(&mut self, motion: &Motion) {
        hjkl_engine::vim::update_block_vcol(self, motion);
    }

    fn apply_visual_operator(&mut self, op: Operator, count: usize) {
        hjkl_engine::vim::apply_visual_operator(self, op, count);
    }

    fn replace_block_char(&mut self, ch: char) {
        hjkl_engine::vim::block_replace(self, ch);
    }

    fn visual_text_obj_extend(&mut self, ch: char, inner: bool) {
        let obj = match ch {
            'w' => TextObject::Word { big: false },
            'W' => TextObject::Word { big: true },
            '"' | '\'' | '`' => TextObject::Quote(ch),
            '(' | ')' | 'b' => TextObject::Bracket('('),
            '[' | ']' => TextObject::Bracket('['),
            '{' | '}' | 'B' => TextObject::Bracket('{'),
            '<' | '>' => TextObject::Bracket('<'),
            'p' => TextObject::Paragraph,
            't' => TextObject::XmlTag,
            's' => TextObject::Sentence,
            _ => return,
        };
        let Some((start, end, kind)) = hjkl_engine::vim::text_object_range(self, obj, inner, 1)
        else {
            return;
        };
        match kind {
            RangeKind::Linewise => {
                self.vim.visual_line_anchor = start.0;
                self.vim.mode = FsmMode::VisualLine;
                self.vim.current_mode = VimMode::VisualLine;
                self.jump_cursor(end.0, 0);
            }
            _ => {
                self.vim.mode = FsmMode::Visual;
                self.vim.current_mode = VimMode::Visual;
                self.vim.visual_anchor = (start.0, start.1);
                let (er, ec) = hjkl_engine::vim::retreat_one(self, end);
                self.jump_cursor(er, ec);
            }
        }
    }

    // ─── Insert-mode primitives ────────────────────────────────────────────

    fn insert_char(&mut self, ch: char) {
        if hjkl_engine::vim::insert_char_bridge(self, ch) {
            after_insert_mutation(self);
        }
    }

    fn insert_newline(&mut self) {
        if hjkl_engine::vim::insert_newline_bridge(self) {
            after_insert_mutation(self);
        }
    }

    fn insert_tab(&mut self) {
        if hjkl_engine::vim::insert_tab_bridge(self) {
            after_insert_mutation(self);
        }
    }

    fn insert_backspace(&mut self) {
        if hjkl_engine::vim::insert_backspace_bridge(self) {
            after_insert_mutation(self);
        }
    }

    fn insert_delete(&mut self) {
        if hjkl_engine::vim::insert_delete_bridge(self) {
            after_insert_mutation(self);
        }
    }

    fn insert_arrow(&mut self, dir: InsertDir) {
        hjkl_engine::vim::insert_arrow_bridge(self, dir);
        after_insert_motion(self);
    }

    fn insert_home(&mut self) {
        hjkl_engine::vim::insert_home_bridge(self);
        after_insert_motion(self);
    }

    fn insert_end(&mut self) {
        hjkl_engine::vim::insert_end_bridge(self);
        after_insert_motion(self);
    }

    fn insert_pageup(&mut self, viewport_h: u16) {
        hjkl_engine::vim::insert_pageup_bridge(self, viewport_h);
        after_insert_motion(self);
    }

    fn insert_pagedown(&mut self, viewport_h: u16) {
        hjkl_engine::vim::insert_pagedown_bridge(self, viewport_h);
        after_insert_motion(self);
    }

    fn insert_ctrl_w(&mut self) {
        if hjkl_engine::vim::insert_ctrl_w_bridge(self) {
            after_insert_mutation(self);
        }
    }

    fn insert_ctrl_u(&mut self) {
        if hjkl_engine::vim::insert_ctrl_u_bridge(self) {
            after_insert_mutation(self);
        }
    }

    fn insert_ctrl_h(&mut self) {
        if hjkl_engine::vim::insert_ctrl_h_bridge(self) {
            after_insert_mutation(self);
        }
    }

    fn insert_ctrl_o_arm(&mut self) {
        hjkl_engine::vim::insert_ctrl_o_bridge(self);
    }

    fn insert_ctrl_r_arm(&mut self) {
        hjkl_engine::vim::insert_ctrl_r_bridge(self);
    }

    fn insert_ctrl_t(&mut self) {
        // Indent-only: no scrolloff re-check (the cursor row does not move).
        let mutated = hjkl_engine::vim::insert_ctrl_t_bridge(self);
        if mutated {
            self.mark_content_dirty();
            let (row, _) = self.cursor();
            self.vim.widen_insert_row(row);
        }
    }

    fn insert_ctrl_d(&mut self) {
        let mutated = hjkl_engine::vim::insert_ctrl_d_bridge(self);
        if mutated {
            self.mark_content_dirty();
            let (row, _) = self.cursor();
            self.vim.widen_insert_row(row);
        }
    }

    fn insert_paste_register(&mut self, reg: char) {
        hjkl_engine::vim::insert_paste_register_bridge(self, reg);
        let (row, _) = self.cursor();
        self.vim.widen_insert_row(row);
    }

    fn insert_ctrl_bracket(&mut self) {
        if hjkl_engine::vim::check_and_apply_abbrev(self, AbbrevTrigger::CtrlBracket) {
            after_insert_mutation(self);
        }
    }

    fn leave_insert_to_normal(&mut self) {
        hjkl_engine::vim::leave_insert_to_normal_bridge(self);
    }

    // ─── Insert-mode entry ─────────────────────────────────────────────────

    fn enter_insert_i(&mut self, count: usize) {
        hjkl_engine::vim::enter_insert_i_bridge(self, count);
    }

    fn enter_insert_shift_i(&mut self, count: usize) {
        hjkl_engine::vim::enter_insert_shift_i_bridge(self, count);
    }

    fn enter_insert_a(&mut self, count: usize) {
        hjkl_engine::vim::enter_insert_a_bridge(self, count);
    }

    fn enter_insert_shift_a(&mut self, count: usize) {
        hjkl_engine::vim::enter_insert_shift_a_bridge(self, count);
    }

    fn open_line_below(&mut self, count: usize) {
        hjkl_engine::vim::open_line_below_bridge(self, count);
    }

    fn open_line_above(&mut self, count: usize) {
        hjkl_engine::vim::open_line_above_bridge(self, count);
    }

    fn enter_replace_mode(&mut self, count: usize) {
        hjkl_engine::vim::enter_replace_mode_bridge(self, count);
    }

    // ─── Normal-mode edit primitives ───────────────────────────────────────

    fn delete_char_forward(&mut self, count: usize) {
        hjkl_engine::vim::delete_char_forward_bridge(self, count);
    }

    fn delete_char_backward(&mut self, count: usize) {
        hjkl_engine::vim::delete_char_backward_bridge(self, count);
    }

    fn substitute_char(&mut self, count: usize) {
        hjkl_engine::vim::substitute_char_bridge(self, count);
    }

    fn substitute_line(&mut self, count: usize) {
        hjkl_engine::vim::substitute_line_bridge(self, count);
    }

    fn delete_to_eol(&mut self) {
        hjkl_engine::vim::delete_to_eol_bridge(self);
    }

    fn change_to_eol(&mut self) {
        hjkl_engine::vim::change_to_eol_bridge(self);
    }

    fn yank_to_eol(&mut self, count: usize) {
        hjkl_engine::vim::yank_to_eol_bridge(self, count);
    }

    fn join_line(&mut self, count: usize) {
        hjkl_engine::vim::join_line_bridge(self, count);
    }

    fn toggle_case_at_cursor(&mut self, count: usize) {
        hjkl_engine::vim::toggle_case_at_cursor_bridge(self, count);
    }

    // ─── Vim mark commands ─────────────────────────────────────────────────

    fn replay_last_change(&mut self, count: usize) {
        hjkl_engine::vim::replay_last_change(self, count);
    }

    fn set_mark_at_cursor(&mut self, ch: char) {
        hjkl_engine::vim::set_mark_at_cursor(self, ch);
    }

    fn goto_mark_line(&mut self, ch: char) {
        hjkl_engine::vim::goto_mark(self, ch, true);
    }

    fn goto_mark_char(&mut self, ch: char) {
        hjkl_engine::vim::goto_mark(self, ch, false);
    }

    fn try_goto_mark_line(&mut self, ch: char) -> MarkJump {
        hjkl_engine::vim::try_goto_mark(self, ch, true)
    }

    fn try_goto_mark_char(&mut self, ch: char) -> MarkJump {
        hjkl_engine::vim::try_goto_mark(self, ch, false)
    }

    // ─── Vim FSM state accessors ───────────────────────────────────────────

    fn pending(&self) -> hjkl_engine::vim::Pending {
        self.vim.pending.clone()
    }

    fn set_pending(&mut self, p: hjkl_engine::vim::Pending) {
        self.vim.pending = p;
    }

    fn take_pending(&mut self) -> hjkl_engine::vim::Pending {
        std::mem::take(&mut self.vim.pending)
    }

    fn count(&self) -> usize {
        self.vim.count
    }

    fn set_count(&mut self, c: usize) {
        self.vim.count = c.min(hjkl_engine::vim::MAX_COUNT);
    }

    fn accumulate_count_digit(&mut self, digit: usize) {
        // Saturate the add too: once the multiply has saturated at
        // `usize::MAX`, a plain `+ digit` overflows (panic in debug builds)
        // after ~20 typed digits. Then clamp at vim's documented count
        // ceiling (`:h count`) so no apply loop can iterate more than
        // 999,999,999 times regardless of how many digits were typed.
        self.vim.count = self
            .vim
            .count
            .saturating_mul(10)
            .saturating_add(digit)
            .min(hjkl_engine::vim::MAX_COUNT);
    }

    fn reset_count(&mut self) {
        self.vim.count = 0;
    }

    fn take_count(&mut self) -> usize {
        if self.vim.count > 0 {
            let n = self.vim.count;
            self.vim.count = 0;
            n
        } else {
            1
        }
    }

    fn fsm_mode(&self) -> hjkl_engine::vim::Mode {
        self.vim.mode
    }

    fn set_fsm_mode(&mut self, m: hjkl_engine::vim::Mode) {
        self.vim.mode = m;
        self.vim.current_mode = self.vim.public_mode();
    }

    fn is_replaying(&self) -> bool {
        self.vim.replaying
    }

    fn set_replaying(&mut self, v: bool) {
        self.vim.replaying = v;
    }

    fn is_one_shot_normal(&self) -> bool {
        self.vim.one_shot_normal
    }

    fn set_one_shot_normal(&mut self, v: bool) {
        self.vim.one_shot_normal = v;
    }

    fn last_find(&self) -> Option<(char, bool, bool)> {
        self.vim.last_find
    }

    fn set_last_find(&mut self, target: Option<(char, bool, bool)>) {
        self.vim.last_find = target;
    }

    fn sneak(&mut self, c1: char, c2: char, forward: bool, count: usize) {
        hjkl_engine::vim::apply_sneak(self, c1, c2, forward, count.max(1));
    }

    fn apply_op_sneak(
        &mut self,
        op: hjkl_engine::vim::Operator,
        c1: char,
        c2: char,
        forward: bool,
        total_count: usize,
    ) {
        hjkl_engine::vim::apply_op_sneak(self, op, c1, c2, forward, total_count);
    }

    fn last_sneak(&self) -> Option<((char, char), bool)> {
        self.vim.last_sneak
    }

    fn last_change(&self) -> Option<hjkl_engine::vim::LastChange> {
        self.vim.last_change.clone()
    }

    fn set_last_change(&mut self, lc: Option<hjkl_engine::vim::LastChange>) {
        self.vim.last_change = lc;
    }

    fn last_change_mut(&mut self) -> Option<&mut hjkl_engine::vim::LastChange> {
        self.vim.last_change.as_mut()
    }

    fn insert_session(&self) -> Option<&hjkl_engine::vim::InsertSession> {
        self.vim.insert_session.as_ref()
    }

    fn insert_session_mut(&mut self) -> Option<&mut hjkl_engine::vim::InsertSession> {
        self.vim.insert_session.as_mut()
    }

    fn take_insert_session(&mut self) -> Option<hjkl_engine::vim::InsertSession> {
        self.vim.insert_session.take()
    }

    fn set_insert_session(&mut self, s: Option<hjkl_engine::vim::InsertSession>) {
        self.vim.insert_session = s;
    }

    // ─── Register selection / chord status / macro controller ──────────────

    fn pending_register(&self) -> Option<char> {
        self.vim.pending_register
    }

    fn pending_register_is_clipboard(&self) -> bool {
        matches!(self.vim.pending_register, Some('+') | Some('*'))
    }

    fn recording_register(&self) -> Option<char> {
        self.vim.recording_macro
    }

    fn pending_count(&self) -> Option<u32> {
        self.vim.pending_count_val()
    }

    fn pending_op(&self) -> Option<char> {
        self.vim.pending_op_char()
    }

    fn is_chord_pending(&self) -> bool {
        self.vim.is_chord_pending()
    }

    fn is_insert_register_pending(&self) -> bool {
        self.vim.insert_pending_register
    }

    fn clear_insert_register_pending(&mut self) {
        self.vim.insert_pending_register = false;
    }

    fn set_pending_register(&mut self, reg: char) {
        // `-` is the small-delete register (readable/pasteable, e.g. `"-p`).
        if reg.is_ascii_alphanumeric() || matches!(reg, '"' | '+' | '*' | '_' | '-') {
            self.vim.pending_register = Some(reg);
        }
        // Invalid chars silently no-op (matches engine FSM behavior).
    }

    fn start_macro_record(&mut self, reg: char) {
        if !(reg.is_ascii_alphabetic() || reg.is_ascii_digit()) {
            return;
        }
        self.vim.recording_macro = Some(reg);
        if reg.is_ascii_uppercase() {
            // Seed recording_keys with the existing lowercase register's text
            // decoded back to inputs so capital-register append continues from
            // where the previous recording left off.
            let lower = reg.to_ascii_lowercase();
            let text = self
                .registers()
                .read(lower)
                .map(|s| s.text.clone())
                .unwrap_or_default();
            self.vim.recording_keys = hjkl_engine::input::decode_macro(&text);
        } else {
            self.vim.recording_keys.clear();
        }
    }

    fn stop_macro_record(&mut self) {
        let Some(reg) = self.vim.recording_macro.take() else {
            return;
        };
        let keys = std::mem::take(&mut self.vim.recording_keys);
        let text = hjkl_engine::input::encode_macro(&keys);
        self.set_named_register_text(reg.to_ascii_lowercase(), text);
    }

    fn is_recording_macro(&self) -> bool {
        self.vim.recording_macro.is_some()
    }

    fn is_replaying_macro(&self) -> bool {
        self.vim.replaying_macro
    }

    fn play_macro(&mut self, reg: char, count: usize) -> Vec<hjkl_engine::input::Input> {
        let resolved = if reg == '@' {
            match self.vim.last_macro {
                Some(r) => r,
                None => return vec![],
            }
        } else {
            reg.to_ascii_lowercase()
        };
        let text = {
            let regs = self.registers();
            match regs.read(resolved) {
                Some(slot) if !slot.text.is_empty() => slot.text.clone(),
                _ => return vec![],
            }
        };
        let keys = hjkl_engine::input::decode_macro(&text);
        self.vim.last_macro = Some(resolved);
        self.vim.replaying_macro = true;
        // Multiply by count (minimum 1). Clamp to vim's count limit
        // (`:h count` — counts are capped at 999999999); an unclamped
        // saturated prefix would overflow `Vec` capacity in `repeat`.
        keys.repeat(count.clamp(1, hjkl_engine::vim::MAX_COUNT))
    }

    fn end_macro_replay(&mut self) {
        self.vim.replaying_macro = false;
    }

    fn record_input(&mut self, input: hjkl_engine::input::Input) {
        if self.vim.recording_macro.is_some() && !self.vim.replaying_macro {
            self.vim.recording_keys.push(input);
        }
    }

    // ─── Mode reset / mouse-driven selection / operator range probe ────────

    fn force_normal(&mut self) {
        hjkl_engine::vim::force_normal_bridge(self);
    }

    fn mouse_click_doc(&mut self, row: usize, col: usize) {
        hjkl_engine::vim::mouse_click_doc_bridge(self, row, col);
    }

    fn mouse_begin_drag(&mut self) {
        hjkl_engine::vim::mouse_begin_drag_bridge(self);
    }

    fn range_for_op_motion(
        &mut self,
        motion_key: char,
        total_count: usize,
    ) -> Option<(usize, usize)> {
        hjkl_engine::vim::range_for_op_motion_bridge(self, motion_key, total_count)
    }
}
