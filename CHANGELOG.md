# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/) once it reaches
0.1.0; the 0.0.x series is a churn phase where breaking changes may land on
patch bumps.

## [Unreleased]

## [0.15.1] - 2026-05-14

### Fixed

- Gate `apps/hjkl/tests/e2e.rs::pty_harness` on `cfg(unix)`. ConPTY +
  portable-pty on Windows behaves differently enough that the harness assertions
  don't hold (cursor reads return 0,0; rendered rows don't carry the expected
  gutter format). Windows CI now green; unix coverage unchanged (13 e2e tests
  still run on linux/macOS). v0.15.0 tag was created but never reached publish
  steps; v0.15.1 is the first 0.15.x line that ships.

## [0.15.0] - 2026-05-14

### Added

- **Phase 5a — marks chord lift (#71).** `m<x>`, `'<x>`, `` `<x> `` chords route
  through the hjkl-vim reducer (`PendingState::SetMark` /
  `PendingState::GotoMarkLine` / `PendingState::GotoMarkChar`) and dispatch
  `EngineCmd::SetMark` / `GotoMarkLine` / `GotoMarkChar` to
  `Editor::set_mark_at_cursor` / `goto_mark_line` / `goto_mark_char` instead of
  re-entering the engine FSM.
- **Phase 5b — macro chord lift (#71).** `q<x>` and `@<x>` chords route through
  `PendingState::RecordMacroTarget` and
  `PendingState::PlayMacroTarget { count }`. `EngineCmd::StartMacroRecord` /
  `EngineCmd::PlayMacro` dispatch to `Editor::start_macro_record` /
  `Editor::play_macro`. Macro replay flows back through `route_chord_key` (Phase
  6 prereq).
- **Phase 5c — dot-repeat (`.`) lift (#71).** `AppAction::DotRepeat { count }`
  resolves the count prefix and calls `Editor::replay_last_change`. Engine FSM
  `.` arm preserved for macro-replay defensive coverage.
- **Phase 5d — `@:` last-ex repeat (#71).** App captures every executed `:`
  command into `last_ex_command: Option<String>`. The `PlayMacroTarget` reducer
  accepts `':'` and the host calls `replay_last_ex`. Normal-mode `:` interceptor
  now guarded with `pending_state.is_none()` so `@:` does not open the command
  prompt.
- **Phase 5e — count + register-routing audit + bug fixes (#71).** New unit
  - e2e tests cover `5dd`, `"a5dd`, `5"add`, `"a5x`, `3@a`, `5.`, `"add`×2,
    `"+y`, `"_d`. Fixed `5"add` count-drop (removed `pending_count.reset()` from
    `BeginPendingSelectRegister` arm) and `"a5dd` premature digit flush
    (count-prefix block now skipped when `pending_state.is_some()`).
- **Phase 3e/f/g — additional motions via hjkl-vim keymap path (#69).** `;` `,`
  `%` `H` `M` `L` `<C-d>` `<C-u>` `<C-f>` `<C-b>` route through
  `AppAction::Motion { kind, count }` → `Editor::apply_motion` instead of the
  engine FSM. Engine FSM arms preserved for macro-replay coverage.
- **Phase 4e — visual-mode operators dispatch via keymap (#70).**
  `AppAction::VisualOp { op, count }` resolves the active selection range and
  calls a range-mutation primitive; engine exits visual mode. Visual, VisualLine
  and VisualBlock all covered (VisualBlock falls back to the engine FSM for
  shapes that need `apply_block_operator`). Named-register routing (`"a y` then
  visual `p`) now honored on the visual path.
- **e2e PTY+vt100 test harness** (`apps/hjkl/tests/pty_harness/`). `portable-pty
  - vt100`driven`TerminalSession`lets tests assert against actual rendered terminal output. Initial coverage:`render_sync.rs`(5 historical bugs:`:100`, `gg`, visual `gg`, `30j`, `G`), `at_colon.rs`(Phase 5d), and`register_count.rs`(round-trip paste,`5"add`, `3@a`).
- `route_chord_key` central dispatcher (apps/hjkl/src/app/mod.rs) — single entry
  point for all chord routing. Engine-pending bypass + reducer step in one
  helper. Macro replay now flows through this path so future Phase 6 engine
  slimming doesn't regress macro behavior.
- `flush_pending_count_to_engine` and `sync_after_engine_mutation` helpers in
  event_loop.rs for centralised count-flush and viewport-sync semantics.

### Changed

- **`cursorline` default flipped from `false` → `true`** (carried via
  `hjkl-engine` 0.6.8). Existing `:set nocursorline` config still toggles off.
- `pending_count: String` migrated to `hjkl_vim::CountAccumulator` —
  digit-prefix buffer with vim's digit-0-vs-LineStart quirk and overflow
  saturation built in.
- Bumped `hjkl-engine` 0.6.7 → 0.6.8 (Phase 5 controllers + cursorline default
  flip).
- Bumped `hjkl-vim` 0.18.0 → 0.18.1 (Phase 5b/5d macro reducer states +
  `EngineCmd` variants).
- Bumped `hjkl-editor` 0.4.5 → 0.4.6 (engine 0.6.8 dep + cursorline snapshot +
  `:100` engine-layer regression test).

### Fixed

- **`:100<Enter>` cursor-stuck regression** (commit 23cb46b). Engine cursor
  moved to line 100 but the window cursor cache stayed at the old row.
  `dispatch_ex` now calls `sync_viewport_from_editor` after every `ex::run`
  regardless of effect — cursor-only ex commands no longer skip the sync.
- **Render-sync class fixes** (commits 219de02, 0694b42, 1cead4e, 4414170,
  b8d0459): keymap-Match dispatch, non-Normal keymap Match, keymap-dispatched
  motion viewport scroll, pending-state Outcome arms, and VisualBlock
  `block_vcol` all now route through `sync_after_engine_mutation`.
- **`gg` / visual-`gg` routing-order fixes** (commits 0621944, 76f3f55):
  pending_state reducer lifted out of Normal-mode gate; non-Normal trie dispatch
  gated on `pending_state.is_none()`.

## [0.14.11] - 2026-05-13

### Changed

- Phase 3d of the vim FSM extraction (#62, tracking #69) — `G` now routes
  through the `hjkl-vim` keymap path. `AppAction::Motion { kind, count }`
  dispatches to `Editor::apply_motion` instead of re-entering the engine FSM.
  Engine FSM arm for `G` is kept for macro-replay coverage. `gg` stays on the
  G-chord path from Phase 2b-ii.
- Bumped `hjkl-vim` 0.14 → 0.15 — adds `MotionKind::GotoLine`. Count 1 means
  last line, count > 1 means goto line N.
- Bumped `hjkl-engine` 0.6.3 → 0.6.4 — `apply_motion` routes `GotoLine` to
  `Motion::FileBottom`.

### Added

- `G` entry in the motion binding loop across Normal / Visual / VisualLine /
  VisualBlock. `'G'` already in `could_start_chord` (no event_loop change
  needed).

## [0.14.10] - 2026-05-13

### Changed

- Phase 3c of the vim FSM extraction (#62, tracking #69) — line-anchored motions
  (`0` / `^` / `$`, plus `<Home>` / `<End>` aliases) now route through the
  `hjkl-vim` keymap path. The host dispatches
  `AppAction::Motion { kind, count }` and calls `Editor::apply_motion` instead
  of re-entering the engine FSM. Engine FSM arms remain for macro-replay
  coverage. `g_` (last non-blank) stays on the G-chord path from Phase 2b-ii.
  `|` (column N) is not yet implemented in the engine and is deferred.
- Bumped `hjkl-vim` 0.13 → 0.14 — adds 3 new `MotionKind` variants (`LineStart`
  / `FirstNonBlank` / `LineEnd`).
- Bumped `hjkl-engine` 0.6.2 → 0.6.3 — `apply_motion` now handles the 3 new
  variants via `Motion::LineStart` / `Motion::FirstNonBlank` /
  `Motion::LineEnd`.

### Added

- 5 entries in the motion binding loop (`apps/hjkl/src/app/mod.rs`) for `0`,
  `<Home>`, `^`, `$`, `<End>` across Normal / Visual / VisualLine / VisualBlock.
  `^` and `$` added to `could_start_chord` matches in `event_loop.rs` (digit `0`
  already handled by the digit-buffer split). `<Home>` / `<End>` added to the
  non-Char branch alongside `<BS>`.

## [0.14.9] - 2026-05-13

### Changed

- Phase 3b of the vim FSM extraction (#62, tracking #69) — word motions (`w` /
  `W` / `b` / `B` / `e` / `E`) now route through the `hjkl-vim` keymap path. The
  host dispatches `AppAction::Motion { kind, count }` and calls
  `Editor::apply_motion` instead of re-entering the engine FSM. Engine FSM arms
  for these keys are intentionally kept so the macro- replay path (`@<reg>`)
  continues to resolve them. `ge` / `gE` were already covered by the G-chord
  migration in Phase 2b-ii.
- Bumped `hjkl-vim` 0.12 → 0.13 — adds 6 word-motion `MotionKind` variants
  (`WordForward` / `BigWordForward` / `WordBackward` / `BigWordBackward` /
  `WordEnd` / `BigWordEnd`).
- Bumped `hjkl-engine` 0.6.1 → 0.6.2 — `apply_motion` now handles the 6 new
  word-motion variants by reusing the same `execute_motion` primitives
  (`Motion::WordFwd` / `BigWordFwd` / `WordBack` / `BigWordBack` / `WordEnd` /
  `BigWordEnd`) the FSM arms call.

### Added

- 6 entries in the Phase 3a motion binding loop (`apps/hjkl/src/app/mod.rs`) for
  `w` / `W` / `b` / `B` / `e` / `E` across Normal / Visual / VisualLine /
  VisualBlock. Count-prefix buffering (`5w` etc.) preserved by extending the
  `could_start_chord` matches in `event_loop.rs`.

## [0.14.8] - 2026-05-13

### Changed

- Phase 3a of the vim FSM extraction (#62, tracking #69) — char + line motions
  (`h` / `l` / `<BS>` / `<Space>` / `j` / `k` / `+` / `-`) now route through the
  `hjkl-vim` keymap path. The host dispatches
  `AppAction::Motion { kind, count }` and calls `Editor::apply_motion` instead
  of re-entering the engine FSM. Engine FSM arms for these keys are
  intentionally kept so the macro- replay path (`@<reg>`) continues to resolve
  them.
- Bumped `hjkl-vim` 0.11 → 0.12 — adds `MotionKind` enum carrying the keymap-
  layer motion identity (`CharLeft` / `CharRight` / `LineDown` / `LineUp` /
  `FirstNonBlankDown` / `FirstNonBlankUp`). Marked `#[non_exhaustive]` so Phases
  3b–3g can extend without breaking downstream match arms.
- Bumped `hjkl-engine` 0.6.0 → 0.6.1 — adds `Editor::apply_motion(kind, count)`
  controller method backed by an internal `apply_motion_kind` helper that reuses
  the same motion primitives as the FSM. Cursor, sticky column, scroll, and sync
  semantics are identical between the two paths.

### Added

- New `AppAction::Motion { kind: MotionKind, count: u32 }` variant in
  `apps/hjkl/src/keymap_actions.rs`. Bound across Normal / Visual / VisualLine /
  VisualBlock for the 8 Phase 3a motions. Count-prefix buffering (`5j` etc.) is
  preserved by extending the `could_start_chord` set in `event_loop.rs` to cover
  the newly keymap-bound keys.

## [0.14.7] - 2026-05-13

### Fixed

- `:nmap` cyclic-recursion guard (`MAX_DEPTH` in `dispatch_action`'s `Replay`
  arm) lowered from 1024 to 128 to fit comfortably within macOS's 512 KB
  per-thread stack default. Previously
  `cyclic_recursive_map_bails_without_stack_overflow` intermittently SIGABRT'd
  on macOS CI before the depth guard fired. 128 is still far beyond any
  realistic nested-map depth.

### Changed

- Phase 2c of the vim FSM extraction (#62) — bare op-pending (`d` / `y` / `c` /
  `>` / `<`), `OpFind` / `OpTextObj` / `OpG` sub-states, chord-init case-ops
  (`gu` / `gU` / `g~` / `gq`), and register prefix (`"<reg>`) all migrated from
  the engine FSM to the `hjkl-vim` reducer. The reducer now owns the entire
  op-pending state machine for user keystrokes; engine `Pending::Op*` arms
  remain only for macro-replay defensive coverage.
- Bumped `hjkl-vim` 0.5 → 0.11 across chunks 2c-i..vi:
  - 0.6 — `OperatorKind` + `PendingState::AfterOp` (chunk 2c-i).
  - 0.7 — `PendingState::OpFind` + `EngineCmd::ApplyOpFind` (chunk 2c-ii).
  - 0.8 — `PendingState::OpTextObj` + `EngineCmd::ApplyOpTextObj` (chunk
    2c-iii).
  - 0.9 — `PendingState::OpG` + `EngineCmd::ApplyOpG` (chunk 2c-iv).
  - 0.10 — `OperatorKind::{Uppercase, Lowercase, ToggleCase, Reflow}` for
    chord-init case-op bridge (chunk 2c-v).
  - 0.11 — `PendingState::SelectRegister` + `EngineCmd::SetPendingRegister`
    (chunk 2c-vi).
- Bumped `hjkl-engine` 0.5.8 → 0.6.0 across chunks 2c-i..vii:
  - 0.5.12..0.5.17 — `Editor::apply_op_motion` / `apply_op_double` /
    `apply_op_find` / `apply_op_text_obj` / `apply_op_g` /
    `set_pending_register` controller entry points.
  - 0.6.0 (breaking) — removed transitional `enter_op_text_obj` / `enter_op_g` /
    `enter_op_find` controllers, now superseded by the matching `apply_op_*`
    methods (chunk 2c-vii).
- Bumped `hjkl-form` 0.3.5 → 0.3.6, `hjkl-editor` 0.4.4 → 0.4.5, and
  `hjkl-ratatui` 0.3.5 → 0.3.6 — caret bumps to `hjkl-engine ^0.6`, no API
  changes.

## [0.14.6] - 2026-05-13

### Changed

- Phase 2b of the vim FSM extraction (#62): the three bare second-char chord
  families — find (`f`/`F`/`t`/`T`), g-prefix, and z-prefix — are now driven by
  the `hjkl-vim` reducer instead of the engine FSM. User-visible behavior
  unchanged; chord dispatch now lives in the host's pending-state loop and calls
  controller methods on `Editor` (`find_char`, `after_g`, `after_z`). Engine
  `Pending::Find` / `Pending::G` / `Pending::Z` arms remain intact for the
  operator-pending variants (`OpFind`, `OpG`) which migrate in chunk 2c.
- Bumped `hjkl-vim` to 0.5 across the three chunks:
  - 0.3 — `PendingState::Find` + `EngineCmd::FindChar` (chunk 2b-i).
  - 0.4 — `PendingState::AfterG` + `EngineCmd::AfterGChord` (chunk 2b-ii).
  - 0.5 — `PendingState::AfterZ` + `EngineCmd::AfterZChord` (chunk 2b-iii).
- Bumped `hjkl-engine` to 0.5.11 across the three chunks:
  - 0.5.9 — `Editor::find_char` controller entry.
  - 0.5.10 — `Editor::after_g` controller entry.
  - 0.5.11 — `Editor::after_z` controller entry.

### Added

- New `AppAction` variants `BeginPendingFind`, `BeginPendingAfterG`,
  `BeginPendingAfterZ` route the app's `f`/`g`/`z` bindings (Normal + Visual)
  through the hjkl-vim reducer.
- Phase 2b-ii pulled 7 overlapping `g*` prefix entries (`gt`, `gd`, `gD`, `gr`,
  `gi`, `gy`, …) out of the static keymap trie to resolve the bare-`g`
  ambiguity; their dispatch now flows through the `AfterGChord` arm. No
  user-visible change.

## [0.14.5] - 2026-05-13

### Fixed

- Bumped `hjkl-engine` to 0.5.8: 5 vim-compat divergences fixed and their oracle
  cases re-promoted from `known_divergences.toml` to active tier-2 corpus files
  (kryptic-sh/hjkl#83):
  - Dot mark `'.`/`` `. `` records change-start position, not post-insert cursor
    (`mark_dot_jump_to_last_edit`).
  - `100G` clamps to last content row on trailing-newline buffers
    (`count_100G_clamps_to_last_line`).
  - `gi` moves to last-insert position and enters insert mode
    (`gi_resume_last_insert`).
  - Visual-block `c<text><Esc>` cursor lands on last inserted char
    (`visual_block_jl_c_change_block`).
  - `"_` (black-hole) register discards deletes without touching unnamed
    register (`register_blackhole_d`).
- `hjkl-engine` 0.5.7 fix: `` `< `` / `` `> `` (and `'<` / `'>` linewise
  variants) now resolve correctly through `handle_goto_mark`. The marks were set
  by the visual-exit hook (0.5.3) but the goto-mark dispatcher didn't list `<` /
  `>` in its target match, so `` `< `` silently no-op'd. Surfaced by the oracle
  tier-2 marks corpus.

### Changed

- Bumped `hjkl-anvil` dependency to `0.2` (TOFU checksum verification). GitHub
  tool installs (rust-analyzer, lua-language-server) no longer fail on
  placeholder zero SHAs; the first download's hash is recorded and enforced on
  subsequent installs.

### Added

- Phase 2 chunk 2a (#68): `hjkl-vim` now exports `PendingState`, `Outcome`,
  `Key`, `step`, and `EngineCmd` (v0.2.0). The `r<x>` replace chord is
  intercepted by the app keymap trie (`BeginPendingReplace`) and driven by
  `hjkl_vim::step` — the engine's `Pending::Replace` arm is now unreachable from
  the umbrella but left intact. `Editor::replace_char_at` promoted to public in
  `hjkl-engine` v0.5.5 as the controller entry point.
- `hjkl-engine` v0.5.6 (#81): vim special marks `[` / `]` (last yank / change /
  paste bounds). After any `y<motion>`, `d<motion>`, `c<motion>`, `p`, or `P`,
  `` `[ `` jumps to the first affected char and `` `] `` to the last. Mode-aware
  positioning (linewise, charwise, blockwise). Enables the `` `[v`] `` re-select
  idiom. Backtick mark jumps now work in Visual modes.
- Oracle (`hjkl-compat-oracle`) tier-2 corpus expansion (#82): 5 → 16 test
  functions, ~120 cases covering marks, visual mode, dot-repeat, search,
  substitute, macros, case/join, paragraph/word, text objects, visual block, and
  registers/increment/insert shortcuts. Acts as a regression net for the
  upcoming `#62` FSM extraction and `#80` ex-extraction refactors. Tier-1
  backfilled with 13 basic cases (`x`, `X`, `r`, `~`, `J`, `p`, `P`, `W`, `B`,
  `E`, `F`, `T`, `;`, `,`).

## [0.14.4] - 2026-05-12

### Changed

- Phase 1 of the vim FSM extraction (#62) — new `hjkl-vim` crate at v0.1.0
  holding the `Mode` discriminator (`Normal`, `Insert`, `Visual`, `VisualLine`,
  `VisualBlock`, `OpPending`, `CommandLine`). Pure plumbing — behavior
  identical. `apps/hjkl::keymap::HjklMode` is now a
  `pub use hjkl_vim::Mode as HjklMode` alias so existing imports keep resolving.
  Subsequent phases (#68–#72) will move the FSM itself out of `hjkl-engine` into
  this crate.
- `hjkl-vim` lives in a standalone submodule repo
  ([kryptic-sh/hjkl-vim](https://github.com/kryptic-sh/hjkl-vim)) with the
  canonical `ci.yml` + tag-driven publish pipeline.

## [0.14.3] - 2026-05-12

### Added

- Visual-mode `:` now prefills the command prompt with `'<,'>` and exits visual
  so range-aware ex commands apply to the selection. Pair this with hjkl-engine
  0.5.3's `<` / `>` mark hook for `:'<,'>sort`, `:'<,'>s/old/new/`,
  `:'<,'>w >>file`, `:'<,'>!fmt`, `:'<,'>d`, and any other `:[range]` command on
  `V<motion>:` / `v<motion>:` / `<C-v>:` flows.
- `App::open_command_prompt_with(prefill)` helper.

### Changed

- Local `HjklMode` enum in `apps/hjkl/src/app/keymap.rs` replaces the
  hjkl-keymap concrete `Mode` enum. `Keymap<AppAction>` field types are now
  `Keymap<AppAction, HjklMode>`. Moves to `hjkl-vim` when issue #62 lands.
  Required by hjkl-keymap 0.2.0's generic-Mode refactor.

### Performance

- Markdown preview no longer hitches on the file picker. Two fixes:
  - `App::spans_for_viewport` clips the highlighter's `byte_range` to the
    viewport (with 50-row slack for off-screen injection context). Parent parse
    still runs over full bytes (no partial-parse API for a fresh tree) but
    injection scan + child highlights stay bounded.
  - hjkl-bonsai 0.6.2 caches child highlighters in `Highlighter` keyed by
    `(lang, content_range)` with content-hash drift detection.
- Fast buffer switching latency cut substantially. Three fixes:
  - `SyntaxLayer::preview_render` now reuses a cached `Highlighter` per grammar
    (calling `reset()` between calls) instead of constructing a fresh one via
    `Highlighter::new` every switch. Skips dlopen-related setup and query
    compilation; the bonsai child-cache survives across calls.
  - Dropped the 5 ms `wait_result` in `recompute_and_install`'s viewport-only
    resubmit path. Worker spans arrive on a subsequent tick instead.
  - New `GitSignsWorker` (apps/hjkl/src/git_worker.rs) spawns a background
    thread that runs `git2::Diff` + `is_untracked` off the UI thread. Coalesce
    policy: latest-wins per `buffer_id`. `App::poll_git_signs` drains results
    each tick.

### Dependencies

- `hjkl-keymap` 0.1.1 → 0.2.0 (breaking — concrete `Mode` enum removed, replaced
  with `Mode` trait; `Keymap<A, M>` generic over mode discriminator). Plus
  `Keymap::children_all`, `Keymap::pop`, and the `timeout_resolve` pure-prefix
  fix that all landed in the 0.1.2 / 0.1.3 / 0.1.4 patches.
- `hjkl-picker` 0.5.1 → 0.5.2 (adds `PreviewHighlighter::spans_for_viewport`
  trait method).
- `hjkl-bonsai` 0.6.1 → 0.6.2 (caches child highlighters across injection
  passes).
- `hjkl-engine` 0.5.2 → 0.5.4 (visual-exit hook sets `'<` / `'>` marks with
  mode-aware position semantics: charwise tuple-ordered, linewise snaps to line
  edges, blockwise uses independent min/max corners).

## [0.14.2] - 2026-05-12

### Added

- Which-key Backspace navigation. Backspace inside an active chord pops the last
  key from the buffer, surfacing the parent level in the popup. When the last
  key is popped the popup enters sticky mode showing root-level entries until
  the user types something else. Esc dismisses and clears chord + sticky state.
- New `App::which_key_sticky` field; cleared on Esc and any non-Backspace
  keypress.

### Changed

- Which-key popup content now reads from `Keymap::children_all` (#57). The five
  static descriptor tables (`LEADER_ENTRIES`, `G_ENTRIES`,
  `BRACKET_RIGHT_ENTRIES`, `BRACKET_LEFT_ENTRIES`, `CTRL_W_ENTRIES`) and the
  `Prefix` enum are deleted. Runtime `:nmap` entries auto-surface in the popup
  with zero changes to `which_key.rs`. Sub-prefix collapse (e.g. `<leader>g`
  showing the top-level Leader table) is fixed.
- `App::active_which_key_prefix` returns the raw pending buffer
  (`Vec<KeyEvent>`) instead of collapsing to a `Prefix` enum.
- Header label derived from `Chord(pending).to_notation(leader)`; sticky
  empty-buffer state renders as `"root"`.

### Fixed

- Which-key popup no longer disappears the instant chord-timeout fires on a
  leader prefix. Pure-prefix buffers (e.g. `<leader>` alone with no terminal
  binding at that depth) now keep the buffer alive past the timeout instead of
  being drained. Requires `hjkl-keymap` 0.1.4.

### Refactor

- Retired the five dead `pending_*` prefix fields on `App` (`pending_leader`,
  `pending_git`, `pending_lsp`, `pending_buffer_motion`,
  `pending_window_motion`) — vestigial after #11 + #59 with
  `#[allow(dead_code)]`. Migrated 7+ tests from inlined event-loop logic to real
  dispatch via `drive_key`. Removed 26 vestigial `self.pending_* = false` writes
  across 11 `open_*_picker` methods. Closes #58.
- `Ambiguous` chord state now resolves to the shorter binding when `timeoutlen`
  elapses (#60). Wired `App::resolve_chord_timeout` into the event-loop
  poll-timeout branch; widened poll deadline calc to include the chord-timeout
  alongside the which-key deadline. Three regression tests: shorter-binding
  fires on timeout, longer-binding fires on fast second key, no-pending returns
  None.

### Dependencies

- `hjkl-keymap` 0.1.1 → 0.1.4. Adds `Keymap::children_all` (which-key children
  with prefix-only submenus), `Keymap::pop` (chord backspace navigation), and
  fixes `timeout_resolve` to not drain pure-prefix buffers.

## [0.14.1] - 2026-05-12

### Fixed

- SHIFT-modifier normalization at the crossterm → keymap boundary. Char events
  arriving with `+SHIFT` (kitty, foot, wezterm w/ kitty keyboard protocol) now
  match bindings registered as `ch('B')` (mods=NONE). Affects `<leader>gB`,
  `<leader>gS`, `<C-w>W`, `<C-w>T`, `<C-w>R`, `]D`, `[D`, `K`.
- Engine-pending bypass — keymap trie skips itself while the engine is in any
  multi-key pending state (`r<x>` Replace, `f<x>`/`t<x>` Find, `m<a>` SetMark,
  operator-pending, etc.), so the engine's in-flight commands complete without
  the trie eating their continuation key. Requires `hjkl-engine` 0.5.2.
- Multi-key Unbound replay forwards to engine — `gg` / `gj` / `gk` / `G` / `gE`
  / `ge` now reach the engine through the chord dispatch path (previously
  silently dropped after the trie's `g`-prefix consumed the first key).
- Cycle guard on recursive maps — `:nmap a a` and similar vertical cycles now
  bail with `E223: recursive mapping (depth limit)` instead of overflowing the
  call stack. Per-frame step counter still catches horizontal cycles.
- `<C-w><` (resize-width-narrower) binding now registers — was silently failing
  to parse due to an unescaped trailing `<`. Use `<lt>` escape.

### Changed

- Merged `RuntimeKeymaps` into the `app_keymap` trie via
  `AppAction::Replay { keys, recursive }` (#59). User `:map` / `:nmap` / `:imap`
  etc. commands now register on the same trie as built-ins.
  `user_keymap_records` side-table backs `:map` listing so built-in chords don't
  leak into user listings. Removed: `RuntimeKeymaps`, `apply_runtime_map`,
  `handle_runtime_mapped_key`, `App::runtime_keymaps`.
- Mode-generalized chord dispatch — `dispatch_keymap_in_mode` feeds the active
  vim mode so user maps in Insert / Visual / OpPending / CommandLine work.
  Terminal-mode runtime maps are silently dropped pending a Terminal variant in
  the keymap crate.
- `<C-w>>` binding switched to `<C-w><gt>` for cosmetic symmetry with
  `<C-w><lt>`. Behavior unchanged.

### Architecture

- Filed #62 (extract `hjkl-vim` grammar layer; engine becomes pure controller)
  and #63 (engine multi-selection primitives) as long-running design issues.

### Dependencies

- `hjkl-engine` 0.5.1 → 0.5.2 (adds `Editor::is_chord_pending()`)
- `hjkl-keymap` 0.1.0 → 0.1.1 (adds `<gt>` notation escape)

## [0.14.0] - 2026-05-10

### Added

- **`:Anvil install / uninstall / update / update <name>`** ex commands and the
  `:Anvil` picker for installing language servers, formatters, and linters from
  the bundled `anvil.toml` registry. `<XDG_DATA_HOME>/anvil/bin` is prepended to
  `$PATH` at startup; LSP servers resolve from anvil-installed binaries with no
  system-package install required. Closes #61.
- **`:LspInfo` anvil state** — output now includes per-server install state
  (`installed @ vX.Y.Z` / `not installed` / `not in registry`).
- **`:set` render options** — parser support for `cursorline` / `cul`,
  `cursorcolumn` / `cuc`, `signcolumn` / `scl` (yes / no / auto), `foldcolumn` /
  `fdc` (0–12), `colorcolumn` / `cc` (comma-separated columns). Closes #34.
- **which-key popup on idle** — pressing `<leader>` / `g` / `]` / `[` / `<C-w>`
  and pausing for 500 ms (configurable via `[which_key] delay_ms`) shows
  reachable bindings in a floating window. Closes #53.
- **App-level count prefix** for `[N]gt`, `[N]gT`, `[N]<C-w>+/-/</>`. Closes
  #46.
- **New crates** extracted from the monorepo and auto-published from their own
  repositories:
  - `hjkl-xdg 0.1.0` — XDG Base Directory resolver shared by bonsai, config, and
    anvil. Closes #19.
  - `hjkl-keymap 0.1.0` — chord parser + mode-scoped trie + `KeyResolve` enum,
    used by `apps/hjkl` for Normal-mode chord dispatch. Closes #11.
  - `hjkl-anvil 0.1.1` — Mason-style installer (Github / Cargo / Npm / Pip /
    GoInstall pipelines, atomic install + symlink, SHA-256 verify, async install
    pool with per-key dedupe). Closes #61.

### Fixed

- **HTML highlighting** — bonsai's `(#set! @cap …)` sanitizer now uses
  paren-balanced excision + pre-extraction so the html grammar resolves fully.
  No more plain-text fallback on `.html` files. Closes #56.
- **Windows CI test paths** — `hjkl-xdg` submodule updated to fix path handling
  on `x86_64-pc-windows-msvc`.

### Changed

- Submodule cycle bumps: `hjkl-bonsai 0.6.1`, `hjkl-engine 0.5.1`,
  `hjkl-editor 0.4.4`, `hjkl-buffer 0.6.0`, `hjkl-picker 0.5.1`,
  `hjkl-config 0.2.1`, `hjkl-form 0.3.5`, `hjkl-ratatui 0.3.5`.
- `hjkl-keymap` now backs Normal-mode chord dispatch in `apps/hjkl`; the legacy
  `pending_*` prefix fields remain `#[allow(dead_code)]` pending follow-up #58.
- `hjkl-xdg`, `hjkl-keymap`, and `hjkl-anvil` extracted to standalone submodule
  repos and wired back via `[patch.crates-io]`.

## [0.13.0] - 2026-05-09

### Added

- **Tree-sitter predicate/directive extension layer.** Bumps `hjkl-bonsai` to
  `0.6.0`, which ships a parser-agnostic dispatcher matching helix and
  nvim-treesitter behavior on top of stock tree-sitter. The
  `(#set! @capture key val)` form used by nvim-treesitter html highlights — and
  by other capture-targeted directives — is now honored end-to-end via
  pre-extraction + replay; `HighlightSpan` carries the resulting metadata
  through to consumers. Unknown predicates degrade gracefully (warn-once, match
  still emitted) instead of dropping the highlight. Resolves #56.
- **File-backed tracing output.** `tracing` now writes to
  `$XDG_DATA_HOME/hjkl/logs/hjkl.log` in addition to stderr, so post-mortem
  debugging of grammar / LSP / picker issues no longer requires re-running with
  the terminal captured.
- **Runtime keymaps.** `hjkl-config` keymap definitions are now applied at
  runtime through the app, allowing user-level keymap overrides to take effect
  without recompiling.

### Fixed

- HTML files no longer render plain text. The bonsai sanitizer's line-based
  `(#set! @cap …)` stripper used to consume a closing `)` belonging to the
  enclosing pattern when the directive's `)` shared a line with the outer
  group's `)`, leaving the resolved query unbalanced and tree-sitter erroring on
  a downstream pattern. Bonsai's paren-balanced excision (then full
  pre-extraction in 0.6.0) replaces the workaround. See bonsai#3 / bonsai#4.
- Web rails: normalized sibling rails, removed install gaps, unified install
  styles in the marketing site.
- Cleaned up clippy warnings introduced by the runtime keymap work in
  `apps/hjkl`.

## [0.12.2] - 2026-05-06

### Changed

- Extracted `hjkl-lsp` from in-tree crate to standalone submodule
  (`kryptic-sh/hjkl-lsp`), published to crates.io as `hjkl-lsp 0.1.0`, and wired
  back via `[patch.crates-io]` so `apps/hjkl` resolves it without a path dep —
  fixing `cargo publish` for the umbrella binary.

## [0.12.1] - 2026-05-07

### Changed

- Pinned `mlugg/setup-zig` to zig 0.15.1 to skip `build.zig.zon` lookup and fix
  post-step CI noise.
- Bumped `hjkl-picker` dep to `^0.5` (bonsai-agnostic `PreviewHighlighter` API).

## [0.12.0] - 2026-05-06

### Added

- **`hjkl-lsp` foundation + 5-phase Language Server Protocol integration (#38,
  #47–#51).** New in-tree `crates/hjkl-lsp` crate ships a tower-lsp-based
  client + per-language server lifecycle manager with full text-sync (open /
  change / save / close) wired onto buffer edits.
  - **Phase 2 — Diagnostics + nav (#48):** inline + signcolumn diagnostic
    rendering, severity-aware highlighting, `]d` / `[d` motions, `:LspInfo` ex
    command surfacing per-buffer attach state, server config, and capability
    diagnostics.
  - **Phase 3 — Goto + hover (#49):** `gd` / `gD` / `gi` / `gy` jump to
    definition / declaration / implementation / type-definition, `K` shows
    hover, `gr` / `:lreferences` opens references in the picker. Cursor reveals
    in viewport after every cross-buffer jump (`gd` / `]d` / `[d` / `:lfirst` /
    `:llast`).
  - **Phase 4 — Completion (#50):** triggered + manual completion popup, kind
    icons, snippet expansion, async resolve.
  - **Phase 5 — Code actions, rename, format (#51):** `<leader>ca` /
    `:LspCodeAction`, `<leader>rn` / `:LspRename`, `:LspFormat` /
    `:LspFormatRange`, with workspace-edit application across multiple files.
  - Bundled default server configs for common languages (rust-analyzer / pyright
    / typescript-language-server / clangd / gopls / lua-language-server) —
    `:LspInfo` shows which one matched the active buffer.
  - Status-line spinner while LSP requests are in flight, sharing
    `hjkl_ratatui::spinner` with the grammar-load indicator.
- **Window splits — full 4-phase rollout (#40–#43).**
  - **Phase 1 — `:sp`, `Ctrl-w j` / `Ctrl-w k`, `:close`.** Horizontal splits
    sharing the active buffer, per-window viewport state.
  - **Phase 2 — `:vsp`, `Ctrl-w h` / `l` / `w` / `W`, `Ctrl-h` / `Ctrl-l`.**
    Vertical splits + cross-direction nav.
  - **Phase 3 — Resize + equalize + maximize.** `Ctrl-w +` / `-` / `>` / `<` /
    `=` / `_` / `|`, plus `:resize` and `:vertical resize`.
  - **Phase 4 — `:only`, `Ctrl-w o`, swap, `:new`, `:vnew`, `:q` redirect.**
    `:q` on the last window in a split closes the split, not the editor.
  - **1-cell separator** painted between sibling panes (`│` / `─`, themed via
    `theme.ui.border`) so splits no longer look like a single wall of text.
  - **Per-window cursor + viewport state.** Two splits onto the same buffer
    track their own cursor row/col + scroll offset independently. Syntax-span
    computation now unions all visible viewports for the active buffer so the
    inactive split keeps its highlights when the focused split scrolls.
- **Tabs — Phase 1 + 2 (#44, #45).** `:tabnew`, `gt` / `gT`, `:tabnext` /
  `:tabprev` / `:tabclose`, plus the Phase 2 set: `:tabfirst` / `:tablast` /
  `:tabonly` / `:tabmove` / `:tabs`, `Ctrl-w T` (move current window to its own
  tab).
- **tmux-navigator handoff at split edges.** Bare `Ctrl-h` / `Ctrl-j` / `Ctrl-k`
  / `Ctrl-l` in Normal mode move between hjkl windows; if there is no neighbour
  in the requested direction and `$TMUX` is set, the keystroke falls through to
  `tmux select-pane -L/-D/-U/-R` so users can move from an edge hjkl pane
  straight into the surrounding tmux pane.
- **Mouse wheel scrolls viewport (not cursor) + `editor.mouse` toggle.** Wheel
  events now scroll the viewport with the cursor clamped inside, respecting
  `scrolloff`. New `editor.mouse = true` config field (defaulting on) and
  matching `:set mouse` / `:set nomouse` / `:set mouse!` / `:set mouse?` runtime
  ex-commands (nvim-style); the terminal mouse capture is enabled / disabled
  live without restart.
- **Picker preview is now consumer-agnostic via a `PreviewHighlighter` trait.**
  Consumers (other kryptic-sh apps) can plug in their own highlight pipeline —
  tree-sitter, LSP semantic tokens, regex, none — without `hjkl-bonsai` ever
  appearing in the picker crate. `hjkl-picker` re-exports `PreviewHighlighter`,
  `PlainPreview`, `PreviewTheme`, and a self-contained `preview_pane()`
  renderer.
- **Picker preview now highlights injected sub-languages in markdown** (e.g.
  ` ```rust ` fences inside `.md` previews) by routing through
  `Highlighter::highlight_range_with_injections` with a non-blocking grammar
  resolver: cached child grammars highlight immediately, missing ones queue an
  async load. Regression test
  `preview_spans_for_markdown_includes_rust_injection` guards the wiring.
- **Global grammar-load spinner.** The status-line `loading grammar: <name>…`
  takeover now reflects any queued grammar across the directory (not just the
  active buffer) and collapses concurrent loads to `<first> +N`.
- **`:set number` / `:set relativenumber` line-number gutter.** Aliases `nu` /
  `rnu` / `nonu` / `nornu` / `nu!` / `rnu!`. Combined `nu rnu` enables vim's
  hybrid mode: cursor row shows its absolute number, others show the offset.
  Plus `:set numberwidth=N` / `:set nuw=N` (1..=20, default 4) for minimum
  gutter width.
- **`~` tilde markers** paint at the first text column on every screen row past
  end-of-buffer, matching vim's `NonText` rendering. New `non_text` theme color
  (default `#4a5266`).
- **`+cmd` and `-c CMD` work in TUI mode** (vim/nvim parity). Previously the
  flag was gated on `--headless`; now any unknown `+token` argv is treated as an
  ex command and dispatched after buffers load but before raw mode begins.
  `hjkl +vsp file1 file2` does what you'd expect.
- **`:LspInfo`** surfaces matched server config + active-buffer attach
  diagnostics for fast triage when a server fails to attach.
- **LSP search-count** now compares in byte offsets so multibyte characters no
  longer poison the `[N/M]` indicator.

### Changed

- **All grammar compiles run on the editor pipeline.** `hjkl_bonsai`'s
  background loader pool is the single source for clone-and-compile work; the
  picker, status spinner, and active-buffer set-language path all call into the
  same async loader. Concurrent loads dedupe per language at the `hjkl-bonsai`
  source-cache layer (`Arc<Mutex<()>>` per key) so two workers no longer race on
  the shared Helix `QuerySourceCache` staging dir.
- **CI collapsed into a single `ci.yml`.** The earlier 3-stage workflow (lint →
  test → release) is replaced by one workflow gated on `tags: ['v*']`, matching
  the org-wide canonical CI pattern.
- **`hjkl-buffer` 0.4 → 0.5**, **`hjkl-editor` 0.4.1 → 0.4.1+3**,
  **`hjkl-engine` 0.3.8 → 0.3.8+3**, **`hjkl-bonsai` 0.5.0 → 0.5.3+2**,
  **`hjkl-picker` 0.4.0 → 0.4.0+5** (substring-fast-path scoring,
  `PreviewHighlighter` trait, `preview_pane`).

### Fixed

- **`/<pat><CR>` no longer double-steps past the cursor's match.** When the
  cursor was already on a match, the search post-step would jump to the next one
  instead of staying put. Forward-direction is now persisted on `+/pat` startup
  search too.
- **Viewport reveals search matches.** `/<CR>` and `+/pat` startup search now
  scroll the viewport so the matched cursor is visible, respecting `scrolloff`.
  `viewport_height` is published in `build_slot` so engine-side reveal logic has
  the height it needs.
- **Engine-handled `g` / `]` / `[` motions** sync the host viewport on the same
  tick instead of one frame later.
- **`:w` on a path whose parent directory does not exist** now does `mkdir -p`
  of the parent and writes through, instead of returning ENOENT.
- **LSP buffer paths absolutized before URI conversion.** Relative paths (e.g.
  opening hjkl with `hjkl src/main.rs`) were sent to servers as relative URIs;
  now `std::path::absolute` is applied first. Regression test added.
- **LSP attach picks up existing slots in `with_lsp`.** Buffers loaded before
  the LSP subsystem registered now attach when it comes up, with clearer status
  messages on success / skip / fail.
- **Goto picker strips `cwd` from location paths** so `references` /
  `definitions` show short relative paths instead of `/full/abs/path/file.rs`.
- **`textDocument/references` requests** now include the required `context`
  object (`includeDeclaration`); some servers rejected the request without it.
- **tmux-navigator binds fall through cleanly when there is no neighbour** (vs.
  eating the keystroke silently).
- **Cross-platform LSP tests.** `goto_definition_multi_opens_picker` and
  `goto_references_always_opens_picker` previously hardcoded `file:///tmp/a.rs`
  literals that broke on Windows runners (no drive letter → `to_path` returned
  None); switched to existing `tmp_path` + `file_url` helpers. URI roundtrip
  tests now use `std::path::absolute` so relative-path inputs survive on
  Windows.

## [0.11.5] - 2026-05-06

### Added

- **`hjkl-splash` crate, extracted to a standalone repo + crates.io publish.**
  The HJKL splash-screen animation moved out of `apps/hjkl/src/start_screen.rs`
  into a rendering-agnostic crate at
  [kryptic-sh/hjkl-splash](https://github.com/kryptic-sh/hjkl-splash) so other
  kryptic-sh projects can reuse the same cursor-trail-on-letterforms animation
  in TUI or GUI frontends. Core API emits pure `SplashCell` items via an
  iterator; an optional `ratatui` feature ships a
  `From<Rgb> for ratatui::style::Color` adapter. The hjkl letterforms + path are
  bundled as `presets::hjkl`. Wired in via the existing submodule pattern
  - `[patch.crates-io]`.

### Changed

- **Splash now wall-clock driven (via hjkl-splash 0.2.0).** Animation tick is
  derived from `Instant::now()` inside the crate; consumer code no longer calls
  `screen.advance()`, so the animation cannot stall when high-frequency events
  (mouse motion, focus, resize) starve the event-loop timeout branch.
  `apps/hjkl/src/start_screen.rs` is now a thin ratatui adapter; the
  per-iteration `advance()` call in `apps/hjkl/src/app/event_loop.rs` is
  removed.
- **Compat-oracle: graduate substitute cases to a dedicated nvim-api tier
  (closes #26).** New `corpus/nvim_api_tier.toml` holds the four substitute
  cases that previously sat in `known_divergences.toml`; a new
  `nvim_api_tier_passes` test asserts them via the `hjkl --nvim-api` subprocess
  driver on every CI run (no `HJKL_ORACLE_NVIM_API` gate). The old
  `substitute_via_nvim_api` test is removed; `known_divergences.toml` now tracks
  no active divergences. Driver gains cross-platform binary discovery
  (`std::env::consts::EXE_SUFFIX`) and drops the redundant `echo 1` sync barrier
  — hjkl's `nvim_input` / `nvim_command` handlers process synchronously, so the
  awaited response already implies a settled state.

## [0.11.4] - 2026-05-05

### Added

- **Grammar-load status indicator (closes hjkl#17 acceptance gap).** The status
  line now shows `loading grammar: <name>…` while an async grammar load is in
  flight for the active buffer, and `grammar load failed: <name> — <error>` for
  5 seconds when a load fails. Both use the same takeover pattern as the
  `recording @r` indicator.
- **`:qall` / `:qall!` / `:wqall` / `:wqall!` ex commands (closes #27).**
  hjkl-editor 0.4.0 → 0.4.1 adds dispatch arms for the qall family that the
  canonical-name table already advertised. Reverts the `:q!` workaround in
  nvim-api tests + compat-oracle introduced in v0.11.3.

## [0.11.3] - 2026-05-05

### Added

- **`--nvim-api` msgpack-rpc surface (phase 3 of #26).** New flag boots a
  msgpack-rpc server speaking the neovim wire protocol. Existing `nvim-rs`
  clients can target hjkl as a drop-in subprocess replacement for
  `nvim --headless --embed`. Implemented methods: `nvim_get_current_buf`,
  `nvim_get_current_win`, `nvim_buf_set_lines`, `nvim_buf_get_lines`,
  `nvim_win_set_cursor`, `nvim_win_get_cursor`, `nvim_input`, `nvim_command`,
  `nvim_get_mode`, `nvim_call_function` (`getreg` only). Compat-oracle gains an
  opt-in `HJKL_ORACLE_NVIM_API=1` mode that drives hjkl via msgpack-rpc; the
  four substitute cases pass on this path (still in `known_divergences` for the
  in-process driver since the vim FSM cannot dispatch `:` from a key-replay).
- **Non-blocking grammar loads (hjkl#17 follow-up).** `set_language_for_path` no
  longer blocks the UI thread on first-edit clone+compile. New `GrammarRequest`
  / `SetLanguageOutcome` enums; pending loads tracked on `SyntaxLayer` and
  drained each tick via `poll_pending_loads`. Cache / on-disk fast paths
  preserved — only true clone+compile cases now defer.

### Changed

- **hjkl-bonsai 0.5.0 → 0.5.3.** Adds `AsyncGrammarLoader` (2-worker pool with
  in-flight dedup) and `Grammar::load_from_path` for skipping the loader chain
  when the `.so` + queries are already on disk. Sync `GrammarLoader::load`
  unchanged.
- **hjkl-clipboard 0.5.1 → 0.5.3.** Wayland data_source `send` callback no
  longer blocks the UI thread when the paste receiver doesn't drain; adds
  O_NONBLOCK + POLLOUT deferred drain with a 5s deadline reaper. Self-paste
  short-circuits in `do_get` when we own the source. Fixes
  kryptic-sh/hjkl-clipboard#4 (downstream kryptic-sh/buffr#34).

### Fixed

- `--embed` JSON-RPC + `--nvim-api` msgpack-rpc tests sent `:qa!` for shutdown
  but ex.rs canonicalizes `qa!` to `qall!` and has no handler arm — server
  returned an error, never quit, tests hung 5+ minutes per case. Switched to
  `:q!`. Proper `qall` family handler tracked in #27.

## [0.11.2] - 2026-05-05

### Added

- **`recording @r` status indicator.** The status line now shows `recording @r`
  while a q-macro is being recorded, matching vim's native indicator.
- **`--headless` / `-c CMD` script runner (phase 1 of #26).** Launch hjkl with
  `--headless +cmd` or `-c CMD` to execute ex commands non-interactively and
  exit. Enables scripted batch processing without a terminal.
- **JSON-RPC 2.0 server over stdin/stdout (phase 2 of #26).** `--embed` flag
  starts an embedded RPC server, allowing external tools (LSP clients, editors,
  scripts) to drive hjkl over a structured protocol.

## [0.11.1] - 2026-05-05

### Added

- **`:s` / `:%s` / `:<n>,<m>s` / `:'<,'>s` substitute ex-command.** Vim-style
  pattern + replacement with `g` (all on line), `i` (case-insensitive), `I`
  (force case-sensitive) flags. `c` flag is parsed and silently ignored (no
  confirm UI in v1). `&` and `\1`..`\9` work in the replacement. Empty pattern
  (`:s//rep/`) reuses the last `/` search. Status line shows
  `<N> substitutions on <M> lines`. v1 limitations: `/` is the only delimiter,
  no `\v` very-magic (regex syntax is Rust's `regex` crate). Powered by new
  `hjkl_engine::substitute` module.

### Fixed

Eight vim-compat divergences caught by the cron compat-oracle (#24 closed):

- `x` / `X` now write deleted chars to the unnamed register `"`, so `xp` swap
  and other delete-then-paste idioms round-trip.
- `G` clamps to the last content row instead of landing on a phantom row past
  the trailing newline.
- `dd` on the last line clamps the cursor to the new last line instead of
  leaving it past EOF.
- `d$` cursor lands on the last char of the new line, not one column past.
- `u` after an insert clamps the cursor to the last valid column in Normal mode.
- `da"` eats trailing whitespace (or leading if no trailing) per
  `:help text-objects`, instead of leaving a double space.
- `daB` cursor placement matches vim on multi-line brace blocks.
- `diB` preserves the surrounding newlines on multi-line brace blocks
  (`{\n body \n}` → `{\n}`, not `{}`).

### Changed

- `hjkl-engine` 0.3.6 → 0.3.8 (substitute parser/applier in 0.3.7; divergence
  fixes in 0.3.8).
- `hjkl-editor` 0.3.3 → 0.4.0 (`ExEffect::Substituted` carries `lines_changed`
  for the vim-accurate status message).

## [0.11.0] - 2026-05-05

### Added

- **Markdown fenced code blocks render with sub-language highlighting.**
  ` ```rust ` / ` ```python ` / etc. inside `.md` buffers now show the inner
  code with the target language's tree-sitter highlights instead of plain
  `markup.raw.block`. Powered by `hjkl-bonsai` 0.5's new
  `Highlighter::highlight_with_injections` / `highlight_range_with_injections`
  methods plus a per-process child grammar cache in `LanguageDirectory`. First
  fence of an unseen language pays a one-time clone+compile (worker thread, off
  the input path); subsequent renders are a cache hit.
- **Homebrew tap auto-publish** for `hjkl` on tag push. New
  `pkg/homebrew/hjkl.rb.in` template + `brew-tap` job in `release.yml` renders
  the formula with the just-uploaded macOS sha256s and pushes it to
  `kryptic-sh/homebrew-tap`. Install with `brew install kryptic-sh/tap/hjkl`.
- **`hjkl-compat-oracle` crate** (workspace-only, `publish = false`) — headless
  neovim diff harness for vim-compat regression testing. Spawns
  `nvim --headless --embed` per case, drives both nvim and the hjkl engine
  through identical key inputs, diffs buffer/cursor/mode/registers. Tier 1
  corpus covers 44 motion/operator/text-object/count/insert/undo/register cases
  in `corpus/tier1.toml`. 8 confirmed engine divergences surfaced and tracked
  separately in `corpus/known_divergences.toml` + issue #24. Wired into cron CI
  (`.github/workflows/cron.yml`). Closes #23.
- AUR + Alpine `.apk` install paths added to README and the marketing site.

### Fixed

- `hjkl-engine` 0.3.6: `pos_at_byte` no longer panics on byte indices that land
  inside a multi-byte UTF-8 codepoint. Caught by the cargo-fuzz `handle_key`
  target on a Cyrillic-seeded input after the fuzz workspace + patch-deps
  plumbing in `crates/hjkl-engine/fuzz` was repaired.

### Changed

- `hjkl-bonsai` 0.4.1 → 0.5.0 (new injection methods).
- `hjkl-engine` 0.3.4 → 0.3.6 (added `decode_macro` re-export at crate root in
  0.3.5; UTF-8 fix in 0.3.6).
- Marketing site refreshed for v0.10.x: nine ecosystem crates including
  `hjkl-config`, `<leader>g` git surface tagline, `.apk` in install line.

## [0.10.1] - 2026-05-05

### Docs

- Bump `hjkl-buffer` 0.3.4 → 0.3.5. Inlines former `IMPLEMENTERS.md` invariants
  into rustdoc on the actual types and methods (`Position`, `Edit` + variants,
  `Fold`, `Viewport`, `Span`, `Buffer::set_cursor` / `clamp_position` /
  `ensure_cursor_visible`, `BufferView` render module). Now renders on docs.rs
  next to each symbol and shows up in IDE hover. No binary behavior change.

## [0.10.0] - 2026-05-05

### Added

- **Git pickers under `<leader>g`.** Lazygit-adjacent surface, all bound to the
  `<leader>g` chord:
  - `<leader>gs` — status picker. Modified / staged / untracked entries; preview
    shows the working-tree diff (body + `+`/`-`/space prefix).
  - `<leader>gl` — log picker. Hash dimmed yellow, conventional-commit prefix
    colored by type, lazygit-style author initials with deterministic per-author
    color, preserves chronological sort on empty query.
  - `<leader>gb` — branches picker. Locals bucketed before namespaced and
    remotes; checkout does a pre-flight conflict check (diff HEAD↔target ∩
    workdir status) and aborts with a path preview instead of letting libgit2
    return an opaque `class=Checkout (20); code=Conflict (-13)`.
  - `<leader>gB` — file history picker for the current buffer's path.
  - `<leader>gS` — stash picker. `Alt+P` pops, `Alt+D` drops, Enter applies.
  - `<leader>gt` — tags picker. Sorted by tagger time desc with alpha tiebreak;
    Enter checks out the tag's commit (detached HEAD).
  - `<leader>gr` — remotes picker. Lists configured remotes with branch counts;
    Enter fetches.
- **Auto-reload buffers from disk on focus regain.** `Event::FocusGained`
  triggers `checktime_all()`; non-dirty buffers whose mtime+len changed are
  reloaded silently. Dirty buffers and deleted-on-disk files are flagged with
  vim-style `[changed on disk]` / `[deleted]` suffixes in the status line.
  `:checktime` ex command available for manual sweep. `:write!` overrides the
  `E13: file has changed on disk` guard.
- **Colored git commit header in preview.** `commit` / `Author` / `Date` /
  subject lines styled distinctly; conventional-commit prefix in the subject
  picks up the same color used by the log picker.
- **Drop pristine default buffer when first real file opens.** Empty unnamed
  unmodified default slot is closed automatically once the first `:edit`
  succeeds, so the buffer list stays clean.
- Animated splash background now inherits the terminal background across themes
  (carry-over polish from 0.9.3).

### Changed

- **`hjkl-picker` 0.3 → 0.4.** Picks up the
  `PickerAction::Custom(Box<dyn Any + Send>)` refactor that drops app-specific
  variants (`OpenPath`, `ShowCommit`, `CheckoutBranch`, etc.) from the library,
  plus `handle_key` / `label_styles` / `preserve_source_order` source hooks.
  App-side `AppAction` enum now carries all hjkl-specific intents and is
  downcast in `dispatch_picker_action`.
- **Git status picker moved app-side.** `GitStatusSource` removed from
  `hjkl-picker` to keep the library free of `git2`. Now lives in
  `apps/hjkl/src/picker_git.rs` alongside the other git pickers.
- **`hjkl-bonsai` 0.4.0 → 0.4.1.** Adds `build/` to the crate's `.gitignore` so
  compiled grammar artifacts no longer pollute `git status`.
- Sub-dep patch bumps (no behavior change in this app, picked up via caret):
  `hjkl-buffer` 0.3.4, `hjkl-clipboard` 0.5.1, `hjkl-editor` 0.3.3,
  `hjkl-engine` 0.3.4, `hjkl-form` 0.3.3, `hjkl-ratatui` 0.3.3.

### Fixed

- Branch + log pickers preserve their source-defined sort (locals-first,
  chronological) on empty query instead of falling back to alphabetical.
- Git status preview rendered headers only — now includes the diff body and
  `+`/`-`/space prefix per hunk line.
- Picker fuzzy-match highlight positions are aligned to the visible label
  (post-prefix/icon) rather than the raw entry text.
- `checktime_all()` now runs after `<leader>gb` branch checkout so reloaded
  buffers reflect the new tree without manual `:checktime`.

## [0.9.3] - 2026-05-04

### Added

- Animated start screen on `hjkl` launched without a file argument: centered
  `HJKL` figlet with a cursor walking the letterforms in vim-motion order (h → j
  → k → l), trailing fading `h`/`j`/`k`/`l` glyphs. Any non-Ctrl-C keypress
  dismisses; the dismissing key falls through to normal handling so `:` opens
  the command bar on the same press. Splash inherits the terminal background to
  match the editor body across themes.

### Changed

- Bump `hjkl-bonsai` dep from `"0.3"` to `"0.4"`. Picks up the breaking schema
  refactor where highlight queries are sourced from helix + nvim-treesitter (the
  curated upstreams) rather than each grammar repo's own `queries/` dir.
  Resolves silent partial-install failures for grammars whose upstream layout
  doesn't match the prior hardcoded `query_dir` (xml/dtd were affected at the
  pinned revs).

### Fixed

- Cron CI: `cargo install cargo-fuzz` no longer passes `--locked`. The
  cargo-fuzz published `Cargo.lock` pinned an old `rustix` that uses internal
  `rustc_layout_scalar_valid_range_*` attributes nightly now rejects, breaking
  the fuzz harness install.

## [0.9.2] - 2026-05-03

### Fixed

- `Cargo.lock` updated to match the bumped `hjkl` package version. 0.9.1 shipped
  with the lockfile still pinned to `hjkl 0.9.0`, so
  `cargo build --release --locked --bin hjkl` (the release.yml command) failed
  with "cannot update the lock file because --locked was passed".

## [0.9.1] - 2026-05-03

### Fixed

- `Cargo.lock` regenerated cleanly from `cargo build` instead of partial
  `cargo generate-lockfile`. 0.9.0 release CI failed on
  `cargo zigbuild --locked` because the lockfile was out of sync with the bonsai
  0.3 + apps/hjkl pin bump. No source changes vs 0.9.0.

## [0.9.0] - 2026-05-03

### Changed

- **`hjkl-bonsai` 0.2 → 0.3.** Tree-sitter grammar storage subdir renamed
  `hjkl/grammars/` → `bonsai/grammars/`, and macOS/Windows now follow
  XDG-everywhere instead of `~/Library/Application Support` / `%APPDATA%`.
  Existing grammars under the old paths are not migrated — hjkl re-fetches and
  re-compiles them into `~/.local/share/bonsai/grammars/` on first use. Distro
  packagers shipping pre-built grammars must move from
  `/usr/share/hjkl/grammars/` to `/usr/share/bonsai/grammars/` (the AUR PKGBUILD
  here doesn't ship grammars, so no PKGBUILD change). See
  `crates/hjkl-bonsai/CHANGELOG.md` for full detail.

## [0.8.1] - 2026-05-03

### Added

- **Alpine `.apk` package** in `pkg/alpine/APKBUILD.in`, built in
  `.github/workflows/release.yml` inside an `alpine:latest` container off the
  `x86_64-unknown-linux-musl` release tarball and uploaded to the GitHub release
  alongside the `.deb` / `.rpm` / `.tar.gz` artifacts. Install with
  `apk add --allow-untrusted ./hjkl-*.apk`. Tracks
  [#18](https://github.com/kryptic-sh/hjkl/issues/18).

## [0.8.0] - 2026-05-03

### Added

- **New `hjkl-config` crate** in the workspace (also published as a standalone
  submodule at
  [kryptic-sh/hjkl-config](https://github.com/kryptic-sh/hjkl-config)): shared
  TOML config loader for hjkl-based apps. XDG path resolution, span-aware parse
  errors (line/col/snippet), opt-in `Validate` hook, plus
  `load_layered`/`load_layered_from` for bundled-defaults + user-overrides
  deep-merge. Reusable bounds-check helpers (`ensure_range`, `ensure_non_zero`,
  `ensure_one_of`, `ensure_non_empty_str`) returning `ValidationError` with
  field names baked in.
- **User config support in the `hjkl` editor.** Reads
  `$XDG_CONFIG_HOME/hjkl/config.toml` (or `--config <PATH>` to override).
  Defaults bundled into the binary via `include_str!()` from
  [`apps/hjkl/src/config.toml`](apps/hjkl/src/config.toml) — the source-tree
  file is the single source of truth for defaults; no default values live in
  Rust code. User file is deep-merged on top: only overridden fields need to
  appear there. Unknown keys are an error.
  - Wired settings: `editor.leader` (replaces hardcoded `Space`),
    `editor.tab_width` / `editor.expandtab` (fallback when no `.editorconfig`
    matches), `editor.huge_file_threshold` (replaces `HUGE_FILE_LINES = 50_000`
    const in syntax_glue), `theme.name` (currently only `"dark"` bundled;
    unknown names warn and fall back).
  - `Config::validate()` bounds-checks `tab_width ∈ 1..=16` and
    `huge_file_threshold > 0`. Surfaced via `hjkl: config validation: …` on
    startup; exits with code 2 on failure.
  - Slot 0 gets the user-config Options reapplied via `App::with_config` so
    overrides take effect on the first opened buffer (not just `:e`-opened new
    slots). Readonly state on existing slots is preserved across the swap.
  - 5 new validation tests, 2 new `--config` end-to-end pipeline tests, 2 new
    `with_config` smoke tests covering slot-0 and readonly-preservation.

### Changed

- **`hjkl` CLI migrated from hand-rolled parser to clap derive.** Behavior
  preserved: `-R` / `--readonly`, positional `[FILES]...`, vim-style `+N`,
  `+/PATTERN`, `+perf`, `+picker` all still work. `+`-prefixed tokens are
  pre-processed out of `argv` before clap sees it (clap doesn't natively parse
  `+` flags). `--help` / `--version` are now generated by clap and honor
  `CARGO_PKG_VERSION`.

### Added

- `hjkl --help` now renders an ASCII-art banner (figlet "ANSI Regular" font)
  plus the package version inline. Banner lives in `apps/hjkl/src/art.txt`,
  embedded via `include_str!`. Regenerate with
  `figlet -f "ANSI Regular" hjkl > apps/hjkl/src/art.txt`.
- CLI smoke tests: `--version` returns `CARGO_PKG_VERSION`, long-form help
  contains the embedded art block and the version, vim-token splitter separates
  `+N`/`+/foo` from clap-handled args, vim-token applier sets
  line/pattern/perf/picker correctly.
- Edge-case tests for vim-style tokens: bare `+` survives into the clap stream
  as a positional, `--` ends vim-token processing (`hjkl -- +42` opens a file
  literally named `+42`), repeated `+N` / `+/PAT` overwrite (last-write-wins),
  unknown `+cmd` produces a warning string, `+/` (empty pattern) currently sets
  `pattern = Some("")` (documented quirk), end-to-end `parse_argv` round-trips
  mixed flags + tokens + warnings.

### Changed (internal)

- `parse_args` extracted into pure
  `parse_argv(raw: Vec<String>) -> Result<(Args, Vec<String>)>` for testability;
  the env+stderr wrapper remains as `parse_args` for `main` to call.
  `apply_vim_tokens` now returns warnings instead of printing to stderr.

## [0.7.0] - 2026-05-03

Adopts `hjkl-clipboard` 0.5.0 — the `Backend` trait went public, with new
`BackendKind` / `Capabilities` introspection plus async variants and the
`MockBackend` / `SshAwareBackend` extensions. Umbrella consumes the new API in
two places.

### Added

- New `:clipboard` ex command — prints the active backend kind plus the active
  capability flags
  (`WRITE READ CLEAR AVAILABLE PRIMARY IMAGE RICH_TEXT URI_LIST ASYNC_WRITE …`)
  to the status line. Useful for diagnosing why a yank/paste failed silently
  (e.g. confirming the OSC 52 fallback is active over SSH).
- `TuiHost::clipboard()` accessor exposing the cached `Clipboard` so the
  ex-dispatch layer can introspect without round-tripping the engine.

### Changed

- **`TuiHost::read_clipboard` is capability-aware.** Returns `None` immediately
  when the active backend doesn't advertise `Capabilities::READ` (OSC 52 over
  SSH, mocks without `preset_get`, etc), avoiding a guaranteed `UnsupportedMime`
  round-trip through the Wayland/X11 thread.
- `TuiHost::write_clipboard` checks `Capabilities::WRITE` before attempting the
  set, so a misconfigured mock backend that advertises no write capability
  silently no-ops instead of recording garbage calls.
- `hjkl-clipboard` dep `0.4` → `0.5` (caret-minor — `0.4` does not accept
  `0.5.x`).

## [0.6.0] - 2026-05-03

Migrates the umbrella binary onto `hjkl-bonsai` 0.2.x's runtime grammar loader.
Grammars are no longer baked into the binary; they're cloned, compiled, and
installed on demand the first time the editor encounters a language. Distros
that pre-populate `/usr/share/hjkl/grammars/` skip the on-demand path entirely.

### Changed

- **Release binary shrinks 31 MB → 5.1 MB.** The 27 baked `tree-sitter-*`
  grammar crates that bonsai 0.1.x bundled are gone.
- New `apps/hjkl/src/lang.rs` `LanguageDirectory` facade wraps
  `bonsai::runtime::{GrammarRegistry, GrammarLoader}` and caches loaded
  `Arc<Grammar>` per-name. `App` owns one `Arc<LanguageDirectory>`;
  `SyntaxLayer` and the three `Highlighted*Source` pickers all share it so each
  language `dlopen`s once per process.
- `SyntaxWorker` IPC now ships `Arc<Grammar>` (Send+Sync via tree-sitter's
  `unsafe impl`s + `libloading::Library`'s thread-safety) in place of
  `&'static LanguageConfig`.
- First-ever edit of an unknown file extension now blocks for ~1–3 s on a
  `git clone` + `cc` compile. Subsequent edits of the same language hit the
  user-data install (`<user_data>/hjkl/grammars/`) instantly. System-shipped
  grammars skip the build entirely.
- On-disk layout reorganized (see hjkl-bonsai 0.2.0 changelog for full detail).
  Existing `~/.local/share/hjkl/grammars/sources/` and
  `~/.cache/hjkl/build-grammars/` from v0.5.0 are now orphan and safe to delete.

### Fixed

- Cross-platform user-directory resolution: Windows / macOS no longer bail with
  `$HOME not set` because the loader now uses the `dirs` crate.

### Internal

- Tests that need a real grammar (network clone + cc compile of
  tree-sitter-rust) are gated behind `#[ignore]` so the default `cargo test`
  lane stays offline. Run them with `cargo test -p hjkl -- --ignored`.

## [0.5.0] - 2026-05-03

### Added

- TOML-driven UI + syntax theme matching the `hjkl.kryptic.sh` palette. Themes
  load from baked `themes/{ui,syntax}-dark.toml` at startup;
  `:set background={dark,light}` swaps live.
- Migrated from the legacy `hjkl-tree-sitter` crate to the renamed `hjkl-bonsai`
  0.1.x (same baked-grammar API, just rebadged). No code changes for end users.

### Fixed

- `:wq` (and `:x`) refuse to exit when the save fails. `do_save` / `save_slot`
  now return `bool`; `E32` (no filename), `E45` (readonly), and IO errors no
  longer silently quit and lose unsaved content.

## [0.4.6] - 2026-05-03

### Fixed

- v0.4.5 release failed (all 7 builds): the submodule pointer for
  `hjkl-clipboard` was stale (commit `6170ad0`, pre-0.4.8) but the lockfile
  recorded `hjkl-clipboard 0.4.8`, so `cargo build --locked` in CI rejected the
  mismatch. Pointer advanced to the v0.4.8 tag.

## [0.4.5] - 2026-05-03

### Fixed

- Bumped `hjkl-clipboard` 0.4 → 0.4.8 to pull in the Wayland bind fix. Clipboard
  now works on sway/wlroots and Hyprland (`FIRST_CLIENT_ID = 4` matches
  libwayland-client; older value of 100 was rejected by those compositors with a
  cryptic `"invalid arguments for wl_registry#2.bind"`).

## [0.4.4] - 2026-05-03

### Fixed

- `aur-bin` release job: include `pkg/aur/PKGBUILD-bin.in` in the repo. The AUR
  `.gitignore` allowlist matched only the literal name `PKGBUILD`, silently
  filtering the template file out of the v0.4.3 tag. Job failed at the sed
  render step.

## [0.4.3] - 2026-05-03

### Added

- Auto-publish `hjkl-bin` to AUR on every release. Mirrors buffr's pattern:
  archlinux container fetches sha256 sidecars from the GitHub release tarballs,
  renders `pkg/aur/PKGBUILD-bin.in`, generates `.SRCINFO`, and pushes to
  `aur.archlinux.org/hjkl-bin.git` via the org-level `AUR_SSH_KEY` secret.
  Targets gnu x86_64 + aarch64.

## [0.4.2] - 2026-05-03

### Added

- 8 new tree-sitter languages bundled via `hjkl-tree-sitter` 0.4.0: Python,
  TypeScript, TSX, Go, YAML, Bash, C, HTML, CSS. Auto-detected by file
  extension; highlighting works out of the box for all 14 supported languages.

### Changed

- Bumped `hjkl-tree-sitter` 0.3 → 0.4. Binary size grows ~8–12 MB
  release-stripped from the additional grammar `.so` artifacts.

## [0.4.1] - 2026-05-03

### Changed

- Doc cleanup pass across all submodule READMEs: dropped "spec frozen", "Buffer
  trait sealed", "engine SPEC types" and other stale rhetoric inherited from the
  pre-0.1.0 era.
- `apps/hjkl/src/host.rs` doc comment now describes the clipboard as in-house
  cross-platform rather than "native per-platform".
- `CONTRIBUTING.md` Releases section rewritten — `release-plz` and lockstep
  workspace versioning have been gone for a while; documents the current manual
  BCTP-per-submodule flow.
- Submodule pointer + lockfile bumps: `hjkl-buffer` 0.3.3, `hjkl-engine` 0.3.3,
  `hjkl-clipboard` 0.4.7, `hjkl-tree-sitter` 0.3.2, `hjkl-form` 0.3.2,
  `hjkl-ratatui` 0.3.2, `hjkl-picker` 0.3.2 (all README-only patch releases).

## [0.4.0] - 2026-05-03

### Changed

- **Bumped `hjkl-clipboard` 0.3 → 0.4.** Our in-house clipboard crate replaced
  its `arboard` dependency with a hand-rolled implementation built for the
  kryptic-sh ecosystem. Wayland paste now works on KDE / wlroots / Hyprland
  sessions where the previous backend silently lost the selection. See the
  [`hjkl-clipboard`](https://crates.io/crates/hjkl-clipboard) changelog for the
  full breakdown.
- **`TuiHost` clipboard adapter migrated to the 0.4 API.** `Clipboard::new()` is
  now fallible — wrapped in `Option<Clipboard>` so probe failure degrades to a
  no-op rather than aborting startup. `set_text` / `get_text` replaced with
  typed `set` / `get` over `Selection::Clipboard` + `MimeType::Text`.

### Removed

- **`SPEC.md`** removed from the `hjkl-engine` repo. The trait surface is
  documented inline via rustdoc and published to docs.rs; the parallel
  hand-maintained spec was a churn liability. Umbrella + per-crate READMEs point
  at [docs.rs/hjkl-engine](https://docs.rs/hjkl-engine) instead.

### Internal

- Lockfile shrunk ~371 lines as the `arboard` transitive tree (`wayland-client`,
  `x11rb`, `image`, etc.) was dropped. Smaller `cargo install hjkl` build.
- Bumped `hjkl-engine`, `hjkl-buffer`, `hjkl-editor` 0.3.1 → 0.3.2 (doc-only:
  SPEC.md drop + reference cleanup; no API changes).

## [0.3.4] - 2026-04-30

### Added

- `.rpm` packages for Fedora, RHEL, openSUSE, and other RPM-based distributions
  on x86_64 and aarch64. Built via `cargo-generate-rpm` on the same linux-gnu
  pipeline as the existing `.deb` packages. Install with
  `dnf install ./hjkl-0.3.4-1.x86_64.rpm` (or `aarch64`).

## [0.3.3] - 2026-04-30

### Fixed

- Release CI publish-crates job: only publishes the umbrella `hjkl` app crate.
  Previous logic looped over the eight `hjkl-*` library crates at the workspace
  version, but those ship from their own `kryptic-sh/hjkl-*` repos at
  independent versions, so the loop failed for any version mismatch. The v0.3.2
  GitHub Release was cut with artifacts but never reached crates.io because of
  this; v0.3.3 ships the fix and the matching crates.io upload.

## [0.3.2] - 2026-04-30 [YANKED]

GitHub Release exists with the new artifacts listed below, but
`cargo install hjkl` was never bumped past 0.3.1 — the publish-crates job in
release.yml had a stale loop over now-independent submodule crates and failed
before reaching the umbrella `hjkl` crate. v0.3.3 ships the fix and the matching
crates.io upload.

### Added

- Release matrix expanded from 4 → 7 targets. New artifacts:
  `aarch64-unknown-linux-gnu` (Graviton, Pi, ARM laptops),
  `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` (statically
  linked, distro-agnostic, Alpine, Docker scratch images).
- `.deb` packages on both linux-gnu targets via `cargo-deb` —
  `hjkl_0.3.3-1_amd64.deb` and `hjkl_0.3.3-1_arm64.deb` attached to each GitHub
  Release alongside `.sha256` checksums.
- `[package.metadata.binstall]` so `cargo binstall hjkl` Just Works.
- Homebrew tap at
  [`kryptic-sh/homebrew-tap`](https://github.com/kryptic-sh/homebrew-tap):
  `brew install kryptic-sh/tap/hjkl` (manual bump per release).

## [0.3.1] - 2026-04-30

### Changed

- Extracted each `crates/hjkl-*` library into its own `kryptic-sh/hjkl-*`
  repository with full git history preserved. Each library now publishes
  independently to crates.io and is consumed via caret version requirements
  (`hjkl-buffer = "0.3"`, etc.) instead of workspace path deps.
- The `kryptic-sh/hjkl` repo now mounts those crates back under `crates/*` as
  git submodules pinned to `v0.3.1` tags, so a single workspace build still
  touches every layer for development.
- Bumped sibling deps to their `0.3.1` releases: `crossterm` 0.29, `ratatui`
  0.30, `criterion` 0.8, `toml` 1.1.

## [0.3.0] - 2026-04-28

### Added

- **Multi-buffer support** in `apps/hjkl`: open many files at once
  (`hjkl a.rs b.rs c.rs`); tab line at top of screen when more than one buffer
  is open; switch buffers with `:bn` / `:bp` / `:bd[!]` / `:bfirst` / `:blast` /
  `:b N` / `:b name` / `:ls` / `:buffers`; alt buffer via `Ctrl-^` or `:b#`;
  cycle with `Shift-H` / `Shift-L` and `gt` / `gT` / `]b` / `[b`; bulk save/quit
  with `:wa` / `:qa[!]` / `:wqa[!]`; helix-style `:q` closes the active slot
  when more than one buffer is open rather than exiting.
- **Fuzzy file picker** (`<Space><Space>` / `<Space>f` / `:picker` /
  `hjkl +picker`) with syntax-highlighted preview pane.
- **Buffer picker** (`<Space>b` / `:bpicker`) — switch open buffers via the same
  fuzzy UI.
- **Grep picker** (`<Space>/` / `:rg <pattern>`) — ripgrep-backed content search
  with grep and findstr fallbacks for platforms without ripgrep; preview pane
  scrolls to and highlights the match line.
- **Multi-file CLI** — `hjkl a.rs b.rs c.rs` opens all files as named slots.
- **Tab line** at the top of the screen listing all open buffers; rendered only
  when more than one buffer is open.
- **Tree-sitter syntax highlighting** in the buffer pane and picker preview
  (Rust, Markdown, JSON, TOML, SQL bundled via `hjkl-tree-sitter`).
- **Comment marker overlay** (`hjkl-tree-sitter::CommentMarkerPass`) — annotates
  `TODO` / `FIXME` / `FIX` / `NOTE` / `INFO` / `WARN` markers with distinct
  highlight styles; consecutive single-line comments inherit the marker across
  continuation lines.
- **Smart indent** — Enter, `o`, and `O` bump indent one level after `{`, `(`,
  or `[`; a leading close brace on the new line auto-dedents.
- **`.editorconfig` support** — `indent_style`, `indent_size`, `tab_width`, and
  `max_line_length` are applied automatically on file open.
- **`hjkl-picker` crate** (`crates/hjkl-picker`) — the entire picker subsystem
  is extracted into a standalone reusable crate with no direct dependency on
  `hjkl-engine`. Provides `Picker`, `PickerLogic` trait, `FileSource`,
  `RgSource`, `PreviewSpans`, and the fuzzy `score` function.
- **Shared braille spinner** (`hjkl-ratatui::spinner::frame`) — 10-frame braille
  animation at ~8 Hz via a monotonic epoch; used in picker loading indicators.
- **Shared register bank** across buffer slots — yank in one slot, paste in
  another; `dd` and other linewise ops write to the unnamed register so
  cross-buffer paste works correctly.
- **Info popup overlay** for `:reg`, `:marks`, `:jumps`, `:changes` — multi-line
  ex-command output renders as a centered floating popup.
- **Status line additions**: `REC@r` badge while recording a macro; pending
  count and pending operator block; search count `[n/m]`.
- **Cursor-line background** in the editor pane (subtle blue-grey); suppressed
  during `:` and `/` prompts.
- **`Gutter::line_offset`** field in `hjkl-buffer` — enables windowed preview
  snapshots to display original document line numbers in the gutter.
- **`Viewport::tab_width`** field — carries the active `tabstop` value through
  the render pipeline.
- **`:set softtabstop=N`** (`sts`) — Backspace deletes a soft tab as a unit; Tab
  fills to the next softtabstop boundary.

### Changed

- **Default tab settings** flipped to modern four-space soft tabs:
  `tabstop=4 shiftwidth=4 softtabstop=4 expandtab=on smartindent=on`. To revert
  to traditional vi defaults:
  `:set noexpandtab tabstop=8 shiftwidth=8 softtabstop=0`.
- **`:set tabstop=N`** is now threaded through `Viewport.tab_width` to the
  renderer and cursor position math end-to-end.
- **Picker prompt symbol** changed from `>` to `/` to match search semantics.
- **`PickerLogic` trait** gains `preview_top_row`, `preview_match_row`, and
  `preview_line_offset` so windowed sources can position, highlight, and
  label-offset the preview pane correctly.
- **Label spacing** across all pickers is uniform (2-cell prefix pad, no leader
  arrow).

### Fixed

- **Tab rendering** — tab characters expand to spaces aligned to tab stops;
  `cursor_screen_pos` accounts for tab visual width.
- **`dd` resets `sticky_col`** so subsequent `j` / `k` lands on the first
  non-blank column rather than the deleted line's column.
- **Paste linewise** reads from the unnamed register slot rather than a
  per-editor cache, fixing cross-buffer linewise paste.
- **Grep picker preview** is no longer empty (status-tag misuse) and now scrolls
  to the match line with correct file line numbers in the gutter.

## [0.1.1] - 2026-04-27

### Fixed

- `hjkl-editor` shell filter (`%!cmd`) now tolerates the child closing stdin
  before all input is consumed. Previously a `BrokenPipe` write error would
  short-circuit and mask the child's actual exit status (e.g. `%!exit 5`
  reported "cannot write to `exit 5`: Broken pipe" instead of "command exited
  5"). Now `BrokenPipe` falls through to `wait_with_output()` so the real exit
  status wins; other write errors still surface. Fixes a flaky CI failure on
  `shell_filter_failing_command_errors`.

### CI

- Replaced `release-plz.yml` with a tag-driven `release.yml` matching the
  org-wide canonical pattern. Runs fmt/clippy/test as a quality gate, then
  publishes the 4 hjkl crates to crates.io in dep order via an idempotent shell
  loop (curl-precheck + `cargo publish --locked`). Fires on
  `git push origin vX.Y.Z`.

## [0.1.0] - 2026-04-27

### Patch C-δ — Editor generic flip + SPEC freeze

The first stability-locked release. Folds the 0.0.20 — 0.0.42 churn (none
published to crates.io) into a single 0.1.0 cut: SPEC trait scaffolding, the
Buffer/Cursor/Query/BufferEdit/Search trait split, viewport relocation onto
`Host` (driven by GUI/TUI cross-platform requirement), motion + editor + vim
reach lifts, the `Editor` generic flip, removal of the pre-0.1.0 dyn-host shim,
and the SPEC freeze.

docs.rs surfaces the canonical API per upload.

#### Pre-1.0 trait-extraction arc (folded into 0.1.0)

23 unpublished patches (0.0.20 — 0.0.42) led up to the freeze. The arc themes:

- **Buffer surface discipline** (0.0.20 — 0.0.31): SPEC trait scaffolding;
  `Cursor` / `Query` / `BufferEdit` / `Search` component traits; `Sealed`
  super-trait gating downstream impls; <40-method cap on `Buffer`.
- **Viewport lift to Host** (0.0.32 — 0.0.34): `Buffer::viewport` field deleted;
  viewport storage + accessors moved to the `Host` trait so vim logic works in
  GUIs (pixel canvas, variable-width fonts) as well as TUIs (cell grid).
- **Editor field consolidation** (0.0.35 — 0.0.39): marks
  (`BTreeMap<char, Pos>`) and search state migrated from `Buffer` to `Editor`;
  `Buffer::dirty_gen` added for invalidation; `FoldProvider` + `FoldOp` lifted
  onto canonical engine surface.
- **Generic body** (0.0.40 — 0.0.42): 24 motion fns lifted to
  `B: Cursor + Query [+ &dyn FoldProvider]`; 70 + 151 internal `self.buffer.…` /
  `ed.buffer().…` reaches in editor.rs + vim.rs routed through the trait
  surface; viewport-math fns relocated as engine-side free fns;
  `apply_buffer_edit` seam centralized as the single concrete-on-`hjkl_buffer`
  funnel.

Buffer trait final shape: 14 methods. The full per-patch detail lives in git
(`git log v0.0.19..v0.1.0`); CHANGELOG entries for those patches are folded here
to keep crates.io history clean.

#### BREAKING — `Editor` generic over `B` + `H`

```rust
// 0.0.42 (and earlier):
pub struct Editor<'a> { /* concrete buffer + boxed dyn host */ }

// 0.1.0:
pub struct Editor<
    B: hjkl_engine::types::Buffer = hjkl_buffer::Buffer,
    H: hjkl_engine::types::Host = hjkl_engine::types::DefaultHost,
> { /* typed buffer + typed host */ }
```

The `'a` lifetime parameter (vestigial since the textarea field was ripped) is
removed. Defaults match the in-tree canonical impls so most call sites that
named `Editor` without a type parameter continue to type-check. Call sites that
wrote `Editor<'_>` / `Editor<'static>` need to drop the lifetime.

The vim FSM (`crate::vim` free functions, `Editor::mutate_edit`, the change-log
emitter, and the undo machinery) is bound to the canonical buffer:

```rust
impl<H: hjkl_engine::types::Host> Editor<hjkl_buffer::Buffer, H> {
    /* most methods */
}
```

The fully generic `<B: Buffer, H: Host>` impl exposes only universal accessors
(`buffer()` / `buffer_mut()` / `host()` / `host_mut()`). Custom buffer backends
compile against the trait but cannot run the vim FSM at 0.1.0 — see SPEC.md
§"Out of scope at 0.1.0" for the rationale (lifting `BufferEdit::apply_edit`
onto an associated type is post-0.1.0 work).

#### BREAKING — constructor

```rust
// 0.0.42:
Editor::new(KeybindingMode::Vim)
Editor::with_host(KeybindingMode::Vim, host)
Editor::with_options(buffer, host, options)

// 0.1.0:
Editor::new(buffer, host, options)
```

The legacy three-constructor surface is gone. There is no `#[deprecated]` shim —
every consumer migrates by passing the buffer, host, and `crate::types::Options`
explicitly. Call sites that don't need a custom host pass
`crate::types::DefaultHost::new()`; call sites that don't need custom options
pass `crate::types::Options::default()`.

The `Options::default()` defaults are SPEC-faithful (vim parity, `shiftwidth=8`
/ `tabstop=8`); the pre-0.1.0 `Settings::default()` defaulted to `shiftwidth=2`
(sqeel-derived). Tests and consumers that relied on `shiftwidth=2` need to
construct an `Options` with `shiftwidth: 2` explicitly.

#### BREAKING — `EngineHost` removed

The pre-0.1.0 object-safe shim trait (`EngineHost`) and its blanket
`impl<H: Host>` are gone. Hosts implement `Host` directly; the `Editor<B, H>`
generic carries the typed slot. `Editor::host()` now returns `&H` (was
`&dyn EngineHost`); `Editor::host_mut()` returns `&mut H`. Callers using the
host accessor need `crate::types::Host` in scope to call its methods through
trait dispatch.

#### BREAKING — `Buffer` trait sealed (preserved)

The `Buffer` super-trait is sealed via the private
`crate::types::sealed::Sealed` trait (already in place since 0.0.31). The
`Sealed` trait is now confirmed `pub(crate)`; downstream cannot
`impl Buffer for MyType` after this change. The canonical `hjkl_buffer::Buffer`
keeps its sealed-marker impl in `crate::buffer_impl`.

#### `apply_buffer_edit` decision

The seam between the engine and `hjkl_buffer::Buffer` for the mutate-edit
channel stays concrete on `&mut hjkl_buffer::Buffer` per the option (c) decision
documented in 0.0.42's CHANGELOG. SPEC.md §"Out of scope at 0.1.0" calls this
out explicitly: lifting onto `BufferEdit::Op` (an associated `type Edit;`) is
post-0.1.0 work — it forces every backend to design its own rich-edit enum and
rewrites the change-log machinery in terms of `B::Op`. The single seam at
`crate::buf_helpers::apply_buffer_edit` is the migration funnel for 0.2.0.

#### `EditorSnapshot::VERSION` frozen

`EditorSnapshot::VERSION` (currently `4`) is locked for the entire 0.1.x line.
Hosts persisting editor state between sessions can rely on the wire format being
stable; 0.2.0+ structural changes require `VERSION++` together with a
major-version bump.

#### SPEC.md frozen

`crates/hjkl-engine/SPEC.md` carries an explicit "0.1.0 (frozen 2026-04-27)"
header. The trait surface (14 `Buffer` methods across `Cursor` / `Query` /
`BufferEdit` / `Search`), `Host` trait surface, `FoldProvider` + `FoldOp`,
`Options`, `EditorSnapshot`, and the `Editor::new(buffer, host, options)`
constructor are all part of the frozen contract. Explicit non-goals: viewport
math on `Buffer`, `Editor`'s apply-edit funnel as part of the public trait
surface, and any host-flavoured fold-op enum (engine-canonical only).

#### `PUBLIC_API.md` regenerated

`crates/hjkl-engine/PUBLIC_API.md` is regenerated against 0.1.0 with the
simplified `cargo +nightly public-api` output. Top-level diff vs the 0.0.39
baseline:

- `Editor<'a>` → `Editor<B: Buffer, H: Host>` (every method now carries the new
  type params; the vim FSM impl is on `Editor<hjkl_buffer::Buffer, H>`).
- `Editor::new(KeybindingMode)` removed; `Editor::new(buffer, host, Options)`
  added.
- `Editor::with_host` / `Editor::with_options` removed.
- `EngineHost` trait + blanket impl removed.
- `motions::*` free functions now generic over `B: Cursor + Query [+ Search]`
  (vs concrete `&mut hjkl_buffer::Buffer` at 0.0.39 — they were lifted in 0.0.40
  but the PUBLIC_API.md hadn't been refreshed since 0.0.33 baseline).
- `BufferEdit::replace_all` added (already landed in 0.0.41; surfaced in this
  release's PUBLIC_API.md regeneration).

#### Tests

684 (unchanged from 0.0.42). One test (`bare_set_reports_current_values`)
updated to assert `shiftwidth=8` per the SPEC-faithful default; one golden
snapshot (`golden_ex__set_listing.snap`) regenerated for the same reason. The
vim-FSM-internal `editor_with` helper explicitly sets `shiftwidth: 2` so the
indent / outdent assertions don't churn.

#### Migration

Consumers updating from 0.0.x:

```rust
// before
let mut editor = Editor::new(KeybindingMode::Vim);

// after
use hjkl_engine::types::{DefaultHost, Options};
let mut editor = Editor::new(
    hjkl_buffer::Buffer::new(),
    DefaultHost::new(),
    Options::default(),
);
```

For consumers that wrote `&mut Editor<'_>` in fn signatures:

```rust
// before
fn step(ed: &mut Editor<'_>, input: Input) { ... }

// after
fn step<H: hjkl_engine::Host>(
    ed: &mut Editor<hjkl_buffer::Buffer, H>,
    input: Input,
) { ... }
```

For consumers that constructed an `Editor` with a custom host:

```rust
// before
let mut editor = Editor::with_host(KeybindingMode::Vim, my_host);

// after
let mut editor = Editor::new(
    hjkl_buffer::Buffer::new(),
    my_host,
    hjkl_engine::types::Options::default(),
);
```

## [0.0.19] - 2026-04-26

### Added

- `smartcase` honoured by `/` and `?` searches. When `ignorecase` is on and the
  pattern contains an uppercase letter, the search compiles case-sensitive
  (matches vim's combined `ignorecase` + `smartcase` behaviour). Wired through
  `Settings::smartcase`, `Options::smartcase`, and `:set smartcase` / `:set scs`
  / `:set nosmartcase`.
- `:set` listing now includes `smartcase=on/off`. Golden snapshot updated.

## [0.0.18] - 2026-04-26

### Added

- `expandtab` honoured by the insert-mode Tab key. When `Settings::expandtab` is
  true, Tab inserts `tabstop` spaces; otherwise a literal `\t` (existing
  behaviour). Wired through `Options::expandtab`, `current_options` /
  `apply_options`, `:set expandtab` / `:set noexpandtab` / `:set et`.
- `:set` listing now includes `expandtab=on/off`. Golden snapshot updated.

## [0.0.17] - 2026-04-26

### Added

- `Options::textwidth` (u32, default 79) — engine-native bridge for the legacy
  `Settings::textwidth` driving `gq{motion}` reflow. Wired through
  `current_options` / `apply_options` and `set_by_name("tw"|"textwidth")`.

## [0.0.16] - 2026-04-26

### Added

- `Options::wrap: WrapMode` (engine-native equivalent of `hjkl_buffer::Wrap`).
  `Editor::current_options` / `apply_options` map between `WrapMode` and
  `hjkl_buffer::Wrap` at the boundary.
- `set_by_name` / `get_by_name` recognise vim's `wrap` and `linebreak` (`lbr`)
  toggles. Combined state collapses into the single `WrapMode` field:
  `wrap=off → None`, `wrap=on,lbr=off → Char`, `wrap=on,lbr=on → Word`.

## [0.0.15] - 2026-04-26

### Added

- IncSearch highlight emission. `Editor::highlights_for_line` now branches:
  active `/` or `?` prompt → `HighlightKind::IncSearch` for live-preview
  matches; committed pattern → `SearchMatch` (existing behaviour). Hosts can
  paint live-preview distinctly from committed-search.
- Insta golden snapshots for ex-command output
  (`crates/hjkl-editor/tests/golden_ex.rs`): `:registers`, `:marks`, bare
  `:set`. Catches user-visible text format churn.

## [0.0.14] - 2026-04-26

### Changed (potentially breaking)

**Trait sealing pass.** Every `#[doc(hidden)] pub` item exposed for cross-crate
ex.rs reach is now sealed behind a proper public method. Hosts that were poking
at `Editor`'s internal fields (and ignoring the `#[doc(hidden)]` warning) now go
through the methods.

Field visibility flipped from `pub` to `pub(crate)`:

- `Editor::vim`, `Editor::registers`, `Editor::settings`, `Editor::file_marks`,
  `Editor::syntax_fold_ranges`, `Editor::undo_stack`, `Editor::change_log`.

`VimState::last_edit_pos`, `jump_back`, `marks` flipped back to `pub(super)` (no
longer reachable from outside hjkl-engine). `vim::do_undo` / `vim::do_redo`
flipped from `pub` to `pub(crate)`; the crate-root re-export is gone.

### Added (replacing the sealed surface)

New Editor methods covering everything ex.rs (and any other host) previously
reached via raw fields:

- `Editor::syntax_fold_ranges() -> &[(usize, usize)]`
- `Editor::file_marks()` — iterator over uppercase marks
- `Editor::buffer_mark(c) -> Option<(usize, usize)>`
- `Editor::buffer_marks()` — iterator over lowercase marks
- `Editor::last_jump_back() -> Option<(usize, usize)>`
- `Editor::last_edit_pos() -> Option<(usize, usize)>`
- `Editor::pop_last_undo() -> bool`
- `Editor::undo()` / `Editor::redo()`

Previously `#[doc(hidden)]` methods on `Editor` are now plain `pub`:

- `jump_cursor`, `mutate_edit`, `push_undo`, `restore`, `settings_mut`.

Fresh rustdoc covers every promoted method.

### Migration

Code that read fields directly should switch to method calls. For write-side
mutation (`undo_stack.pop()` etc.), `pop_last_undo()` is the supported
replacement.

## [0.0.13] - 2026-04-26

### Added

- `Editor::feed_input(PlannedInput) -> bool` — SPEC Input dispatch. Bridges
  hosts that don't carry crossterm (buffr CEF, future GUI shells) into the
  engine. Char + Key variants route to handle_key; Mouse / Paste / FocusGained /
  FocusLost / Resize fall through.

## [0.0.12] - 2026-04-26

### Added

- `Editor::intern_engine_style(types::Style) -> u32` — SPEC-typed style
  interning. Same opaque ids as the ratatui-flavoured `intern_style`; both share
  the underlying table.
- `Editor::engine_style_at(id) -> Option<types::Style>` — looks up an interned
  style by id, returns it as a SPEC type. Hosts that don't depend on ratatui
  (buffr, future GUI shells) reach this surface for syntax-span installation.

## [0.0.11] - 2026-04-26

### Added

- `Editor::take_changes() -> Vec<EditOp>` — pull-model SPEC change drain. Editor
  accumulates EditOp records on every mutate_edit; take_changes drains the
  queue. Best-effort mapping for compound buffer edits (JoinLines, InsertBlock,
  etc.) emits a placeholder covering the touched range.
- `Editor::current_options() -> Options` and `Editor::apply_options(&Options)`
  bridge between SPEC Options and legacy Settings. Lets hosts read/write engine
  config through the SPEC API.

## [0.0.10] - 2026-04-26

### Added

- `hjkl-engine::types::OptionValue { Bool, Int, String }` — typed value carrier
  for the `:set` parser.
- `Options::set_by_name(name, OptionValue) -> Result<(), EngineError>` and
  `Options::get_by_name(name) -> Option<OptionValue>`. Vim-style short aliases
  supported (`ts`, `sw`, `et`, `isk`, `ic`, `scs`, `hls`, `is`, `ws`, `ai`,
  `tm`, `ul`, `ro`).

## [0.0.9] - 2026-04-26

### Changed (breaking the 0.0.8 snapshot wire format)

- `EditorSnapshot::VERSION` bumped to `3`. Adds a
  `file_marks: HashMap<char, (u32, u32)>` field carrying the uppercase / "file"
  marks (`'A`–`'Z`). Survives `set_content`, so hosts persisting between tab
  swaps round-trip mark state. 0.0.8 snapshots fail `restore_snapshot` with
  `EngineError::SnapshotVersion`.

## [0.0.8] - 2026-04-26

### Changed (breaking the 0.0.7 snapshot wire format)

- `EditorSnapshot::VERSION` bumped to `2`. The struct gains a
  `registers: Registers` field carrying vim's `""`, `"0`, `"1`–`"9`, `"a`–`"z`,
  and `"+`/`"*` slots. 0.0.7 snapshots fail `restore_snapshot` with
  `EngineError::SnapshotVersion`.
- `Slot` and `Registers` derive `Serialize` / `Deserialize` behind the `serde`
  feature.

## [0.0.7] - 2026-04-26

### Added

- `hjkl-engine::types::RenderFrame` — borrow-style render frame the host
  consumes once per redraw. Coarse today: mode + cursor + cursor_shape +
  viewport_top + line_count.
- `Editor::render_frame()` builder.
- `Editor::highlights_for_line(u32)` — SPEC `Highlight` emission with
  `HighlightKind::SearchMatch` for search hits.
- `Editor::selection_highlight()` — bridges the active visual selection to a
  SPEC `Highlight` with `HighlightKind::Selection`. None outside visual modes;
  visual-line / visual-block collapse to their bounding char range.

### Changed

- `CursorShape` now derives `Hash` so `RenderFrame` can derive it.

## [0.0.6] - 2026-04-26

### Added

- `hjkl-engine::types::EditorSnapshot` — coarse serde-friendly snapshot of
  editor state for host persistence. Carries `version`, `mode`, `cursor`,
  `lines`, `viewport_top`. Bumps the snapshot `EditorSnapshot::VERSION` constant
  to track wire-format compat.
- `hjkl-engine::types::SnapshotMode` — status-line mode summary embedded in the
  snapshot.
- `Editor::take_snapshot()` — produces an `EditorSnapshot` at the current state.
- `Editor::restore_snapshot(snap)` — restores from a snapshot; returns
  `EngineError::SnapshotVersion` on wire-format mismatch.

## [0.0.5] - 2026-04-26

### Changed

- **`ex.rs` relocated from `hjkl-engine` to `hjkl-editor`.** Ex commands now
  live in the crate they belong to. Consumers reach `ex` via
  `hjkl_editor::runtime::ex` (unchanged surface — the facade was already routing
  there).
- `hjkl-editor` gains `regex` as a direct dep (ex uses it for `:s/pat/.../`) and
  `crossterm` as a dev-dep.
- `mark_dirty_after_ex` is now a free function. Ex callsites that previously
  wrote `editor.mark_dirty_after_ex()` now write `mark_dirty_after_ex(editor)`.

### Added (engine internal — sealed at 0.1.0)

Several `pub(super)` / `pub(crate)` items on `Editor` and `VimState` gained
`#[doc(hidden)] pub` visibility so ex commands can reach them across the crate
boundary:

- `Editor`: `vim`, `undo_stack`, `registers`, `settings`, `file_marks`,
  `syntax_fold_ranges` fields; `settings_mut`, `mutate_edit`, `push_undo`,
  `restore`, `jump_cursor` methods.
- `VimState`: `last_edit_pos`, `jump_back`, `marks` fields.
- `vim::do_undo`, `vim::do_redo` re-exported at the crate root.

These are explicit churn-phase exposures and will be sealed under the 0.1.0
trait extraction. Do not rely on them.

### Migrated tests

5 vim+ex integration tests (`gqq` reflow, `gq` motion, paragraph break
preservation, `gqq` undo, `:marks` listing) moved from
`crates/hjkl-engine/src/vim.rs` to
`crates/hjkl-editor/tests/vim_ex_integration.rs`. cargo dev-dep cycles between
hjkl-engine and hjkl-editor produce duplicate type IDs, so they must run from
the editor side.

## [0.0.4] - 2026-04-26

### Changed

- Workspace `homepage` set to <https://hjkl.kryptic.sh>.
- Per-crate READMEs now carry CI / crates.io version / docs.rs / License /
  Website badges and a Website + Source line.

## [0.0.3] - 2026-04-26

### Added

- `hjkl-engine::Editor::take_content_change()` — pull-model coarse change
  observation. Returns `Some(Arc<String>)` if content changed since the last
  call, `None` otherwise. Drains the dirty flag. Bridges the gap until SPEC's
  `take_changes() -> Vec<EditOp>` ships with full edit-path instrumentation.
- `hjkl-engine::types::Viewport` (re-exported as `PlannedViewport` at the crate
  root to disambiguate from `hjkl_buffer::Viewport`).
- `hjkl-engine::types::BufferId` opaque newtype.
- 513-case proptest harness for the FSM (`tests/proptest_fsm.rs`): random
  keystroke sequences never panic, and three Escapes always return to Normal
  mode.

## [0.0.2] - 2026-04-26

### Added

- `hjkl-engine::types` extended with the planned 0.1.0 trait surface: `Options`,
  `EngineError`, `Modifiers`, `SpecialKey`, `MouseEvent`, `MouseKind`, `Input`,
  `Host` trait. All additive — coexists with the legacy runtime types in
  `hjkl-engine::editor`.
- `hjkl-editor`: real facade crate (was placeholder). Exposes three modules:
  `buffer`, `runtime`, `spec`. Consumers depend on hjkl-editor alone instead of
  all three downstream crates.
- `hjkl-buffer/IMPLEMENTERS.md`: caller invariants documentation.
- `hjkl-buffer` criterion benches under the `budgets` harness:
  `insert_char_1MB_buffer`, `search_next_10k_lines`, `cold_load_10MB`.
- `hjkl-buffer` proptest roundtrip property suite for `apply_edit` (768 random
  scenarios per test run).

### Changed

- `hjkl-engine::types::Edit` re-exported from the crate root as `EditOp` to
  disambiguate from `hjkl_buffer::Edit`.

## [0.0.1] - 2026-04-26

### Added

- `hjkl-buffer`: full sqeel-buffer port with cursor, edits, motions, folds,
  viewport, search. ratatui Widget impl behind optional `ratatui` feature.
  Default features off — buffer is UI-agnostic.
- `hjkl-engine`: full sqeel-vim port with vim FSM, ex commands, registers,
  dot-repeat, marks. ratatui + crossterm currently mandatory; phase 5 trait
  extraction will move them behind features.
- `hjkl-engine::types` module: SPEC core types (`Pos`, `Selection`,
  `SelectionKind`, `SelectionSet`, `Edit`, `Mode`, `CursorShape`, `Style`,
  `Color`, `Attrs`, `Highlight`, `HighlightKind`). Additive alongside the legacy
  public API; trait extraction wires the FSM and Editor onto these
  progressively.

### Changed

- `hjkl-editor` and `hjkl-ratatui`: still placeholder; ship 0.0.1 to keep
  lockstep workspace version.

## [0.0.0] - 2026-04-26

### Added

- Initial placeholder release. Reserves `hjkl-engine`, `hjkl-buffer`,
  `hjkl-editor`, and `hjkl-ratatui` names on crates.io. No public API.
- `MIGRATION.md` — extraction plan and design rationale.

[Unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.15.1...HEAD
[0.15.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.15.1
[0.15.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.15.0
[0.14.11]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.14.11
[0.14.10]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.14.10
[0.14.9]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.14.9
[0.14.8]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.14.8
[0.14.7]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.14.7
[0.14.6]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.14.6
[0.14.5]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.14.5
[0.14.4]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.14.4
[0.14.3]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.14.3
[0.14.2]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.14.2
[0.14.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.14.1
[0.14.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.14.0
[0.13.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.13.0
[0.12.2]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.12.2
[0.12.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.12.1
[0.12.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.12.0
[0.11.5]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.11.5
[0.11.4]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.11.4
[0.11.3]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.11.3
[0.11.2]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.11.2
[0.11.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.11.1
[0.11.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.11.0
[0.10.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.10.1
[0.10.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.10.0
[0.9.3]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.9.3
[0.9.2]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.9.2
[0.9.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.9.1
[0.9.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.9.0
[0.8.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.8.1
[0.8.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.8.0
[0.7.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.7.0
[0.6.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.6.0
[0.5.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.5.0
[0.4.6]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.4.6
[0.4.5]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.4.5
[0.4.4]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.4.4
[0.4.3]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.4.3
[0.4.2]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.4.2
[0.4.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.4.1
[0.4.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.4.0
[0.3.4]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.3.4
[0.3.3]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.3.3
[0.3.2]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.3.2
[0.3.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.3.1
[0.3.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.3.0
[0.1.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.1.1
[0.1.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.1.0
[0.0.19]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.19
[0.0.18]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.18
[0.0.17]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.17
[0.0.16]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.16
[0.0.15]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.15
[0.0.14]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.14
[0.0.13]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.13
[0.0.12]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.12
[0.0.11]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.11
[0.0.10]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.10
[0.0.9]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.9
[0.0.8]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.8
[0.0.7]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.7
[0.0.6]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.6
[0.0.5]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.5
[0.0.4]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.4
[0.0.3]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.3
[0.0.2]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.2
[0.0.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.1
[0.0.0]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.0.0
