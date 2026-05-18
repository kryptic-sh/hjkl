# Editor field classification

Classification of every `hjkl_engine::Editor` (and embedded `VimState`) field as
per-buffer, per-window, or global, in preparation for the per-window Editor
refactor (#151, phases B–E).

## Notable findings

**Cursor lives inside `Buffer`, not `Editor` (discovered Phase A.5).** Prior to
0.8.0, `hjkl_buffer::Buffer` mixed per-document state (text rows, dirty gen,
folds) with the per-window `cursor: Position` field. All cursor reads in
`editor.rs` went through `buf_cursor_rc(&self.buffer)`. This meant that naively
making `Editor` take `Arc<Buffer>` (Phase C) would have caused two windows
sharing the same `Arc<Buffer>` to share their cursor — semantically wrong for
splits.

**Helix Document+View pattern adopted (Phase A.5, #158).** `Buffer` was split
into:

- `Content` — Arc-shareable per-document state (`lines`, `dirty_gen`, `folds`).
  Wrapped in `Arc<Mutex<Content>>`.
- `Buffer` — per-window view holding `Arc<Mutex<Content>>` + `cursor: Position`.

`Buffer::new_view(Arc<Mutex<Content>>)` creates a second window onto the same
document with an independent cursor. See #158 for the architectural decision.

## Editor fields

| Field                              | Current line | Type                                              | Classification                                         | New home                                                        | Rationale                                                                                                                                                             |
| ---------------------------------- | ------------ | ------------------------------------------------- | ------------------------------------------------------ | --------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `keybinding_mode`                  | 548          | `KeybindingMode`                                  | per-window                                             | Editor                                                          | active binding scheme is a window preference (e.g. normal vs emacs on a per-pane basis)                                                                               |
| `last_yank`                        | 550          | `Option<String>`                                  | global                                                 | Registers                                                       | legacy mirror of `registers.unnamed`; per-editor comment says "will be removed at 0.1.0"; logically the same as the unnamed register                                  |
| `vim`                              | 555          | `VimState`                                        | per-window                                             | Editor                                                          | entire FSM — mode, pending chord, visual anchors, jump list — is per active cursor                                                                                    |
| `undo_stack`                       | 559          | `Vec<(Vec<String>, (usize, usize))>`              | per-buffer                                             | Buffer                                                          | vim undo is per buffer; two windows on the same buffer share undo history                                                                                             |
| `redo_stack`                       | 561          | `Vec<(Vec<String>, (usize, usize))>`              | per-buffer                                             | Buffer                                                          | paired with `undo_stack`; same reasoning                                                                                                                              |
| `content_dirty`                    | 563          | `bool`                                            | per-buffer                                             | Buffer                                                          | tracks whether the buffer has unsaved changes; independent of which window is viewing it                                                                              |
| `cached_content`                   | 568          | `Option<Arc<String>>`                             | per-buffer                                             | Buffer                                                          | caches `lines().join("\n")` — a property of buffer content, not the viewing window                                                                                    |
| `viewport_height`                  | 573          | `AtomicU16`                                       | per-window                                             | Editor (Host)                                                   | each window has its own physical height; viewport already lives on `Host::viewport_mut()`                                                                             |
| `pending_lsp`                      | 577          | `Option<LspIntent>`                               | per-window                                             | Editor                                                          | LSP intents are triggered from the active window's cursor position                                                                                                    |
| `pending_fold_ops`                 | 585          | `Vec<FoldOp>`                                     | per-buffer                                             | Buffer                                                          | fold state is per buffer; all windows on the same buffer share fold ranges                                                                                            |
| `buffer`                           | 592          | `B` (generic, default `hjkl_buffer::Buffer`)      | per-buffer                                             | `Arc<Buffer>`                                                   | the rope + cursor + folds; will become a shared `Arc<Buffer>` per #151                                                                                                |
| `style_table` (ratatui feature)    | 605          | `Vec<ratatui::style::Style>`                      | per-window                                             | Editor                                                          | style intern table is populated per render pass and tied to a viewport's syntax context                                                                               |
| `engine_style_table` (non-ratatui) | 611          | `Vec<crate::types::Style>`                        | per-window                                             | Editor                                                          | same as `style_table` but for non-ratatui hosts                                                                                                                       |
| `registers`                        | 616          | `crate::registers::Registers`                     | global                                                 | Registers                                                       | app copies registers across buffer switches (`buffer_ops.rs:18`); vim's registers are instance-global                                                                 |
| `styled_spans` (ratatui feature)   | 624          | `Vec<Vec<(usize, usize, ratatui::style::Style)>>` | per-window (near-term; per-document target in Phase D) | Editor                                                          | syntax spans are re-installed per parse through an `Editor` method today; hoisting to `Arc<Content>` requires syntax worker to write through the Arc — Phase D target |
| `settings`                         | 629          | `Settings`                                        | per-window                                             | Editor                                                          | `:set wrap`, `:set number`, etc. are per-window in vim                                                                                                                |
| `marks`                            | 644          | `BTreeMap<char, (usize, usize)>`                  | mixed — see below                                      | Buffer (`'a`–`'z`), Editor (`'.` `'[` `']` `'<` `'>` `'A`–`'Z`) | see ambiguous decisions                                                                                                                                               |
| `syntax_fold_ranges`               | 650          | `Vec<(usize, usize)>`                             | per-buffer                                             | Buffer                                                          | host-installed syntax fold ranges describe buffer structure, shared across windows                                                                                    |
| `change_log`                       | 659          | `Vec<crate::types::Edit>`                         | per-buffer                                             | Buffer                                                          | edit log records buffer mutations; all windows sharing a buffer must see all edits                                                                                    |
| `sticky_col`                       | 668          | `Option<usize>`                                   | per-window                                             | Editor                                                          | vim curswant is per active cursor; already hoisted to Editor ownership at 0.0.28                                                                                      |
| `host`                             | 676          | `H` (generic, default `DefaultHost`)              | per-window                                             | Editor                                                          | host carries the viewport and cursor-shape channel — inherently per window                                                                                            |
| `last_emitted_mode`                | 681          | `crate::VimMode`                                  | per-window                                             | Editor                                                          | tracks mode transitions for cursor-shape emission; each window has its own mode                                                                                       |
| `search_state`                     | 688          | `crate::search::SearchState`                      | per-window                                             | Editor                                                          | vim last-search-pos is per window; `n`/`N` in window 1 must not advance window 2's match cursor                                                                       |
| `buffer_spans`                     | 699          | `Vec<Vec<hjkl_buffer::Span>>`                     | per-window (near-term; per-document target in Phase D) | Editor                                                          | syntax spans derive from buffer content — stays on Editor near-term; target per-document in Phase D when syntax worker is wired through `Arc<Content>`                |
| `pending_content_edits`            | 704          | `Vec<crate::types::ContentEdit>`                  | per-buffer                                             | Buffer                                                          | byte-level content change records for external observers (syntax trees); tied to buffer mutations                                                                     |
| `pending_content_reset`            | 709          | `bool`                                            | per-buffer                                             | Buffer                                                          | signals full buffer replacement; a buffer-level event, not window-specific                                                                                            |
| `last_indent_range`                | 715          | `Option<(usize, usize)>`                          | per-window                                             | Editor                                                          | auto-indent flash range is a UX artefact of the active editing session in this window                                                                                 |

## VimState sub-struct fields

| Field                     | Current line | Type                           | Classification | New home   | Rationale                                                                        |
| ------------------------- | ------------ | ------------------------------ | -------------- | ---------- | -------------------------------------------------------------------------------- |
| `mode`                    | 351          | `Mode`                         | per-window     | Editor.vim | each window has its own normal/insert/visual state                               |
| `pending`                 | 353          | `Pending`                      | per-window     | Editor.vim | in-flight chord is per active cursor                                             |
| `count`                   | 356          | `usize`                        | per-window     | Editor.vim | digit prefix for next command; per active cursor                                 |
| `last_find`               | 358          | `Option<(char, bool, bool)>`   | per-window     | Editor.vim | `;`/`,` repeat target is per window                                              |
| `last_change`             | 360          | `Option<LastChange>`           | per-window     | Editor.vim | `.` dot-repeat is per window (vim convention)                                    |
| `insert_session`          | 362          | `Option<InsertSession>`        | per-window     | Editor.vim | tracks the current insert-mode entry; inherently per active cursor               |
| `visual_anchor`           | 366          | `(usize, usize)`               | per-window     | Editor.vim | char-wise Visual mode anchor; each window has its own selection                  |
| `visual_line_anchor`      | 368          | `usize`                        | per-window     | Editor.vim | VisualLine mode row anchor; per window                                           |
| `block_anchor`            | 371          | `(usize, usize)`               | per-window     | Editor.vim | VisualBlock anchor corner; per window                                            |
| `block_vcol`              | 377          | `usize`                        | per-window     | Editor.vim | virtual column for VisualBlock active corner; per window                         |
| `yank_linewise`           | 379          | `bool`                         | per-window     | Editor.vim | linewise flag for the pending paste; set by the most recent yank in this window  |
| `pending_register`        | 382          | `Option<char>`                 | per-window     | Editor.vim | `"reg` prefix for next operator; per active cursor                               |
| `recording_macro`         | 386          | `Option<char>`                 | per-window     | Editor.vim | `q{reg}` target; macro recording is a per-window operation                       |
| `recording_keys`          | 391          | `Vec<Input>`                   | per-window     | Editor.vim | captured keys for the in-progress macro; per window                              |
| `replaying_macro`         | 394          | `bool`                         | per-window     | Editor.vim | replay guard; per window                                                         |
| `last_macro`              | 396          | `Option<char>`                 | per-window     | Editor.vim | `@@` target; per window                                                          |
| `last_edit_pos`           | 399          | `Option<(usize, usize)>`       | per-window     | Editor.vim | `'.` / `` `. `` marks — last edit position in this window's session              |
| `last_insert_pos`         | 403          | `Option<(usize, usize)>`       | per-window     | Editor.vim | `gi` return target; per window                                                   |
| `change_list`             | 407          | `Vec<(usize, usize)>`          | per-window     | Editor.vim | `g;`/`g,` change-position ring; vim's change list is per window                  |
| `change_list_cursor`      | 410          | `Option<usize>`                | per-window     | Editor.vim | walk cursor into `change_list`; per window                                       |
| `last_visual`             | 413          | `Option<LastVisual>`           | per-window     | Editor.vim | `gv` restore of previous visual selection; per window                            |
| `viewport_pinned`         | 417          | `bool`                         | per-window     | Editor.vim | `zz`/`zt`/`zb` pin flag for the current step; per window                         |
| `replaying`               | 419          | `bool`                         | per-window     | Editor.vim | dot-replay guard; per window                                                     |
| `one_shot_normal`         | 421          | `bool`                         | per-window     | Editor.vim | `Ctrl-o` one-shot normal mode; per active cursor                                 |
| `search_prompt`           | 424          | `Option<SearchPrompt>`         | per-window     | Editor.vim | live `/`/`?` prompt text; per window                                             |
| `last_search`             | 428          | `Option<String>`               | per-window     | Editor.vim | last committed search pattern for `n`/`N`; per window (vim convention — #151 Q2) |
| `last_search_forward`     | 432          | `bool`                         | per-window     | Editor.vim | direction for `n`/`N`; per window                                                |
| `jump_back`               | 437          | `Vec<(usize, usize)>`          | per-window     | Editor.vim | back jumplist for `Ctrl-o`; vim jumplists are per window                         |
| `jump_fwd`                | 440          | `Vec<(usize, usize)>`          | per-window     | Editor.vim | forward jumplist for `Ctrl-i`; per window                                        |
| `insert_pending_register` | 444          | `bool`                         | per-window     | Editor.vim | `Ctrl-R` flag in insert mode; per active cursor                                  |
| `change_mark_start`       | 450          | `Option<(usize, usize)>`       | per-window     | Editor.vim | stashed `'[` start for Change op; per window                                     |
| `search_history`          | 454          | `Vec<String>`                  | per-window     | Editor.vim | committed `/`/`?` patterns for `Ctrl-P`/`Ctrl-N` in prompt; per window           |
| `search_history_cursor`   | 459          | `Option<usize>`                | per-window     | Editor.vim | walk cursor into `search_history`; per window                                    |
| `last_input_at`           | 468          | `Option<std::time::Instant>`   | per-window     | Editor.vim | wall-clock timestamp of last keystroke for `:set timeoutlen`; per window         |
| `last_input_host_at`      | 472          | `Option<core::time::Duration>` | per-window     | Editor.vim | `Host::now()` reading at last keystroke; per window                              |
| `current_mode`            | 478          | `crate::VimMode`               | per-window     | Editor.vim | canonical public mode (mirrors `mode`); per window                               |

## Ambiguous decisions

### `marks` (Editor field, line 644)

The unified `BTreeMap<char, (usize, usize)>` stores two conceptually distinct
groups under one field.

**Decision**:

- **Lowercase `'a`–`'z`** → **per-buffer** (move to `Buffer`). Vim's `:h marks`
  says lowercase marks are buffer-local; `'a` set in window 1 is visible via
  `` `a `` in window 2 when both view the same buffer.
- **Uppercase `'A`–`'Z`** (file marks) → **per-window / Editor**. In vim, file
  marks survive tab switches within the same session and are associated with a
  file path, not just buffer content. The code comment notes they survive
  `Editor::set_content` calls "across tab swaps within the same Editor"; in the
  refactored shape an uppercase mark maps to an `(Arc<Buffer>, row, col)`
  triple, which can live on the Editor.
- **Special marks `'.` `'[` `']` `'<` `'>` `'gi`** — these are stored via
  `VimState` fields (`last_edit_pos`, `change_mark_start`, `last_visual`) rather
  than in the `marks` BTreeMap, so they are already classified above as
  per-window.

**Split action**: after #151 phase B, replace the single `marks` field with
`buffer_marks: BTreeMap<char, (usize, usize)>` on `Buffer` (lowercase) and
`file_marks: BTreeMap<char, (usize, usize)>` on `Editor` (uppercase).

### `registers` (Editor field, line 616)

Currently one `Registers` bank per `Editor`. `apps/hjkl/src/app/buffer_ops.rs`
copies the full register bank when switching buffer slots (`switch_to` clones
registers from the old slot and overwrites the new slot) — an explicit
workaround for registers being per-editor when they should be global.

**Decision**: **global** — move to a `Registers` value owned by `App` (or a
shared `Arc<Mutex<Registers>>`). Every `Editor` gets a reference rather than
owning its own copy. Matches vim's single-instance register bank.

**Rationale**: vim registers (`"a`–`"z`, `"0`–`"9`, `"`, `"+`, `"*`) are
process-global, not per-window. The workaround in `buffer_ops.rs:18-19` is
direct evidence the current shape is wrong.

### `styled_spans` / `buffer_spans` (Editor fields, lines 624 and 699)

Syntax spans derive from buffer content (tree-sitter parse) and are re-installed
by the host on every completed parse. Currently stored per-editor.

**Decision**: **per-window** (stays on Editor) in the near term, with a
longer-term path to per-buffer.

**Rationale**: in a multi-window scenario both windows viewing the same buffer
should display the same syntax colours, so logically spans belong on `Buffer`.
However, the install path (`install_syntax_spans` /
`install_ratatui_syntax_spans`) goes through an `Editor` method today, and
`buffer_spans` feeds into `BufferView::spans` which is parameterised per render
call. Hoisting spans to `Arc<Buffer>` requires the syntax worker to write
through the `Arc` rather than through the `Editor`. Treat as per-window for
phase B/C; target per-buffer in phase D when the syntax worker is wired through
`Arc<Buffer>`.

### `last_yank` (Editor field, line 550)

The doc comment states this is a "legacy mirror of the unnamed register" that
"will be removed at 0.1.0". It is written by `record_yank_to_host` alongside
`Host::write_clipboard`. No app-level code reads `.last_yank` directly (grep
finds only engine-internal and test usages).

**Decision**: **global** (remove alongside `registers` consolidation). After the
Registers struct moves to App-level, `last_yank` becomes dead code and is
deleted as scheduled.

### `settings` (Editor field, line 629)

Contains both window-rendering options (`:set wrap`, `:set number`,
`:set cursorline`) and buffer-semantic options (`:set readonly`,
`:set autoindent`, `undo_levels`, `iskeyword`).

**Decision**: **per-window** — keep `Settings` on Editor in full. Vim's `:set`
applies per-window even for options that happen to affect buffer semantics (e.g.
`:set readonly` is per-buffer in vim but implemented as per-window in the
current engine). Splitting `Settings` into a `BufferSettings` sub-struct is a
follow-on concern beyond the scope of #151.

## Per-buffer field count: 11

`undo_stack`, `redo_stack`, `content_dirty`, `cached_content`,
`pending_fold_ops`, `buffer`, `syntax_fold_ranges`, `change_log`,
`pending_content_edits`, `pending_content_reset`, and the lowercase `'a`–`'z`
portion of `marks`.

## Per-window field count: 51

All VimState fields (36) plus `keybinding_mode`, `vim` (the VimState itself),
`viewport_height`, `pending_lsp`, `style_table` / `engine_style_table`,
`settings`, `sticky_col`, `host`, `last_emitted_mode`, `search_state`,
`buffer_spans`, `styled_spans`, `last_indent_range`, and the uppercase `'A`–`'Z`
portion of `marks`. (Corrected from 40 in #153 — previous count understated
VimState at 35 and omitted several non-VimState per-window fields.)

## Global field count: 2

`registers` and `last_yank` (the latter is a scheduled-for-removal mirror of the
unnamed register slot).
