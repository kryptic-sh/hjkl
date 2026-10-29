# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed (BREAKING)

- `PendingState` and `VimState` are `#[non_exhaustive]`: a `match` over
  `PendingState` needs a wildcard arm, and `VimState` cannot be built with a
  struct literal outside this crate. New variants and fields stop being
  breaking. `EngineCmd` stays exhaustive, since a new command needs the host to
  handle it.
- `PendingState::OpMark`, `EngineCmd::ApplyOpMark` and the required
  `VimEditorExt::apply_op_mark` are new, for an operator over a mark motion. A
  host matching `Outcome::Commit(EngineCmd::..)` exhaustively must handle
  `ApplyOpMark` by calling `apply_op_mark`, as `hjkl`'s chord routing does.

### Added

- Insert-mode auto-wrap under `'formatoptions'` `t` (vim's `internal_format`): a
  non-blank character typed where it would end past `'textwidth'` breaks the
  line at the last blank run starting at or before the margin, dropping that run
  and copying the indent under `'autoindent'`. hjkl's default `formatoptions`
  (`ro`) leaves `t` out, so nothing wraps unless it is set. `.`, a counted
  insert and `".p` retype the text through the same check, so they wrap where
  their own text reaches the margin, as vim's redo does, instead of replaying
  the first insert's line breaks.
- `apply_after_z` handles `F`: `[count]zF` adds a closed fold over `count` rows
  from the cursor (clamped to `last_content_row`) and parks on the top row's
  first non-blank, setting the curswant there, as vim's `nv_lineop` does for the
  doubled `zf`. In Visual it takes the `zf` selection path.
- `g0` / `g^` / `g$` (`Motion::ScreenLineStart` / `ScreenLineFirstNonBlank` /
  `ScreenLineEnd`) through `apply_after_g`, `op_g_motion` and the Visual `g`
  arm. The screen line is the `wrap_segments` segment holding the cursor under
  wrap, and the columns from `Viewport::top_col` that fit the text width without
  it. `g$` is inclusive (`motion_kind`); all three set curswant to the landing
  and leave closed folds alone, as measured against nvim 0.12.5.
- `hjkl_vim::charwise_visual_range`, for a host that runs the Visual operators
  itself: the range a charwise selection covers, taking the line break when it
  ends on a line's NUL. The hjkl app's Visual `d` / `y` / `c` and its Cut / Copy
  menu use it.
- `gI` inserts at column 0, before any indent; `.` and a count repeat it there.
- Operator + mark motion: `d'a` / `y'a` / `c'a` / `>'a` … run linewise to mark
  `a`'s row, ``d`a`` … charwise exclusive to its position, on both the engine
  FSM and the keymap path, and `.` repeats them. An unset mark aborts, as in
  vim.

- vim's default `'foldopen'`: after a Normal-mode motion of the `hor`, `block`,
  `percent`, `search` or `mark` class, the closed folds holding the cursor row
  open (`foldopen_cursor`). Gated as vim gates it — not in Visual, not under an
  operator, not while `@q` replays.

### Fixed

- `.` after a Visual-block change rebuilds the block with the same display width
  from the cursor's display column, as vim's `v_.` does, instead of the same
  number of characters: after `<C-v>ld` over a tab, `j.` deletes the same screen
  width on the next line.
- `buffer_selection` gives a Visual block's corners in display columns
  (`block_corners`), so the highlight matches the cells the block operators act
  on over a line with a tab; `selection_highlight` converts them to the top and
  bottom rows' characters.
- Visual-block `>` / `<` shift in display columns, as vim's `shift_block` does:
  `>` rewrites the whitespace around the block's left edge (the run before it, a
  tab the edge cuts, and the run starting there) as one stretch a shiftwidth
  wider, and `<` takes cells out of the whitespace starting at the edge. On a
  line with a tab the shift used to land at the top row's character column;
  `<C-v>j>` over `\tab` now gives `"        ab"`, as in nvim.
- `VimEditorExt::indent_block`, the app's Visual-block `>` / `<`, shifts at the
  block's left edge instead of indenting whole lines.
- Visual-block operators work in display columns, as vim's do (`block_prep`, now
  `hjkl_vim`'s `block` module): over a line with a tab or a double-width
  character, `d`, `y`, `c`, the case operators, `r`, `I`, `A` and a block paste
  take the same screen columns on every row. A tab cut by an edge keeps its
  outside cells as spaces on a delete and gives its inside cells as spaces to a
  yank, and `r` turns each cell of a tab into the replacement. Before, each row
  took the top row's character columns, so `<C-v>kd` from the end of
  `abcdefghij` below `\tab` cut `defghij` where nvim cuts `ghij`.
- Block `A` / `I` on a single row leave the cursor on the block's left edge, as
  on a taller block.
- Undo after a linewise change over more than one line (`c2j`, `2cc`, `Vjc`)
  lands on the range's second line at the column the change started at, as vim's
  does: `op_change` deletes the lines after the first before saving the first,
  so that is where its undo step holds the cursor. hjkl landed on the first
  line.
- `J` keeps the current line's marks and moves the next line's onto it the way
  vim's `do_join` does: shifted to where its text lands, less the indent the
  join strips, and onto the join point from inside that indent. Before, `lmaJ`
  lost `'a` and a mark on the joined line kept its old column.
- A macro recorded through `dispatch_input` keeps the keys typed into the search
  prompt: `qq/g<CR>q@q` searched nothing on replay, because the prompt's
  intercept returned before the recorder and `"q` held only `/`. During replay
  an `<Esc>` in the prompt runs the search instead of cancelling it, as vim's
  command line does inside a macro (`:h c_<Esc>`).
- An operator over a counted `gj` / `gk` that runs out of screen lines is
  cancelled, as in vim, with the cursor left where the motion stopped: `d9gj`
  two lines from the end deletes nothing, where it deleted to the last line.
- A count typed before a register prefix is kept, and one typed after it
  multiplies it, as in vim: `2"ap` pastes twice (the Normal-mode FSM dropped the
  count at `"`), and `2"a3p` pastes six times where the app's keymap
  concatenated the digits into 23. `CountAccumulator::begin_register_count`
  starts the multiplication; the app calls it on `"`.
- The read-only registers `".` and `"/` can be named in Normal and Visual mode.
  `".p` / `".P` type the last inserted text again, leaving the cursor on its
  last character; `"/p` pastes the last search pattern. A delete, yank or change
  naming either is refused with `E354` before it touches the text, as in vim:
  `".dd` leaves the line and every register alone, and `".cw` enters Insert
  without deleting. Before, `set_pending_register` dropped both, so `".p` pasted
  the unnamed register and `".dd` deleted.
- `.` and `".` after an insert that broke a line hold what was typed, read from
  where the insert started. The text diff split a repeated character the wrong
  way, so `iab<CR>c<Esc>j.` broke the next line after `c` instead of after `ab`.
- `.` after an insert that backspaced past its start replays the backspaces:
  `A<BS><BS>x<Esc>j.` deletes two characters on the next line before typing `x`,
  and `I<BS>` at column 0 joins the line above, as in vim. Before, `.` retyped
  only text the buffer grew by, so it did nothing after `A<BS><BS>x`. `<BS>` and
  `<C-h>` past the start also move the session's start with them, as vim moves
  `Insstart`, and a count (`2A<BS>x`) repeats the backspaces with each copy.
- `visual_text_obj_extend_counted`: a bracket object (`i(`, `a[`, `iB`, ...) on
  a charwise or block selection bigger than one cell follows vim's
  `current_block` (`text_object::bracket_visual`). It searches from the
  selection's low end, takes the next bracket forward when none encloses it,
  grows an inner object that is not bigger than the selection, and makes the
  selection charwise. From a one-cell block it selects the object charwise as
  `v` does; it was a no-op in blockwise Visual.
- `op_is_change` covers every operator vim redoes: the case operators, `=`, `gq`
  and `gw` join `d` / `c` / `>` / `<`, so `gUw`, `g?iw` and `gqj` are recorded
  for `.`. `!` and `gc` stay out, unverified.
- `apply_op_with_motion` runs the operator over an empty range when the motion
  stays put without cancelling it (`0`, `^`, `h` at column 0, ...): the case
  operators change the cursor's character on the first line and the whole line
  on a later one, and `>` / `<` / `=` / `gq` / `gw` act on the cursor's line.
- `<Home>` parses to `Motion::Home` and `MotionKind::Home` runs it, so it leaves
  a closed fold closed, as vim's `nv_home` does; it shared `0`'s `'foldopen'`
  `hor` handling.
- `.` after a counted Visual `>` / `<` (`Vj2>.`, `<C-v>j2<.`) shifts by the
  saved count, recorded in `LastChange::VisualOp::levels`; it shifted one level.
- `n` / `N` apply the last search's offset again (`/ab/e` then `n` lands on the
  next match's end), sharing the search prompt's offset handling. A bare `/<CR>`
  keeps the offset, `//<CR>` drops it, and `*` / `#` clear it. With an operator,
  an `e` offset makes `n` inclusive and a line offset linewise.
- `apply_op_search_range` (`d/pat<CR>`) runs through
  `operate_over_motion_range`, so it gets vim's exclusive-motion adjustment
  (`d/ab<CR>` to column 0 of a later line stops at the previous line's end) and
  closed-fold handling; a line offset (`d/pat/+1`) makes it linewise.
- `operate_over_motion_range`: a linewise motion whose top row is inside a
  closed fold starts at column 0 of the fold's first row, and the operator parks
  the cursor there (`zfjyj` / `zfjgUj` / `zfjzfG`), as vim's
  `do_pending_operator` does.
- `do_paste` puts the cursor on the insert point after a charwise paste whose
  text contains a line break, unless it is `gp` / `gP`.
- `operate_over_motion_range` takes the motion (`None` for a mark) instead of a
  `w` flag: `%` skips the charwise-to-linewise delete promotion and widens to a
  closed fold even when it did not move. `motion_opens_fold` opens a fold on
  every `%`, found or not.
- `open_line_below_bridge` / `open_line_above_bridge` move the cursor to the
  closed fold's last / first line before opening, as vim's `n_opencmd` does, and
  `O` puts the cursor on the new line directly instead of through a fold-aware
  `k`.
- `substitute_char_bridge` runs `cl` through `apply_op_with_motion` on a closed
  fold, and the charwise change in `run_operator_over_range` saves its undo step
  with the cursor on the range's start.
- `do_char_delete` and the `l` operator path in `apply_op_with_motion` do not
  widen to a closed fold when the cursor is on its line's last character, where
  vim's `l` cannot move.
- `run_operator_over_range` sets the sticky column to 0 after `zf` over a
  linewise range, `[count]zF` keeps the cursor where it was with a sticky column
  of 0, and `execute_motion` skips the vertical re-move after a `j` / `k` that
  did not move.
- `apply_visual_operator` widens the selection over closed folds before the
  operator runs (`include_closed_folds_in_selection`): the earlier end moves to
  its fold's first line, column 0, the later one to its fold's last line past
  the last character, as vim's `do_pending_operator` does in Visual mode.
- `step_normal`'s `r` chord records `LastChange::ReplaceChar` only when the
  count fits the line, and `visual_paste` saves its undo step with the cursor on
  the selection's start (column 0 of the top row in Visual-line, unless the
  cursor is above the anchor).
- `apply_op_text_obj_inner` records `LastChange::OpTextObj` only when
  `text_object_range` finds the object.
- `bracket_text_object` falls back to `find_next_open` when the cursor's closing
  bracket has no open partner, as it already did off a bracket.
- `visual_operator_body`'s charwise branch parks the cursor on the selection's
  start before the operator saves its undo step (except `gq` / `gw`), as the
  Visual-line branch already did, and its case and shift arms use the bounds
  computed once instead of re-deriving them from the cursor;
  `visual_replace_char` parks it the same way.
- `delete_to_eol_bridge` / `change_to_eol_bridge` take the
  `apply_op_with_motion` path for a count of one too when the cursor is on a
  closed fold, so the fold is included whole; the single-line shortcut stays for
  every other case. `operate_over_motion_range` takes an `include_folds` flag,
  false for a motion that succeeded in place (`succeeds_in_place`), whose
  one-character range nvim keeps inside a closed fold.
- `Motion::FirstNonBlankNextLine` / `FirstNonBlankPrevLine` move with the
  fold-aware `move_down` / `move_up` instead of the engine's row-only
  `move_first_non_blank_next_line` / `_prev_line`, fail when the fold provider
  has no visible row past the cursor's fold, and start `-` from the fold's first
  line, as vim's `cursor_down` / `cursor_up` do.
- `visual_text_obj_extend` runs a sentence object from a one-row Visual block
  through `sentence_visual`, anchored at `block_anchor`; an `Extend` keeps the
  block and syncs `block_vcol`, a `Select` turns it charwise as vim's
  `current_sent` does.
- `visual_text_obj_extend` moves `block_anchor` to a word object's start when
  the block is one cell (anchor on the cursor), as vim selects the object afresh
  there.
- `indent_block` skips empty rows, as its doc already said;
  `replay_block_visual_op` replays `Operator::Indent` / `Outdent`, which it
  dropped; and `apply_block_operator` puts the cursor on the block's top-left
  before the operator saves its undo step, as vim's `do_pending_operator` does.
- `update_block_vcol` syncs `block_vcol` for every motion that sets vim's
  curswant (the paragraph, sentence, section and unmatched-bracket motions,
  `Motion::FirstNonBlank*Line`, `GotoColumn`, `SearchNext`), leaving only `gj` /
  `gk` and `H` / `M` / `L` out; the `[` / `]` chords and `gg` now call it in
  Visual-block mode.
- `step_normal`'s `r` chord takes `<Enter>` as `'\n'` in Normal mode, and
  `replace_char` turns a `'\n'` into vim's `nv_replace` line break: it deletes
  the `count` characters and runs `insert_newline_bridge` (so `'autoindent'`
  applies) before the `<Esc>` step-back. `replace_char` now stores the curswant
  with `remember_cursor_column`.
- `apply_op_double` no longer records a `LastChange::LineOp` for
  `Operator::Yank`.
- `apply_op_with_text_object` ends the row-wise operators (`Indent`, `Outdent`,
  `AutoIndent`, `Reflow`, `ReflowKeepCursor`, `Fold`, `Comment`) on the
  exclusive-adjusted end's row, and `gq` there takes the same
  `step_past_adjusted_format` cursor step as the motion path.
- `apply_op_with_motion` keeps the start column for `Operator::Yank` over
  `Motion::FirstNonBlankLine`, as vim's `nv_lineop` skips `beginline` for a
  yank.
- The search prompt runs an `e` / `s` / `b` offset search through the new
  `offset_search_match` (vim's `do_search` start position and `SEARCH_END`
  test), applies the offset only when a match was found, and ends an operator's
  range one past an `e` landing so it is inclusive.
- `step_normal` handles `/` / `?` in the Visual modes with `enter_search`; they
  were reachable only from Normal mode (`handle_normal_only`).
- `replay_last_change` maps a recorded `Motion::ChangeWordEnd` back to the typed
  `w` / `W` and runs `change_word_motion` against the replay's cursor, so the
  `cw` rewrite follows where `.` starts instead of where the change did.
- `replay_last_change`'s `LastChange::InsertAt` arm and
  `replay_insert_and_finish` call `remember_cursor_column` after their `<Esc>`
  step-back, as `leave_insert_to_normal_bridge` does for a typed `<Esc>`.
- Every sticky-column write that stored the cursor's char column (`gg`, `%`, `J`
  / `gJ`, the paste paths, Visual join / paste / `g<C-a>`, Insert-mode `<Esc>`,
  mark and jumplist jumps, mouse placement, `zF`, the line operators, `gw` and
  the replace-mode `.`) now goes through the new `remember_cursor_column`, which
  stores `Editor::cursor_want`: the display column, with a Normal-mode cursor on
  a tab at its last cell.
- `execute_motion` passes `motion_opens_fold` whether the motion succeeded, not
  only whether it moved: a `t` / `T` onto an adjacent target
  (`till_succeeds_in_place`, now shared with `op_motion`'s `succeeds_in_place`)
  and an `n` / `N` / `*` / `#` that wraps back to a match at the cursor (with
  `wrapscan`) succeed in place, as in vim.
- `execute_motion` counts a `Motion::WordBack` / `BigWordBack` that started
  inside a closed fold beginning on row 0 as failed for `motion_opens_fold`, as
  vim's `bck_word` fails there after its snap to the fold's first line.
- `visual_text_obj_extend` sends Visual-line and Visual-block `ip` / `ap`
  through `paragraph_extend_landing`, vim's `current_par` extend walk, when the
  selection is taller than one line or (Visual-line) the fresh object starts on
  the cursor's row, replacing the generic one-row grow for linewise objects.
  `paragraph_extend_landing` takes a `count` and walks as far as it can before
  vim's `FAIL`, and no longer refuses a one-row selection.
- `update_block_vcol` syncs `block_vcol` to the cursor after `Motion::Up` /
  `Down`, as vim's `block_prep` reads the cursor's own column: a block taken
  onto a shorter line narrows to the cursor (the line's NUL in Visual), where it
  kept the old column. `block_to_eol` still survives `j` / `k`.
- `Motion::ScreenUp` / `ScreenDown` that stay on their row under wrap skip the
  `Move::Vertical` re-move after the motion, which put the cursor back on the
  curswant column of that row and so undid the step. `apply_op_with_motion` runs
  them charwise exclusive under wrap (`screen_wraps`) instead of linewise.
- `Motion::ScreenLineMiddle` (`gm`) counts half the text width from the screen
  line's start, the span `g0` uses, instead of from column 0.
- `step_normal` no longer reads a digit after `g` (`Pending::G` /
  `Pending::OpG`) as a count: the `g` arm stashes a count of 1, so `0` extended
  it to 10 instead of reaching `apply_after_g`.
- `Motion::SentencePrev` (`(`, and `d(` / `c(` / `y(` / `v(`) walks vim's
  `findsent` backward (the `VimBuf` port, through the new unguarded
  `walk_vim_buf`), replacing the row scan `text_object::sentence_boundary`,
  which is removed: after a blank line `(` lands on the first non-blank, and
  `2(` on a buffer of blank lines reaches the top instead of failing.
- `Motion::SentenceNext` (`)` in Normal and Visual) walks the same `findsent`
  port as an operator's `)`: a landing on the NUL past the end steps back onto
  the last character in Normal mode and stays on the NUL in Visual, as `$` does.
  `text_object::sentence_step_forward`, `SentenceStep` and the row scan behind
  them are removed.
- The `:h word` end-of-line clamp for an operator's `w` / `W` ends the range on
  the NUL of the last word's line, exclusive, as vim's `fwd_word` stops there,
  instead of on the word's last character; the blanks after it are included.
- `apply_op_with_motion` takes an operator's `)` from vim's `findsent` (the
  `VimBuf` port) and its `adjust_cursor`: a landing on the NUL past the end
  makes the range inclusive, and a `FAIL` cancels the operator.
- `apply_op_with_motion` runs `Operator::Change` over the empty range a
  non-cancelling motion leaves in place (`h`, `l`, `0`, `^`, `|`, `(`, `{`, and
  the word / `$` motions on an empty line), so Insert starts at the cursor
  instead of the operator being dropped.
- `quote_text_object` follows vim's `current_quote`: off a quote, the nearest
  quote before the cursor (else the first after it) opens and the next one
  closes, instead of pairing every quote from the line start and jumping to the
  next pair.
- `visual_join` puts the cursor on the selection's start column when no join
  happens, instead of leaving it at `(top, 0)` where it parks before joining.
- `toggle_case_at_cursor_bridge` (`~`) resets the curswant through
  `remember_operator_column`, and the `.` replay of `~` now calls that bridge
  instead of a copy of its loop that also skipped the end-of-line clamp.
- The `f` / `F` / `t` / `T` entry in `normal.rs` is no longer limited to Normal
  mode: the Visual modes take a find too, and `apply_find_char` runs it through
  `execute_motion_with_block_vcol` so a Visual-block find moves `block_vcol`.
- `succeeds_in_place` counts `Motion::MatchBracket` with no count as a
  zero-distance success, so an operator over a `%` that finds no match acts on
  the cursor's character, as measured in nvim 0.12.5 (vim's source not read).
- `visual_text_obj_extend_counted` grows a one-line Visual-line selection only
  when the line-wise object found is that same line (vim's `current_par` "same
  area" case); otherwise it selects the object afresh, so `Vip` on a paragraph's
  last line takes the whole paragraph.
- `visual_paste` in Visual-line mode lands like a linewise delete when the
  register is empty, and replaces the empty row a whole-buffer cut leaves
  instead of inserting above it (keeping whether the buffer ended in a newline).
- In the Visual modes `Motion::Right` (`l`) may step onto the line's NUL and
  `Motion::LineEnd` (`$`) lands on it, as vim's `nv_right` / `nv_dollar` do with
  the default `'selection'`; `charwise_visual_range` then takes the line break.
- `update_block_vcol` moves `block_vcol` (and clears the ragged-`$` flag) for
  `Motion::FileTop` and `FileBottom`, which reset vim's curswant.
- `quote_text_object` takes the count: with 2 or more, `i<q>` covers the quotes
  but none of `a<q>`'s white space (`:h i"`).
- `cut_vim_range` with an inclusive end on a line's NUL stops at the end of that
  line instead of taking its break, as `read_vim_range` already did; the Visual
  `d` / `c` / `p` paths go through `charwise_visual_range`, which says when a
  selection does take the break (an empty line with a real line after it).
- `word_text_object` is a port of vim's `current_word` and its word motions
  (`fwd_word`, `bck_word`, `end_word`, `bckend_word`) over `VimBuf`, replacing
  the single-line scan; Visual-mode word objects go through the new
  `word_visual`, as sentences go through `sentence_visual` (the shared result
  type is now `VisualTextObject`). `op_delete`'s linewise promotion
  (`delete_goes_linewise`) now covers text objects as well as motions, and
  `force_normal_bridge` / `exit_visual_to_normal_bridge` clamp the cursor onto
  the last character.
- Insert-mode `<Up>` / `<Down>` (`insert_vertical`) take the want from the
  cursor unless the previous Insert-mode keys were vertical — tracked by the new
  `VimState::insert_want_current`, which `step_insert` clears for every other
  key and `begin_insert_noundo` on entry — and land up to the line's length, not
  its last character. `<End>` stores the MAXCOL want.
- `remember_operator_column` takes the curswant from `Editor::cursor_want`, so
  an operator that leaves the cursor on a tab stores the tab's last cell.
- `execute_motion` stores `usize::MAX` as the sticky column after
  `Motion::LineEnd` (vim's MAXCOL curswant), and leaves the sticky column alone
  when `h` / `l` / `<Space>` / `<BS>` fail to move, as vim's failed commands do.
- `do_char_delete` (`x` / `X`) resets the curswant through
  `remember_operator_column`, so the `.` replay, which calls it directly, does
  too.
- Operator-pending `g` maps `_`, `M` and `m` to `Motion::LastNonBlank`,
  `LineMiddle` and `ScreenLineMiddle` (`op_g_motion`, now the one table that
  `apply_op_g_inner` and `range_for_op_g_bridge` share), and `g_` is an
  inclusive motion. `Motion::LastNonBlank` takes its count as `$` does: down
  `count - 1` lines first, failing on the last line (`count_lines_down`).
- Operators park the cursor at vim's operator start before they run — the cursor
  the undo step saves, so `u` returns there: the earlier of the cursor and the
  motion's end (`operate_over_motion_range`), the earlier of the cursor and the
  line's first non-blank for a doubled operator with no count
  (`execute_line_op`, after `nv_lineop`), and the earlier of the cursor and
  `(anchor, 0)` in Visual-line mode, which is also where `Vky` leaves the
  cursor. `gq` / `gw` keep the cursor, as in vim. The Visual-line `.` repeat
  goes through the new `execute_line_op_from_cursor`, which has no `nv_lineop`
  step.
- An inclusive motion that succeeds without moving operates on the cursor's
  character, as in vim: `d$` / `y$` / `c$` / `gU$` on a line's last character,
  `de` / `dE` / `d2e` at the end of the buffer, and `dtx` with `x` adjacent all
  did nothing, because `apply_op_with_motion` read every zero-distance range as
  a failed motion. `succeeds_in_place` tells the two apart per motion, so `2d$`
  on the last line, `d2tx` and `dge` at the buffer start stay no-ops. Under an
  operator, `;` / `,` after a `t` / `T` now set `find_repeat_skip` as the cursor
  path does, so `tbd;` skips the adjacent match.
- After an operator, `j` / `k` aim for the column the operator left the cursor
  in, as vim does (`'curswant'` is reset): `Vjy`, `Vj>`, `>j`, `gUU`, `yy`, `=`
  and the rest went back to the column from before. One rule in
  `remember_operator_column`, applied where the Visual, operator-motion and line
  operators finish; it stores a display column, so tabs before the cursor count
  at their width.
- `.` after a Visual-line operator applies to the lines left when fewer remain
  than it covered (`Vjd` then `.` on the last line deletes it): vim's Visual
  redo clamps the saved line count to the buffer, where the replay had taken the
  `[count]dd` path, which aborts.
- Visual `X` / `D` (delete), `C` / `S` / `R` (change) and `Y` (yank) in charwise
  and linewise Visual act on the selected lines whole, as in vim (`:h v_X`). `X`
  / `D` / `C` / `Y` did nothing and left Visual mode on; `S` / `R` changed only
  the selected characters.
- A charwise Visual delete (`d`, `x`) or put (`p` with an empty register) that
  reaches the end of the line leaves the cursor on the new last character, not
  one past it.
- **`is` / `as` stop at an empty line.** The sentence text object ran on through
  a paragraph break into the next paragraph — `das` in a paragraph whose last
  sentence has no terminator deleted every paragraph after it. It is now a port
  of vim's `current_sent` / `findsent`, which also gives `as` the white space
  BEFORE the sentence when there is none after it (`das` on the last sentence of
  a line or paragraph). Lines are read as the walk reaches them instead of
  flattening a window of the buffer.
- Visual `is` / `as` follow vim's `current_sent` with Visual active (new
  `sentence_visual` / `VisualSentence`): a selection already bigger than one
  character extends by sentences on the cursor's side, keeping the anchor, and
  so does one whose object comes out a single cell (`vas` on an empty line
  selects through the next sentence; `Vis` there stays Visual-line). `vasas` had
  relied on the generic text-object grow, which took the wrong unit from the
  white space between sentences.
- A Visual text object that is one character on a fresh selection (`viw` on a
  one-letter word, `vis` on an empty line) is selected, not grown into the next
  unit: vim grows a charwise selection only once it is bigger than one
  character.
- A charwise Visual selection ending on an empty line takes that line's break:
  `vy` there yanks `"\n"` (it yanked nothing), and `vu` / `vU` / `v~` (and their
  `.` repeat) no longer delete the line. The inclusive read left out the break
  the cut removed, so the case change put one character fewer back; the Visual
  yank, case ops and their replay now cover the selection as an exclusive range
  to the next line's start (`charwise_visual_range`).
- `[count]J` / `[count]gJ` on the last line with a count of 3 or more move the
  cursor to column 0, as vim does (a bare or `2` count still keeps the column);
  the cursor stayed where it was.
- `J` / `gJ` with an empty (or, for `J`, blank) line below leave the cursor on
  the line's last character. Nothing is appended, so the join point is past the
  end; the cursor stayed there, and a following `x` deleted nothing.
- A linewise delete through an operator motion, a text object or Visual-line
  (`dj`, `dap`, `Vd`) leaves the cursor on the first non-blank of the line it
  lands on, as `dd` already did; it landed on column 0. The three copies of that
  landing rule are one `land_after_linewise_delete` now.
- Linewise pastes (`p`, `P`, `Vp`, and a linewise register over a charwise or
  blockwise selection) keep a register's blank first and last lines. Each
  trimmed every leading and trailing `\n` off the register; now only the last
  line terminator goes (`linewise_payload`). A linewise delete of a whole buffer
  whose first line is blank no longer drops that line from the register either.
- A linewise register pasted over a charwise selection (`vp`) splits the line
  there and puts its lines between the two halves, as vim does; it was put below
  the line.
- `dw` / `yw` on a line's last character (or on trailing whitespace) act on it.
  The `:h word` clamp makes that a one-character INCLUSIVE range, and
  `run_operator_over_range` returned early for every charwise range with
  `top == bot`; only an exclusive one is empty.
- `.` of a Replace-mode change that contained `<CR>` splits the line there, as
  the typed `<CR>` did, instead of overtyping a character with the break.
- Insert-mode `<BS>` under `softtabstop` deletes back to the previous stop, as
  vim's `ins_bs` does: from column 2 of `"  q"` both spaces go, and a tab counts
  its display width (a tab that overshoots the stop is made up with spaces). It
  only fired when the cursor sat exactly on a stop after a full run of spaces,
  so it deleted one space at a time otherwise.
- `[count]o` / `[count]O` repeat the whole typed text on every line: `2oab<CR>c`
  gave `ab`, `c`, `c` instead of `ab`, `c`, `ab`, `c`. And `.` keeps the count
  (`2oab<Esc>` then `.` adds two lines) unless given a new one.
- `gq` / `gw` rebuild the indent they format with, as vim's `set_indent` does:
  the same width, as spaces under `expandtab` and as tabs + spaces otherwise. A
  tab indent stayed a tab under `expandtab`, and a space indent stayed spaces
  under `noexpandtab`.
- `.` of `o` / `O` retypes what was typed into the opened line; it opened an
  empty line and dropped the text. The open bridges restart the insert session's
  snapshot after opening, so the recorded text is just what was typed, and the
  replay opens through the same bridge, so autoindent and the comment leader
  come from the line it opens at.
- `.` of an insert that broke a line (`ia<CR>b<Esc>`, `A<CR>…`, `cw…<CR>…`)
  repeats it. `finish_insert_session` compared the same row range of the buffer
  before and after, so a session that added rows saw no growth and recorded
  nothing to replay.
- `.` of an insert (`i`, `a`, `I`, `A`, counted or not) leaves the cursor on the
  last inserted character, as the typed insert's `<Esc>` did; it stayed one
  column right.
- `u` after an operator delete leaves the cursor at the start of the deleted
  range, as vim does: an upward `d2k` / `dgg` / `d{` / `d'a` undoes onto its top
  row, where hjkl returned to the row the delete was typed on.
- A same-buffer uppercase mark jump (`'A`) opens a closed fold it lands in, like
  the lowercase ones; it had its own copy of the jump that missed `'foldopen'`.
- On a line that is all whitespace, `^`, `_`, `+`, `-`, `<CR>`, `G`, `dd` and
  `>>` land on its last char, as in vim, and `I` inserts after the whitespace
  rather than before it (`move_to_insert_start`).
- `gq` keeps a whitespace-only line as it is instead of emptying it, and leaves
  the cursor on the last line of the range, a trailing blank line included — or,
  for `gq}` / `gq{`, whose range the exclusive-motion rule shrank, on the line
  after it. `gw` with the cursor on a whitespace-only line keeps its column.
- `gq` / `gw` under `noautoindent` start the lines they open at column 0, as vim
  does; they copied the paragraph's indent onto every line regardless.
  `'autoindent'` (hjkl's default) keeps the indent. The wrap now takes a
  `WrapOpts` (`textwidth`, `tabstop`, `autoindent`) instead of loose arguments.
- A forward charwise operator started inside a closed fold takes the fold whole
  from any column, and a backward charwise one (`db`, `d0`) is left as the
  motion made it. The column-0 proxy for "the fold is still closed"
  (`cursor_left_fold_open`) is gone, along with the fold-blind word-motion
  provider it selected.

- `cw` / `cW` on the last character of a word changes only that character, as
  vim's `end_word(stop)` does; it previously ran on to the next word's end,
  because `cw` was mapped to the same motion as `ce`.
- Word motions and operators treat a closed fold the way vim does: the fold is
  jumped at each count iteration, so `2dw` walks past it, `de` ends at the next
  word end, and `dj` / `d3w` landing inside a later fold take that fold whole.
- Counted reverse blockwise sentence (`<C-v>k2is`) offers only the blank run the
  cursor stands in or the one directly above its sentence, matching nvim, rather
  than treating every blank run as a step.

## [0.41.0] - 2026-08-04

### Fixed

- `insert_newline_bridge` applies `autoindent` the way vim does: the computed
  indent REPLACES the leading whitespace of the text that moves down, and it is
  computed from the part of the line left of the cursor rather than the whole
  line. Previously it inserted in front of that whitespace, so an Enter at
  column 0 of an indented line produced a row carrying both the old indent and a
  fresh copy — geometric growth under a held key. Only the plain autoindent path
  changed; the autopair open-pair, code-fence and comment-continuation branches
  are untouched, and `noautoindent` still moves the text down verbatim.

- `.` reuses the register the repeated change named (`:h redo-register`).
  `LastChange::OpMotion`, `OpTextObj`, `DeleteToEol` and `Paste` did not carry
  the register, so `"adw` then `.` deleted into the unnamed register and `"ap`
  then `.` pasted from it. They now record it the way `LineOp` already did, and
  `dot_repeat` restores it before replaying. Four cases in
  `corpus/tier2_registers.toml` pin this against nvim.
- An over-budget paste reports `E342: Out of memory!  (allocating N bytes)`
  instead of failing silently. `do_paste` returned `false` and nothing
  downstream could tell that apart from an empty register; it queues the message
  on the editor now (`Editor::push_error`), which the host drains. Both the
  byte-budget checks and the arithmetic-overflow guards report it — the user's
  ask is the same size either way. An empty register is still silent, as in vim.
- **Blockwise `>` / `<` shift at the block's left column.** They fell through to
  the linewise `indent_rows` / `outdent_rows` under a comment claiming vim did
  the same — it does not: `<C-v>jl>` with the block starting at column 2 turns
  `"abcdef"` into `"ab    cdef"`, where the linewise fallback produced
  `"    abcdef"`. New `indent_block` / `outdent_block` do the real thing: the
  fill is built from the block's DISPLAY column so `noexpandtab` emits a tab
  only where one actually reaches the next tab stop, a row too short to reach
  the block column (or an empty one) is skipped, and an outdent splits a tab
  that straddles the boundary — `"ab\tcd"` outdented from column 2 at
  `tabstop=8 shiftwidth=4` becomes `"ab  cd"`. A row with no whitespace at the
  block column is left alone. Six oracle cases; the differential fuzzer at seed
  777 drops from 78 to 77.

  Not fixed, and tracked in `docs/backlog.md`: `<C-v>iw<` still outdents the
  whole line, because a text object collapses hjkl out of blockwise visual
  before the operator runs. That is a separate defect from the shift geometry.

- **Visual mode takes a `"reg` selector.** `"` opened the register chord only in
  Normal mode, so in a selection the `"` fell through unconsumed, the register
  letter armed the around-text-object chord instead, and the operator key was
  swallowed as that chord's target — `vll"ad` left the buffer untouched and the
  selection still up. `"` now arms `Pending::SelectRegister` in charwise,
  linewise and blockwise visual as well, so `"ad`, `"ay`, `"ac` and `"ap` all
  reach the register they name.
- `.` after `"ax` / `"aX` deletes into `"a` too. `LastChange::CharDel` was the
  last variant with no `register` field, so the repeat fell back to the unnamed
  register and left `"a` holding the FIRST deletion (`"axl.` on `"abcdef"` left
  `"a` as `a` where nvim leaves `c`). The live `x` / `X` path already honoured
  an explicit register; only the repeat did not. **`LastChange::CharDel` gained
  a `register` field** — a breaking change for anyone matching on it.
  `dot_repeat_char_delete_reuses_the_explicit_register` covers `x`, `X` and a
  counted `x`, and five more cases in `corpus/tier2_registers.toml` pin the
  whole family against nvim.
- `.` after a visual operator or `"acgn` reuses the register too, closing out
  the family. `LastChange::GnOp` and `VisualOp` were the two remaining variants
  with no `register` field — both **gained one**, a breaking change for anyone
  matching on them. The blockwise replay had a second defect behind that one:
  `replay_block_visual_op` passed a hardcoded `None` target to
  `record_delete_block`, so even with the register restored the repeat could not
  have written it.
- `d}` with the cursor already on the last character of the last line deletes
  that character. vim's `}` sets `*pincl` even when it does not move, so the
  zero-distance form is a successful INCLUSIVE motion; hjkl treated every
  zero-width charwise range as a failed motion and did nothing. The landing is
  now emitted as the equivalent one-char exclusive range — the shape `dl` on a
  line's final char already produces — rather than by loosening the
  `start == end` guards, which still have to reject `d$` on an empty line.
- A counted `$` fails on the last line instead of clamping to it. vim's
  `nv_dollar` runs `cursor_down(count - 1)` before moving to the line end and
  aborts the whole command when that fails — which happens only when the cursor
  is already on the last line; an overshooting count still succeeds, clamped.
  hjkl always clamped, so `2$` on a one-line buffer moved to the line end and
  `2C` / `2D` emptied the line where vim does nothing at all. `5$` on three rows
  still lands on row 2 and `5C` still collapses them. Found by the differential
  fuzzer (seed 777, case 115); its divergence count drops from 84 to 83.
- `"aD` and `"aC` write the named register. `command::delete_to_eol` wrote the
  deleted text with `set_yank`, which only ever reaches the unnamed register, so
  an explicit `"reg` was silently dropped. It now routes through
  `record_delete`, which also puts an unnamed `D` in the small-delete register
  `"-` as vim does.

### Added

- `MAX_PASTE_BYTES` is public, so a host can report the paste budget it hit
  rather than restating the number. `vim::command` is public to carry it.

### Changed

- The `p` / `P` byte budget is raised from 1 MiB to 64 MiB. The old value was
  derived from a per-iteration paste implementation that no longer exists;
  measured on the current batched path, peak RSS is linear in payload (~3.1x
  charwise, ~4.1x linewise, ~2.9x blockwise) and independent of document size,
  so 64 MiB costs ~208 MiB peak and ~73 ms. Pasting is no longer stricter than
  opening a file of the same size. Over-budget pastes are still rejected — but
  no longer silently; see the `E342` entry above.
- Blockwise `p` / `P` no longer rebuilds the whole document. It applied its edit
  by materializing every line into a `Vec<String>`, joining it back and
  replacing the entire buffer, so cost scaled with the DOCUMENT rather than the
  pasted block and every block paste reached the undo stack, the change log and
  any LSP as a whole-document replacement. It now issues one `Edit::InsertBlock`
  (plus one `Edit::InsertStr` when the block extends past the last row). A 1 MiB
  block paste into a 1 000 000-line buffer drops from +128.5 MiB and 351 ms to
  +0.2 MiB and 1 ms; it remains a single undo step.

### Fixed

- A blockwise paste extending past the last row no longer strips the buffer's
  trailing newline. Rows opened past EOF were spliced into ropey's phantom
  trailing line — the newline terminator — instead of past it.
- Operator ranges over `}` follow vim's three `findpar` / `:h exclusive` rules.
  A `}` that lands on the last character of the last line makes the motion
  inclusive, so `d}` in the final paragraph reaches end-of-buffer instead of
  stopping one character short. An exclusive charwise range that ends in column
  0 of a later row pulls its end back to the end of the previous line and turns
  inclusive, and promotes to linewise when the start is at or before its line's
  first non-blank — so `y}P` from column 0 pastes the paragraph back as whole
  lines like nvim, and `d}` from mid-line no longer eats the line break. A
  charwise `d` spanning more than one row whose range ends with only whitespace
  left on the last row, started from within the indent, promotes to linewise
  (vim's "strange Vi behaviour" in `op_delete`), which is what gives `d}` the
  linewise register content nvim produces.
- `(` and `)` are all-or-nothing under a count. `findsent` fails as soon as one
  repetition has nowhere left to go and vim leaves the cursor untouched; the
  motion previously moved as far as it could, so `2)` on a line with no sentence
  terminator landed on the last character instead of staying put (the cause of
  the `V}u2)1gUiW` composite divergence) and `3(` past the first sentence
  stopped at `(0, 0)` instead of failing. A terminator that closes the buffer
  still counts as a real boundary, so `3)` on `"One. Two."` succeeds while `3)`
  on `"One. Two"` fails — the new `SentenceStep` classification in
  `vim::text_object` is what separates the two.

## [0.40.0] - 2026-08-01

### Fixed

- Linewise operator motions at a buffer edge no longer delete the current line;
  rejected oversized pastes preserve the prior dot-repeat change; and empty
  Replace-mode dot replay restores the cursor's sticky column without an undo
  entry.
- Counted linewise paste now uses one edit and rejects oversized charwise or
  blockwise payloads before allocating or mutating the buffer.

### Removed

- Dropped the `crossterm` feature and `handle_key`. The crossterm-driven FSM
  wrapper lives in the new `hjkl-vim-tui` crate. Three test files
  (`editor_fsm.rs`, `vim_fsm.rs`, `proptest_fsm.rs`) relocated to
  `hjkl-vim-tui/tests/`. Phase 3 of #162.

## [0.23.2] - 2026-05-18

### Fixed

- Test helpers in `vim_fsm.rs` and `editor_fsm.rs` updated to use
  `unwrap_or_default()` after `Buffer::line` changed to return `Option<String>`
  in hjkl-buffer 0.8.1.

## [0.23.1] - 2026-05-18

## [0.23.0] - 2026-05-17

### Added

- `descriptors` module: `VimDescriptor` struct and `children_for(mode, prefix)`
  function that return the direct children of a vim FSM prefix for which-key
  popup integration (#64). Covers Normal root (83 bindings), g-prefix (19),
  z-prefix (11), operator-pending motions (24), and Visual root. `COUNT_*`
  constants assert exact counts to catch table drift.
- Added `hjkl-keymap = "0.3"` dependency (used by
  `VimDescriptor::key: KeyEvent`).

## [0.22.0] - 2026-05-17

### Changed

- `default` features now `[]` — `crossterm` dropped from defaults (#99).
  Consumers that relied on the default must add `features = ["crossterm"]`
  explicitly.
- Bumped pinned `hjkl-engine` `0.10` → `0.11` (#99 cascade).

## [0.21.0] - 2026-05-17

### Changed

- Bumped pinned `hjkl-engine` `0.9` → `0.10` (#96 cascade).

## [0.20.0] - 2026-05-16

### Added

- `OperatorKind::AutoIndent` — new public variant for the `=` operator. The FSM
  grammar now routes `=` in Normal mode into the `AfterOp` reducer with
  `op = OperatorKind::AutoIndent`, enabling `=<motion>` and `==` (double) forms.
  Users mapping operator-based keybinds gain auto-indent without extra host
  code.

### Changed

- `hjkl-engine` dependency bumped `0.8` → `0.9` (tracks hjkl-engine 0.9.1 which
  fixes the buffer pin and resolves type collisions with hjkl-buffer 0.7).
- `hjkl-buffer` dependency bumped `0.6` → `0.7`.

### Fixed

- FSM test suite updated to use `Editor::mouse_click_doc` after the upstream
  rename removed `mouse_click_in_rect`.

## [0.19.0] - 2026-05-15

### Added

Phase 6.6 of kryptic-sh/hjkl#72 — the vim FSM physically lives in `hjkl-vim` now
(previously inline in `hjkl-engine::vim`). hjkl-vim is the canonical external
entry point for driving the vim grammar.

- `hjkl_vim::dispatch_input(editor, input) -> bool` — canonical FSM entry. Wraps
  `Editor::begin_step` / per-mode dispatch / `Editor::end_step`.
- `hjkl_vim::handle_key(editor, key_event) -> bool` (under
  `#[cfg(feature = "crossterm")]`) — convenience wrapper that decodes a
  crossterm `KeyEvent` via `hjkl_engine::crossterm_to_input` and routes through
  `dispatch_input`. Emits cursor-shape change after dispatch.
- `hjkl_vim::feed_input(editor, planned) -> bool` — convenience wrapper that
  decodes a `hjkl_engine::PlannedInput` via `hjkl_engine::decode_planned_input`
  and routes through `dispatch_input`. Emits cursor-shape change after dispatch.
- `hjkl_vim::search_prompt::step_search_prompt` — search-prompt FSM body.
  Dispatched by `dispatch_input` before the general per-mode dispatch.
- `hjkl_vim::insert::step_insert` (+ `handle_insert_key`) — insert-mode FSM
  body.
- `hjkl_vim::normal::step_normal` (+ `handle_normal_only` + 17 dispatch helpers)
  — normal + visual mode FSM body. Drives all keys for non-insert
  non-search-prompt modes.

### Changed

- Depends on `hjkl-engine` `>=0.7`. hjkl-engine 0.7.0 ships the breaking FSM
  removal that this crate's new entry points replace.
- Test suite expanded with ~200 FSM-driving tests relocated from hjkl-engine's
  internal test mods. New `tests/vim_fsm.rs`, `tests/editor_fsm.rs`,
  `tests/proptest_fsm.rs`, `tests/dispatch_input.rs`.

### Migration

If you previously drove the FSM through `hjkl-engine`:

```rust
// Before:
hjkl_engine::step(&mut editor, input);
editor.handle_key(crossterm_key);
editor.step_input(input);
editor.feed_input(planned);

// After:
hjkl_vim::dispatch_input(&mut editor, input);
hjkl_vim::handle_key(&mut editor, crossterm_key);
hjkl_vim::dispatch_input(&mut editor, input);
hjkl_vim::feed_input(&mut editor, planned);
```

## [0.18.1] - 2026-05-14

### Added

- `PendingState::RecordMacroTarget` — second-key chord state for `q<x>`. On the
  next `Key::Char(ch)` matching `[a-zA-Z0-9]` emits
  `EngineCmd::StartMacroRecord { register: ch }`; Esc or any other key cancels.
  Phase 5b of kryptic-sh/hjkl#71.
- `PendingState::PlayMacroTarget { count }` — second-key chord state for `@<x>`.
  Accepts `[a-zA-Z0-9]`, `'@'` (replay last played), and `':'` (replay last `:`
  ex command, vim's `@:`); emits `EngineCmd::PlayMacro { register, count }`. Esc
  or any other key cancels. Phase 5b/5d of kryptic-sh/hjkl#71.
- `EngineCmd::StartMacroRecord { register }` — chord completion for `q<ch>`.
  Host calls `Editor::start_macro_record(register)`. Phase 5b.
- `EngineCmd::PlayMacro { register, count }` — chord completion for `@<ch>`.
  Host calls `Editor::play_macro(register, count)` (or, for `':'`, replays the
  last ex command via the host). Phase 5b/5d.

## [0.18.0] - 2026-05-13

### Added

- `PendingState::SetMark` — second-key chord state for `m<x>`. On the next
  `Key::Char(ch)` emits `EngineCmd::SetMark { ch }`; Esc or any non-char key
  cancels. Phase 5a of kryptic-sh/hjkl#71.
- `PendingState::GotoMarkLine` — second-key chord state for `'<x>`. On the next
  `Key::Char(ch)` emits `EngineCmd::GotoMarkLine { ch }`; Esc or any non-char
  key cancels. Phase 5a of kryptic-sh/hjkl#71.
- `PendingState::GotoMarkChar` — second-key chord state for `` `<x> ``. On the
  next `Key::Char(ch)` emits `EngineCmd::GotoMarkChar { ch }`; Esc or any
  non-char key cancels. Fires in both Normal and Visual modes. Phase 5a of
  kryptic-sh/hjkl#71.
- `EngineCmd::SetMark { ch }` — chord completion for `m<ch>`. Host calls
  `Editor::set_mark_at_cursor(ch)`. Phase 5a of kryptic-sh/hjkl#71.
- `EngineCmd::GotoMarkLine { ch }` — chord completion for `'<ch>`. Host calls
  `Editor::goto_mark_line(ch)`. Phase 5a of kryptic-sh/hjkl#71.
- `EngineCmd::GotoMarkChar { ch }` — chord completion for `` `<ch> ``. Host
  calls `Editor::goto_mark_char(ch)`. Phase 5a of kryptic-sh/hjkl#71.

## [0.17.0] - 2026-05-13

### Added

- `CountAccumulator` — digit-prefix count buffer for the vim grammar. Owns vim's
  count semantics including the digit-0-vs-LineStart quirk and overflow
  saturation. Migrated from `apps/hjkl`'s `pending_count: String` field.
- `MotionKind::BracketMatch` (`%`) — jump to the matching bracket (`()`, `[]`,
  `{}`). Count is passed through to the engine; the engine currently implements
  the matching-bracket semantic only (vim's `N%` percentage-of-file form is not
  yet wired). No-op when the cursor is not on a bracket character. Phase 3f of
  kryptic-sh/hjkl#69. Enum remains `#[non_exhaustive]`; consumers on hjkl-vim
  0.16.x must bump to 0.17 to handle this new arm.
- `MotionKind::ViewportTop` (`H`) — cursor to top of visible viewport; count
  offsets `count - 1` rows down from top (matching vim's `H` count semantics).
  Lands on first non-blank. Phase 3g of kryptic-sh/hjkl#69.
- `MotionKind::ViewportMiddle` (`M`) — cursor to middle row of visible viewport;
  count ignored (vim's `M` is a plain motion). Lands on first non-blank. Phase
  3g of kryptic-sh/hjkl#69.
- `MotionKind::ViewportBottom` (`L`) — cursor to bottom of visible viewport;
  count offsets `count - 1` rows up from bottom (matching vim's `L` count
  semantics). Lands on first non-blank. Phase 3g of kryptic-sh/hjkl#69.
- `MotionKind::HalfPageDown` (`<C-d>`) — move cursor half a page down; count
  multiplies the half-page distance. Lands on first non-blank. Phase 3g of
  kryptic-sh/hjkl#69.
- `MotionKind::HalfPageUp` (`<C-u>`) — move cursor half a page up; count
  multiplies. Lands on first non-blank. Phase 3g of kryptic-sh/hjkl#69.
- `MotionKind::FullPageDown` (`<C-f>`) — move cursor a full page down (2-line
  overlap); count multiplies. Lands on first non-blank. Phase 3g of
  kryptic-sh/hjkl#69.
- `MotionKind::FullPageUp` (`<C-b>`) — move cursor a full page up (2-line
  overlap); count multiplies. Lands on first non-blank. Phase 3g of
  kryptic-sh/hjkl#69.

## [0.16.0] - 2026-05-13

### Added

- `MotionKind::FindRepeat` (`;`) — repeat last `f`/`F`/`t`/`T` in the same
  direction. No-op if no prior find exists. Phase 3e of kryptic-sh/hjkl#69.
- `MotionKind::FindRepeatReverse` (`,`) — repeat last `f`/`F`/`t`/`T` in the
  reverse direction. No-op if no prior find exists. Phase 3e of
  kryptic-sh/hjkl#69. Enum remains `#[non_exhaustive]`; consumers on hjkl-vim
  0.15.x must bump to 0.16 to handle these new arms.

## [0.15.0] - 2026-05-13

### Added

- `MotionKind::GotoLine` (`G`) — Phase 3d of kryptic-sh/hjkl#69. Count
  semantics: count 0 or 1 (bare `G`) → last line of buffer; count > 1 → jump to
  that 1-based line number. `gg` (first line) continues to route through the
  G-chord path (`Editor::after_g`) and is unaffected. Enum remains
  `#[non_exhaustive]`; consumers on hjkl-vim 0.14.x must bump to 0.15 to handle
  this new arm.

## [0.14.0] - 2026-05-13

### Added

- `MotionKind::LineStart` (`0` / `<Home>`), `MotionKind::FirstNonBlank` (`^`),
  `MotionKind::LineEnd` (`$` / `<End>`) — the 3 Phase 3c line-anchored motion
  variants added to `crates/hjkl-vim/src/motion.rs`. Enum remains
  `#[non_exhaustive]`; consumers on hjkl-vim 0.13.x must bump to 0.14 to handle
  these new arms.

## [0.13.0] - 2026-05-13

### Added

- `MotionKind::WordForward` (`w`), `MotionKind::BigWordForward` (`W`),
  `MotionKind::WordBackward` (`b`), `MotionKind::BigWordBackward` (`B`),
  `MotionKind::WordEnd` (`e`), `MotionKind::BigWordEnd` (`E`) — the 6 Phase 3b
  word-motion variants added to `crates/hjkl-vim/src/motion.rs`. Enum remains
  `#[non_exhaustive]`; consumers on hjkl-vim 0.12.x must bump to 0.13 to handle
  these new arms.

## [0.12.0] - 2026-05-13

### Added

- `MotionKind` enum (`crates/hjkl-vim/src/motion.rs`, re-exported from
  `lib.rs`): names the 6 Phase 3a cursor motions so the host keymap path can
  dispatch them without depending on engine internals. Marked
  `#[non_exhaustive]` so later phases add variants without a major bump on the
  `hjkl-vim` side. Initial variants: `CharLeft` (`h` / `<BS>`), `CharRight` (`l`
  / `<Space>`), `LineDown` (`j`), `LineUp` (`k`), `FirstNonBlankDown` (`+`),
  `FirstNonBlankUp` (`-`).

## [0.11.0] - 2026-05-13

### Added

- `PendingState::SelectRegister` — reducer sub-state for the `"<reg>` chord in
  Normal mode. Hosts set this variant after intercepting `"`; `step` routes the
  next `Key::Char(ch)` to `EngineCmd::SetPendingRegister { reg: ch }` or cancels
  on `Key::Esc` / any non-char key (mirrors the `AfterG` arm). The char is
  passed through unvalidated — engine validates against `[a-zA-Z0-9"+*_]`.
- `EngineCmd::SetPendingRegister { reg: char }` — emitted by the
  `SelectRegister` reducer arm; host calls `Editor::set_pending_register(reg)`
  on receipt. Engine validates `reg` and sets `vim.pending_register` if valid;
  invalid chars are silently ignored (no-op, matching the engine FSM behaviour).

## [0.10.0] - 2026-05-13

### Added

- `OperatorKind::Uppercase`, `OperatorKind::Lowercase`,
  `OperatorKind::ToggleCase`, `OperatorKind::Reflow` variants — chord-initiated
  case/reflow operators bridged through the reducer in Phase 2c-v. `Uppercase`
  maps to `gU`, `Lowercase` to `gu`, `ToggleCase` to `g~`, `Reflow` to `gq`.
- `OperatorKind::double_char` updated to cover all nine variants: the four new
  operators map to `'U'`, `'u'`, `'~'`, `'q'` respectively, so the `AfterOp`
  reducer's doubled-letter detection (`gUU`, `guu`, `g~~`, `gqq`) works
  automatically via the existing `ch == op.double_char()` check.

## [0.9.0] - 2026-05-13

### Added

- `PendingState::OpG { op, total_count }` — reducer sub-state reached from
  `AfterOp` when the operator key is followed by `g`. `total_count` is
  `count1.max(1) * inner_count.max(1)` folded at transition time. The next char
  is the g-chord second key (`g` for `gg` = file-top, `e` for `ge` =
  word-end-back, `E`, `j`, `k`, or the case-op doubled form `U`/`u`/`~`);
  `Key::Esc` or any non-char key cancels.
- `EngineCmd::ApplyOpG { op, ch, total_count }` — emitted by the `OpG` reducer
  arm when the second char arrives. Host calls the new `Editor::apply_op_g`
  method (hjkl-engine 0.5.16+). Unknown chars are passed through unvalidated;
  the engine treats them as a no-op.

### Removed

- `EngineCmd::EnterOpG { op, count1 }` — **breaking**. The `AfterOp` arm no
  longer emits this variant; it transitions to `PendingState::OpG` instead,
  keeping the g-chord second char in the reducer rather than handing control
  back to the engine FSM. Hosts must replace any `EnterOpG` match arm with
  `ApplyOpG`.

## [0.8.0] - 2026-05-13

### Added

- `PendingState::OpTextObj { op, total_count, inner }` — reducer sub-state
  reached from `AfterOp` when the operator key is followed by `i` or `a`.
  `total_count` is `count1.max(1) * inner_count.max(1)` folded at transition
  time. The next char is the text-object kind; `Key::Esc` or any non-char key
  cancels.
- `EngineCmd::ApplyOpTextObj { op, ch, inner, total_count }` — emitted by the
  `OpTextObj` reducer arm when the text-object char arrives. Host calls the new
  `Editor::apply_op_text_obj` method (hjkl-engine 0.5.15+).

### Removed

- `EngineCmd::EnterOpTextObj { op, count1, inner }` — **breaking**. The
  `AfterOp` arm no longer emits this variant; it transitions to
  `PendingState::OpTextObj` instead, keeping the text-object char in the reducer
  rather than handing control back to the engine FSM. Hosts must replace any
  `EnterOpTextObj` match arm with `ApplyOpTextObj`.

## [0.7.0] - 2026-05-13

### Added

- `PendingState::OpFind { op, total_count, forward, till }` — reducer sub-state
  reached from `AfterOp` when the operator key (`d`/`y`/`c`/`>`/`<`) is followed
  by `f`/`F`/`t`/`T`. `total_count` is `count1.max(1) * inner_count.max(1)`
  folded at transition time. The next char is the find target; `Key::Esc` or any
  non-char key cancels (vim's `f<Esc>` semantics).
- `EngineCmd::ApplyOpFind { op, ch, forward, till, total_count }` — emitted by
  the `OpFind` reducer arm when the find-target char arrives. Host calls the new
  `Editor::apply_op_find` method (hjkl-engine 0.5.14+). Engine builds
  `Motion::Find { ch, forward, till }` and applies the operator.

### Removed

- `EngineCmd::EnterOpFind { op, count1, forward, till }` — **breaking**. The
  `AfterOp` arm no longer emits this variant; it transitions to
  `PendingState::OpFind` instead, keeping the find-target char in the reducer
  rather than handing control back to the engine FSM. Hosts must replace any
  `EnterOpFind` match arm with `ApplyOpFind`.

## [0.6.0] - 2026-05-13

### Added

- `OperatorKind` enum — carries operator identity in the reducer without
  depending on `hjkl-engine`. Variants: `Delete`, `Yank`, `Change`, `Indent`,
  `Outdent`. Exported from crate root.
- `PendingState::AfterOp { op, count1, inner_count }` — pending variant for bare
  op-pending entered from Normal mode after `d` / `y` / `c` / `>` / `<`. The
  reducer owns both `count1` (prefix) and `inner_count` (post-operator digit
  accumulation); `total = count1.max(1) * inner_count.max(1)` is passed to the
  engine on completion. Vim quirk: bare `0` when `inner_count == 0` is the
  `LineStart` motion, not a digit.
- `EngineCmd::ApplyOpMotion { op, motion_key, total_count }` — emitted when the
  next char is any single-key motion; host calls `Editor::apply_op_motion`.
- `EngineCmd::ApplyOpDouble { op, total_count }` — emitted on doubled-letter
  line op (`dd` / `yy` / `cc` / `>>` / `<<`); host calls
  `Editor::apply_op_double`.
- `EngineCmd::EnterOpTextObj { op, count1, inner }` — emitted on `i` / `a`; host
  calls `Editor::enter_op_text_obj` to set `Pending::OpTextObj`.
- `EngineCmd::EnterOpG { op, count1 }` — emitted on `g`; host calls
  `Editor::enter_op_g` to set `Pending::OpG`.
- `EngineCmd::EnterOpFind { op, count1, forward, till }` — emitted on `f` / `F`
  / `t` / `T`; host calls `Editor::enter_op_find` to set `Pending::OpFind`.

## [0.5.0] - 2026-05-13

### Added

- `PendingState::AfterZ { count }` — pending variant for the bare `z<x>` chord.
  Hosts set this variant after intercepting `z`; `step` routes the next
  `Key::Char(ch)` to `EngineCmd::AfterZChord { ch, count }` or cancels on
  `Key::Esc` / any non-char key (mirrors the `AfterG` arm).
- `EngineCmd::AfterZChord { ch, count }` — emitted by the `AfterZ` reducer arm;
  host calls `Editor::after_z(ch, count)` on receipt.

## [0.4.0] - 2026-05-13

### Added

- `PendingState::AfterG { count }` — pending variant for the bare `g<x>` chord.
  Hosts set this variant after intercepting `g`; `step` routes the next
  `Key::Char(ch)` to `EngineCmd::AfterGChord { ch, count }` or cancels on
  `Key::Esc` / any non-char key (mirrors the `Find` arm).
- `EngineCmd::AfterGChord { ch, count }` — emitted by the `AfterG` reducer arm;
  host calls `Editor::after_g(ch, count)` on receipt.

## [0.3.0] - 2026-05-13

### Added

- `PendingState::Find { count, forward, till }` — pending variant for `f<x>` /
  `F<x>` / `t<x>` / `T<x>` bare find chords. Hosts set this variant; `step`
  routes the next `Key::Char` to `EngineCmd::FindChar` or cancels on `Key::Esc`
  / any non-char key.
- `EngineCmd::FindChar { ch, forward, till, count }` — emitted by the `Find`
  reducer arm; host calls `Editor::find_char` on receipt.

## [0.2.0] - 2026-05-12

### Added

- `PendingState` enum: app-level chord accumulator; initial variant
  `Replace { count }`.
- `Outcome` enum: reducer result — `Wait`, `Commit`, `Cancel`, `Forward`.
- `Key` enum: crossterm-free key representation for the pending-state reducer.
- `step(state, key) -> Outcome`: pure reducer driving `PendingState`
  transitions.
- `EngineCmd` enum: controller commands emitted to the host; initial variant
  `ReplaceChar { ch, count }`.
- Re-exports at crate root: `PendingState`, `Outcome`, `Key`, `step`,
  `EngineCmd`.

## [0.1.0] - 2026-05-12

### Added

- Initial release: `Mode` enum extracted from `apps/hjkl::keymap::HjklMode`.
  Future phases will land the vim FSM here.

[Unreleased]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.23.0...HEAD
[0.23.2]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.23.1...v0.23.2
[0.23.1]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.23.0...v0.23.1
[0.23.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.22.0...v0.23.0
[0.22.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.21.0...v0.22.0
[0.21.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.20.0...v0.21.0
[0.20.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.19.0...v0.20.0
[0.19.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.18.1...v0.19.0
[0.18.1]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.18.0...v0.18.1
[0.18.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.17.0...v0.18.0
[0.17.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.16.0...v0.17.0
[0.16.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.15.0...v0.16.0
[0.15.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.14.0...v0.15.0
[0.14.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.13.0...v0.14.0
[0.13.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/kryptic-sh/hjkl-vim/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/kryptic-sh/hjkl-vim/releases/tag/v0.1.0
