# hjkl — review backlog

Single source for open findings, deferred decisions, and blocked work. Findings
use symbol names rather than line numbers so references survive refactors.

**Docs consolidation 2026-08-04.** Folded in and deleted: `docs/code-review.md`,
`docs/performance-review.md`, `docs/embed-rpc.md` — read `git log` for what they
said before this. All 14 code-review findings and all ten perf findings shipped
the same day they were written; the four perf minors still open are §1.11; the
`--embed` / `--nvim-api` design record's binding conventions are under Standing
constraints.

**Corrections made during the merge:**

- `buf_line_chars` moved since the perf review cited it (`hjkl-buffer`
  `buf_helpers.rs` → `hjkl_engine::buf_helpers`); the open item records its
  current home.
- The reviews' cited line numbers were re-verified against the current tree;
  those that had shifted (notably `hjkl-buffer-tui`'s render restructure) are
  recorded by symbol, per this backlog's convention.
- The five code-review "Cleared" candidates and the two perf "confirmed fine"
  notes are summarized in the Record, not carried with full repros; the perf
  minor for buffer-impl's find-next/find-prev row allocation was a
  cross-reference to finding #2, not a separate item, and was merged into it.
- Coverage GAPs from both reviews are compressed into the §1.7 coverage note.

## 1. Open work — ranked

### 1.1 Undo-tree follow-ups

- `retarget_current` is O(tree distance), not O(1): a `g-` crossing to a far
  branch still walks to the fork. Those ancestors genuinely have to change, so
  the worst case stays O(depth) for an adversarial branch layout. Every benched
  case, and `u` / `<C-r>`, are distance 1.
- **A freshly deserialized undofile has no keyframes**, so its first deep jump
  is O(depth). Eager construction may waste work and memory; not measured.
- **Considered and declined: renumbering `depth` in `prune_root_side`
  (2026-08-03).** When the root's on-path child is promoted, it keeps its
  original `depth` instead of restarting at 0 the way `clear_all` does. That is
  a real inconsistency, and it is harmless: `depth` is read in exactly two
  places — `is_keyframe` (a multiple-of-`KEYFRAME_INTERVAL` test) and
  `child_depth = parent.depth + 1` — so the keyframe ladder keeps its spacing
  and only its offset from the new root moves, leaving the root-to-nearest-
  keyframe distance bounded by `KEYFRAME_INTERVAL` as before. Serialization
  fixes the offset anyway: `depths_from_root` recomputes from the new root on
  load. Not worth a change.

### 1.3 Round-2 deferred items

| Item                           | Where                                                  | Why deferred                                                                                          |
| ------------------------------ | ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------- |
| Settings/Options full collapse | `hjkl-engine/src/editor.rs`                            | L-sized; staged for 0.1.0.                                                                            |
| P6 per-cell span resolve sweep | engine span layering                                   | M–L, layering-order-sensitive; needs a sortedness guarantee first.                                    |
| P10 wrap-mode scrolloff O(h²)  | wrap scroll math                                       | Wrap is not the default; needs the same care as P6.                                                   |
| R10 stringly errors → enum     | `hjkl-app/src/git.rs` (`Result<(), String>`)           | Design decision, not mechanical.                                                                      |
| R13 `unnecessary_wraps` triage | dispatch tables                                        | Uniform signatures are deliberate; needs per-family review.                                           |
| Y5 `hjkl-editor::spec`         | `crates/hjkl-editor/src/lib.rs`                        | Needs external-consumer confirmation before deletion; workspace grep is insufficient for public APIs. |
| Multicursor `lens` vector      | `hjkl-engine/src/editor.rs` (`buf_line_chars` collect) | O(buffer) per edit, but gated behind unwired multicursor.                                             |

### 1.4 LSP and span follow-ups

| Item                                                | Where                       | Note                                                                                                                                                                                 |
| --------------------------------------------------- | --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `styled_spans` cannot be removed — `sqeel` reads it | `hjkl-engine/src/editor.rs` | `sqeel-tui` pins published `hjkl-engine` and reads the field (one `mem::take`, two `clone`s). Removing it needs a supported accessor in `sqeel` first. The field documents this now. |

**The block-row marker is a convention, not a type (2026-08-02).** A span whose
`end_byte` is exactly one byte past its row's content means "multi-row span
covering this row's end — paint its bg across the whole row". It is produced by
`row_local_end` (duplicated in `hjkl_syntax` and `hjkl_picker::preview`),
preserved by `Editor::translate_row_spans`, and consumed by
`BufferView::paint_row` and `resolve_span_style`. Nothing in the type system
says so, and the two copies of `row_local_end` can drift. A `Span` flag (or a
newtype) would state it once; not done because `hjkl-picker` deliberately has no
dependency on `hjkl-syntax` and `Span` is public API on a published crate.

Consequences that were accepted rather than fixed:

- The fill applies to ANY multi-row span carrying a `bg`, not just markdown code
  blocks. Shipped themes only give a bg to `@markup.raw.block`, so today it is
  exactly the code-block case; a theme that gave `@string` a bg would tint whole
  rows of a multi-line string.
- On the cursor row the block bg is dropped entirely rather than blended, unlike
  the fold-header row, which blends `fold_line_bg` with `cursor_line_bg`
  (`mix_colors` in `render.rs`). Blending would read as a third colour on every
  code-block line the cursor visits; suppression was chosen deliberately.
- Suppression matches on the block's COLOUR, not on which span carries it, so
  any span resolving to that bg yields on the cursor row. That is what covers a
  fence line, where markdown emits `@markup.raw.block` twice — once for the
  block, once as a narrow span over the ``` itself. A theme that gave some
  unrelated capture the identical bg would see it yield there too; the two are
  indistinguishable on screen anyway.
- **An indented code block's opening fence keeps an untinted prefix.** In
  `- item` / `  ```rust`, the fence row's span starts at the backticks, so the
  two leading indent spaces stay untinted while the block's other rows are
  tinted edge to edge (verified in a real render, 2026-08-02). Filling leftward
  from a span's start would be a guess about where the block begins; fixing it
  properly needs the block's own start column from the grammar.
- Not reviewed: whether other span-consuming renderers want the same fill.
  `hjkl-markdown-tui` and `qf_dock_spans` (`apps/hjkl/src/app/quickfix.rs`)
  build their own span tables and were not touched or examined.

### 1.4b Fold gaps left after the 2026-08-02 autofold audit

Fold ranges were diffed against neovim's `vim.treesitter.foldexpr` (folds
enumerated with `foldclosed` / `foldclosedend`, not foldlevels — levels merge
adjacent siblings and read as false differences). After the fixes, every fold
hjkl emits for markdown, yaml, html, json, lua, css, bash, python, toml, Go, C,
C++, Java, PHP, Ruby, C#, JavaScript and TypeScript is exactly one of neovim's.
What neovim still has that hjkl does not:

- **No folds inside a workflow YAML's `run: |` scripts** — and the reason is not
  the fold code. Folds inside injected regions now work in general
  (`folds::extract_fold_ranges_rope_with_injections`, 2026-08-03: markdown
  fences and HTML `<script>` / `<style>` match neovim exactly), but a YAML
  buffer has **no injections at all** in hjkl:
  `GrammarLoader::build_and_install` takes `injections.scm` from the grammar's
  own repository — deliberately, since the curated query repos "use non-standard
  predicates" — and `tree-sitter-grammars/tree-sitter-yaml` @1805917 ships only
  `queries/highlights.scm`. So `.github/workflows/ci.yml` is unchanged at 152
  folds against neovim's 173 (verified after the injection work; the 21 missing
  are all inside embedded bash). This also means YAML `run:` blocks are not
  syntax-highlighted as bash today.

  Closing it needs a decision, not just code:
  1. Bundle injection queries the way `folds.scm` is bundled
     (`builtin_injections(lang)`, falling back when the grammar repo ships
     none). Changes highlighting too, not just folds.
  2. **Blocker if we do:** injection discovery evaluates NO predicates — not in
     `folds::injection_regions` and not in
     `highlight_range_with_injections_rope`. nvim-treesitter's YAML query gates
     the bash injection on `(#any-of? @_run "run" "script" …)`, so without
     predicate evaluation every block scalar in every YAML file — including
     `description: |` prose — would be parsed as bash. Predicate evaluation for
     injection matches (`#eq?`, `#any-of?`, `#match?`) is the prerequisite.
  3. Or keep taking injections from grammar repos and accept the gap.

- **One fold per start row.** `View::add_fold` / `set_auto_folds` key folds by
  `start_row`, so where two nested folds legitimately share one (markdown's
  `(list)` and the `(list_item (list))` inside it), hjkl keeps the outer and
  drops the inner — one nesting level fewer than vim. Changing this touches the
  fold model, `remove_fold_at`, and every toggle path, so it was left alone. Now
  also a `foldlevelstart` divergence: `set_auto_folds` derives each fold's
  nesting level by containment over the folds it actually keeps, so wherever a
  start-row collision drops an inner fold, everything below it is one level
  shallower than vim thinks it is and `foldlevelstart=N` opens one level too
  much there. Same fix, same blast radius.
- **Language fold queries are hjkl's own, not neovim's.** They fold a subset
  (e.g. rust also folds bare blocks that neovim leaves alone; toml folds
  tables). This is deliberate, and the ranges are correct — noted so the next
  differential run does not read it as a regression.

Left undone by the injected-folds work (2026-08-03), all in
`folds::extract_fold_ranges_rope_with_injections`:

- **One level of injection only.** A region inside a region — bash inside a
  `run: |` inside a ` ```yaml ` markdown fence — is not folded. Recursing needs
  a depth cap and a story for the cost below; nothing measured needs it yet.
- **The region-discovery query pass cannot be memoised, and it is the whole
  remaining cost.** Every extraction runs the host's `injections.scm` over the
  full tree; region byte ranges move with every edit, so there is nothing stable
  to key a memo on. Measured on `hjkl-engine/src/editor.rs` (7334 lines,
  release, best of 20): 9.0 ms host-only, 19.7 ms with injections and no memo,
  10.0 ms with `InjectedFoldCache` warm — i.e. the parses are now ~free and the
  ~1 ms that remains is discovery. Only languages whose grammar ships an
  `injections.scm` pay it (locally: cpp, html, javascript, jinja, lua, markdown,
  rust, vue). Rust is the worst ratio in the set — it injects itself into every
  macro body and yielded 2 extra folds on that file. If this ever shows up in a
  keystroke profile, the cheap lever is a setting to disable injected folds, or
  skipping regions whose language equals the host's.
- **`resolve` can block.** The highlight path's exposure is accepted; the fold
  path's was closed 2026-08-07 (`45aab1cc`): the injected-folds pass now
  resolves via the new cache-only `LanguageDirectory::by_name_cached`, so a fold
  pass can never be the trigger for a clone+compile+fetch. An unloaded injected
  region contributes no folds until the highlight path loads it.
- **An injected fold that starts on a host fold's start row is dropped.** The
  `by_start` map keeps the widest span, unchanged from before. Deliberate:
  `View::set_auto_folds` keys folds by start row too, so the second one could
  not survive downstream anyway. Fixing it is the "One fold per start row" item
  above.
- **Not verified:** whether hjkl's injection discovery ignoring `#offset!` in
  the HIGHLIGHT path (`highlight_range_with_injections_rope`) mis-highlights
  anything today. The fold path applies the directive; the highlight path still
  does not, and nothing installed locally uses it on `@injection.content`.

Coverage note: the fold comparison was run over this repo's markdown, one rust
file, `.github/workflows/ci.yml`, and small hand-written yaml/json/lua/css/
bash/python/html/toml fixtures, plus (2026-08-02, second pass) hand-written
Go/C/C++/Java/PHP/Ruby/C#/JS/TS fixtures — a "representative" one per language
(~40-77 lines, covering every node type each query captures) and an adversarial
"edge" one for Java annotations, Ruby `elsif`/heredoc/`=begin`, Go `const`/`var`
blocks, PHP `match` and alternative `if:`/`endif` syntax, and C preprocessor
conditionals. Both passes were re-measured against neovim 0.12.4 with
nvim-treesitter's own fold queries. The compact fixtures are pinned as
`<lang>_fold_ranges_match_neovim` in `hjkl_syntax`'s test module (all
`#[ignore]`d — CI has no grammars; run with
`cargo test -p hjkl-syntax --lib -- --ignored`).

Measuring neovim's folds has one trap worth writing down: a headless nvim parses
injections **lazily**, so the enumeration script must call
`vim.treesitter.get_parser(0):parse(true)` before reading `foldclosed`. Without
it nvim reports only the host language's folds — 152 on
`.github/workflows/ci.yml` instead of 173 — which reads as agreement with hjkl
rather than as the measurement being wrong.

A second trap, from the C# miss: **a bundled fold query is keyed by the name
`GrammarRegistry` resolves the _extension_ to, which is not necessarily the
`.scm`'s file name, and a query that never runs looks exactly like a language
with nothing foldable.** C# folded nothing at all for that reason. Now guarded
by `folds::every_bundled_fold_query_is_reachable_by_extension`, which is
grammar-free and runs in the normal lane.

Coverage gap left by the `foldlevelstart` fix (2026-08-02): the LEVEL rule was
checked, the level-to-grammar wiring was not, end to end. `set_auto_folds`'s
levels are pinned by `set_auto_folds_closes_folds_deeper_than_foldlevelstart` in
`hjkl_buffer::folds`, whose expectations are neovim's measured output for a
6-fold / 3-level lua file — but the ranges are typed into the test as a
constant, not extracted from a live tree. The end-to-end test
(`auto_fold_pass_applies_foldlevelstart_as_a_level` in `syntax_glue`) uses
`foldmethod=marker` so it can run without grammars. Nothing runs the real
`foldmethod=expr` path at a non-zero `foldlevelstart` and compares the closed
set to neovim's. Cheapest close: add `foldlevelstart` to whatever the
`<lang>_fold_ranges_match_neovim` fixtures already parse, asserting closed state
rather than only ranges.

Granularity differences found in the second pass — all deliberate, ranges on
both sides correct, recorded so the next differential run does not read them as
regressions:

- **hjkl anchors on the declaration, neovim on the body.** For Java, C# and
  (partly) PHP, neovim's queries capture `(class_body)`,
  `body: (declaration_list)`, `(block)` — nodes that start on the `{`. hjkl
  captures `(class_declaration)`, `(method_declaration)`,
  `(namespace_declaration)`, which start on the signature. With K&R braces the
  two agree exactly; with Allman braces (idiomatic C#) hjkl's fold starts one
  row earlier, and with leading annotations it starts on the FIRST annotation —
  measured on Java `@Deprecated`/`@SuppressWarnings` above `public class B`:
  hjkl `(2, 9)`, neovim `(4, 9)`. hjkl's range is the true span of the node it
  captured, but the visible header row is then `@Deprecated` and the `class B {`
  line is hidden inside the fold. Whether to switch these queries to body nodes
  is a UX decision, not a correctness one — left for the user to call.
- **Node types neovim folds and hjkl does not**, per language: Go
  `(const_declaration)`, `(var_declaration)`, `(expression_case)`,
  `(default_case)`, `(literal_element)`; C/C++ `(comment)`, `(preproc_if)`,
  `(preproc_ifdef)`, `(preproc_else)`, `(case_statement)`; Java
  `(import_declaration)+`, `(argument_list)`, `(annotation_argument_list)`; C#
  `(using_directive)+`, `(accessor_list)`, `(initializer_expression)`; PHP
  `(array_creation_expression)`, `(match_expression)`,
  `(namespace_use_declaration)+`; Ruby `(else)`, `(singleton_class)`,
  `(lambda)`; JS/TS `(switch_case)`, `(switch_default)`, `(arguments)`,
  `(do_statement)`, `(with_statement)`, `(catch_clause)`, `(import_statement)+`.
  Note the `+` ones: neovim folds a _run_ of consecutive imports/usings as one
  fold, which hjkl's one-node-per-capture extractor has no way to express at
  all.
- **Node types hjkl folds and neovim does not**: C/C++/PHP
  `(compound_statement)` (so an Allman-braced function body, and every `else` /
  `catch` block, gets its own fold), Java `(array_initializer)`, Ruby `(block)`
  (the `{ |x| … }` form) and `(begin)`, JS/TS `(statement_block)`. Ranges
  verified correct in every case.
- **vim's fold model truncates siblings; hjkl's does not.** Where two folds
  would abut (a consequence block ending on the same row an `else` block
  begins), vim ends the first one row early: neovim reports `(26, 27)` for a Go
  block hjkl reports as `(26, 28)`. This is the line-based fold model, not the
  query, and it shows up in the raw diff for every language with `else`
  branches. Not a difference worth chasing.

Not verified in either pass: fold behaviour for these nine languages at
non-trivial scale (all fixtures are hand-written and under 80 lines), and any
language outside the 20 with a bundled `folds.scm`.

**No CI lane runs the grammar-backed tests.** Every one in `hjkl_syntax` is
`#[ignore]`d because CI has no grammars, so a defect in that path can only be
caught by running `cargo test -p hjkl-syntax --lib -- --ignored` by hand.
`incremental_path_matches_cold_for_small_edit` sat red at HEAD for the whole of
the 2026-08-02 audit for exactly that reason (fixed 2026-08-04).

### 1.5 Remaining differential-oracle divergences

Every entry below was reproduced through
`cargo run -p hjkl-compat-oracle --release --example dfcase` against neovim
0.12.4 and left unfixed on purpose. Preserve each fixed case in the tier-2
compatibility corpus and verify it against nvim before changing expectations.

### 1.5b Left open by the motion- and blockwise-parity passes

**A text object in blockwise visual collapses the block (2026-08-04).**
`editor_ext::visual_text_obj_extend` used to send EVERY text object down one
path — collapse to charwise (or linewise), set `visual_anchor`, move the cursor
— and never wrote `block_anchor` / `block_vcol`, which are what `block_bounds`
reads.

**Word objects fixed 2026-08-06 (`2be058b8`).** `iw`/`aw`/`iW`/`aW` now keep
`FsmMode::VisualBlock` and write `block_vcol` to the landed column (cursor
already matched nvim; only the mode was wrong), so `<C-v>iw<` reaches the
blockwise shift arm and `<C-v>jiw~` flips the block instead of the line. Pinned
by seven corpus cases in `corpus/tier2_block_textobj.toml` measured against
neovim 0.12.4 (word mode + `~`/`<` consequences, quote no-op).

The 2026-08-04 behaviour table was re-measured against neovim 0.12.4 during that
work, and two of its rows did not reproduce at the positions probed:

- **Brackets (`ib`/`ab`/`iB`) and tags (`it`) fixed 2026-08-07 (`ddc81797`).**
  They stayed BLOCKWISE in nvim at every position probed (`(x\ny)`, `(ab\ncd)`,
  `a(b\nc)d`, `<a>x\ny</a>`, cursors on rows 0 and 1) — and the block is a
  selection NO-OP: the object is found but mode stays `visual_block` and the
  cursor keeps the post-motion position (`<C-v>jib~` flips the same cells as
  `<C-v>j~`). hjkl now returns without touching mode or cursor for
  `TextObject::Bracket` / `TextObject::XmlTag` in blockwise visual; the NOTE at
  `visual_text_obj_extend` was corrected (it claimed brackets collapse — that
  row of the 2026-08-04 table did not reproduce). Four corpus cases pinned
  against neovim 0.12.4.
- **Quotes (`i"`) no-op on both sides** at the measured position
  (`a"hello\nworld"b`, cursor inside the content) — hjkl's `text_object_range`
  returns no object from that row, so hjkl already matches nvim. Pinned by the
  `blockwise_quote_obj_noop` corpus case.
- **`ip`/`is` stay blockwise in nvim but hjkl collapses** (to visual-line /
  visual). The cursor lands identically, but the block's column/row EXTENT after
  the object is not yet measured per position (`<C-v>jip~` on `"one\n\ntwo"`
  flipped both paragraphs' first word in nvim, which is more than "rows kept,
  cursor extends") — so the fix shape is not yet determined.

Open per-object routing for paragraph / sentence; each needs its measured corpus
cases first. The word-object code documents this in the NOTE comment at
`visual_text_obj_extend`.

**`H` / `L` / `gE` in blockwise visual still diverge on motion and cursor.**
`<C-v>H>` is the standing repro. Blockwise `~` is correct.

**Every edit used to clone its payload for the change log — closed 2026-08-07
(`1f062fe0`), but not the way the note below proposed.** The `Vec<EngineEdit>`
built per edit had NO consumer anywhere in hjkl or its checked-out siblings
(every `take_changes` call site is a test), and the proposed `Arc<str>` in the
log would not have reduced the paste peak anyway: the payload is freshly built
per paste, so an `Arc::from` still copies. The log is now opt-in
(`View::set_change_log_enabled`, default OFF) and `mutate_edit` skips
`edit_to_editops` entirely when unsubscribed — measured peak RSS 651 MB → 456 MB
on the paste path, 45 MB → 18 MB on a keystroke burst. The 2026-08-02
~3.1x/~4.1x peak-RSS multiple is gone. The public API is unchanged.

**Left open by the blockwise-paste rewrite (2026-08-02).**

- **`visual_paste` was examined 2026-08-07 and cleared.** Neither it nor
  `do_block_paste` (`vim/command.rs`) uses `rope_to_lines_vec` — they build
  yank-sized `segments`/`chunks`, never a whole-document `Vec<String>`. The §1.7
  suspicion did not reproduce.
- **`content()` hides trailing-newline bugs from unit tests.** It appends a
  newline when the rope lacks one, so a paste that eats the buffer's terminator
  compares equal through it. That is why the first version of the rewrite passed
  every hand-written geometry case while the nvim oracle failed on
  `visual_block_paste_past_eof`, which reads the rope directly. Any assertion
  about buffer termination must read the rope, as
  `block_paste_past_eof_preserves_the_trailing_newline` does. Whether other
  buffer-shape assertions in the suite are blunted the same way was not audited.

**The engine message queue is deliberately dumb.** `Editor::push_error` /
`take_errors` carries vim's `E342` out of `do_paste` and `apps/hjkl` drains it
in both post-mutation sync paths (`App::drain_engine_errors`). An empty register
is still silent (vim says nothing there either), and nothing rate-limits the
queue, so code that pushes per-iteration would flood the toast bar.

**A corpus case could not pin a value when nvim was absent — closed 2026-08-07
(`6cddabde`).** `run_single` now runs the hjkl driver first and, when nvim is
missing, compares its outcome against the authored `expected_*` fields — a real
pass-or-mismatch instead of a wholesale skip (every case carries an
`expected_buffer`). The corpus tests' nvim early-returns are gone (the five
author-error tests keep theirs, since they sanity-check nvim itself); verified
by running the whole corpus with nvim hidden from PATH: 75/75 pass via the
fallback. The `expected_*` fields now guard hjkl on every machine, CI lanes
without neovim included.

**The `:s` curswant fix has no oracle case.** It is covered by two unit tests in
`hjkl-engine`'s `substitute` module, because the corpus driver cannot replay `:`
keys. A corpus case would need the ex layer driven some other way.

**The differential fuzzer reports 69 distinct divergences at seed 777** (89
before the 2026-08-02 pass, 84 after it, then 83 with the counted-`$` failure
rule, 78 with the backward-word-motion rewrite, 77 with the blockwise-shift
geometry, all 2026-08-04, and 69 after the harness cleared nvim's undo history
post-seeding and enabled search / fold / `gq` tokens — the `u`/undo noise class
is gone and the remaining `u` divergences are real post-edit undo parity). The
bulk are the known-excluded blockwise `<C-v>` non-delete operators plus the
entries above.
`cargo run -p hjkl-compat-oracle --release --example difffuzz -- 400 777`
reproduces the list; build with `--examples`, not `--example difffuzz`, or the
other binary goes stale.

### 1.6 Cursor-move API migration

`Move` and the debug invariant shipped; phase 1 (hjkl-vim's motion dispatch)
landed 2026-08-04 — every `Motion` variant is classified (Vertical / Jump /
Horizontal, no plain motion is Raw) and `execute_motion` + the `+`/`-`/`_`
inline arms re-move the landed cursor through `Editor::move_cursor` instead of
the deleted `apply_sticky_col` catch-all; the debug invariant never fired and
the oracle divergence counts were unchanged. Remaining phases:

1. ~~Migrate remaining `hjkl-engine` motions to `Editor::move_cursor`.~~ (done
   2026-08-04)
2. Migrate `hjkl-vim` motion, command, bridge, visual, and operator paths. Fix
   insert paths first, then visual yank, then normal operators/edits; widen the
   invariant after each class is clean.
3. Migrate `apps/hjkl` cursor writes.
4. Make raw cursor primitives crate-internal, remove public `set_sticky_col`,
   and reduce `apply_sticky_col` to the vertical clamp.
5. Report counts by `Move` variant and justify every `Move::Raw` site. Keep the
   compat oracle and PTY e2e behavior unchanged.

**Assessment 2026-08-07: needs-guidance, not started.** Phase 2 is the backlog's
largest item (~100 `buf_set_cursor_rc` write sites across the vim layer) and was
not attempted this round: the shipped debug invariant deliberately does NOT
check insert/visual/operator cursor moves (the curswant.rs doc enumerates them
as a different bug class), so nothing in the gate distinguishes a correct
per-site `Move` classification from a mechanical `Raw` translation — the exact
failure the §5 note warns "would compile while preserving the bug class". A
rushed full pass would ship that silently. The phases exist to widen the
invariant after each class is clean, which makes it inherently multi-session;
the honest next step is one dedicated session on the insert path with the
invariant widened per class.

Design and measured violation classes are in §5.

### 1.8 Open from the 2026-08-01 review and the 0.40.0 cut

#### Needs an owner decision, not more work

| Item                                                | Where                                                  | Decision needed                                                                                                                                                                                                               |
| --------------------------------------------------- | ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Anvil TOFU sidecar survives uninstall               | `apps/hjkl/src/app/ex_dispatch.rs` (`anvil_uninstall`) | Keeping it is safer (a changed artifact still trips `ChecksumMismatch`) but a user uninstalling to recover from a bad install cannot clear it. Delete on uninstall, or add `:Anvil forget`.                                   |
| `hjkl-quickfix` / `hjkl-app` have no `CHANGELOG.md` | those two crates                                       | Both are published and both shipped BREAKING changes in 0.40.0, documented only in the root changelog. BCTP says do not create changelog files unasked — but these are the two crates a consumer checks after a failed build. |

#### Deferred refactors

| Item                                           | Where                                                                                                         | Why deferred                                                                                                                                                                                                                                                |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Four hand-rolled width truncators              | `hjkl-statusline`, `hjkl-prompt-tui`, `hjkl-editor-tui`, `hjkl-which-key`, `hjkl-buffer-tui`                  | Each re-implements "accumulate `UnicodeWidthChar::width` until the budget runs out" with different tab handling. Cross-crate UI refactor, real regression surface, no bug attached.                                                                         |
| `is_safe_relative_path` vs `safe_join`         | `hjkl-bonsai/src/runtime/source.rs`, `hjkl-anvil/src/installer.rs`                                            | Same invariant, different contracts. The clean unification is reworking both onto `hjkl_fs::resolve_under`, which also catches a symlink in the prefix — a behaviour change, so its own change.                                                             |
| `Options` / `OptionsConfig` still hand-written | `hjkl-engine/src/types.rs`, `hjkl-app/src/config.rs`                                                          | The option registry drives every `:set` table and the config key mapping, but not these two structs — both need compile-time fields (engine snapshot, serde schema). Pinned by tests instead. A generating macro was considered and declined as too opaque. |
| Oversized modules                              | `nvim_api.rs` (5.7k), `explorer.rs` (4.4k), `render.rs` (3.8k), `lsp_glue.rs` (3.2k), `ex_dispatch.rs` (3.2k) | Recording only; splitting has no correctness payoff. Noted because the duplicate-`WorkspaceEdit` bug lived in `lsp_glue.rs` and survived precisely because the file is that size.                                                                           |

#### Wide-char column math: what the fix left open (2026-08-02)

`hjkl_buffer::geom` is `unicode-width`-aware and `Editor`'s duplicate
`visual_col_for_char` delegates to it. Three things were deliberately NOT done.

- **hjkl's `l` moves one CHAR, nvim's moves one GRAPHEME.** Surfaced the moment
  the oracle could express non-ASCII columns (2026-08-04). On `"e\u{301}abc"`
  hjkl lands on the combining mark (char 1) where nvim lands on `a`; on
  `"\u{2764}\u{fe0f}abc"` hjkl lands on the variation selector and nvim on `a`.
  Both verified against neovim 0.12.4. Deliberately NOT in the corpus — they
  would be cases pinning a known divergence — and the same root cause as the
  emoji-width entry below; fixing either wants grapheme segmentation, which is
  its own change.

- **Emoji presentation sequences are one cell narrower than vim's.** vim widens
  `U+2764 U+FE0F` ("❤️") to two cells because it segments graphemes;
  `unicode-width` is consulted per `char` in both `geom::cell_width` and
  `paint_row`, giving 1 + 0. The two sides of hjkl agree, so the cursor stays on
  its glyph — but a terminal that renders the sequence two cells wide will show
  the text one cell right of where hjkl thinks it is. Fixing it needs grapheme
  segmentation in `paint_row` first (`unicode-segmentation` is not currently a
  dependency of `hjkl-buffer-tui`), then in `cell_width`. Declined as its own
  change: it is a renderer change with a real regression surface, not a
  column-math change.

- **Control characters measure one cell, not vim's two.** `paint_row` maps them
  through `sanitize_control` to single-width Control Pictures glyphs (`U+0001` →
  `␁`), so vim's `^A` notation was never what hjkl paints. Matching the painted
  glyph was chosen over matching vim. Revisit only if `sanitize_control` is
  changed to emit two-cell `^X`.

Not re-checked after the fix: `hjkl-picker`'s preview column math and the four
hand-rolled width truncators listed above. They do their own width accounting
and were out of scope; whether any of them also disagrees with `paint_row` is
unknown.

#### Smaller, unclaimed

- **Bare `:!cmd` gives the child no tty.** `hjkl-ex/src/shell.rs` runs it under
  `Command::output()`, which captures stdout and hands the child a null stdin,
  so `:!git commit` or `:!less` cannot work. Vim suspends the TUI and passes the
  terminal through. Either implement the suspend or document the limitation on
  `:!`.
- **The trash directory has no reaper — NEEDS A DECISION (assessed
  2026-08-07).** `$XDG_CACHE_HOME/hjkl/trash/` grows without bound and
  `MAX_RETRIES = 1000` means the 1001st deletion of a same-named file fails
  rather than recycling a slot. But `crates/hjkl-app/src/trash.rs` documents the
  no-reaper behavior as DELIBERATE: "the whole point of trashing rather than
  deleting is that the editor does not get to decide when the data is gone", and
  the slot-exhaustion failure as "the intended failure mode — a full trash
  surfaces as a failed delete, not as a silent overwrite of something the user
  still has". A reaper contradicts that stated contract, so the decision is the
  user's: keep the documented design (do nothing), add an explicit
  `:Trash`-style command, or cap by count/size with a clearly-labeled policy.
  The slot-exhaustion half alone (a naming scheme that cannot collide) is
  fixable without deleting anything, but the "intended failure mode" doc would
  need updating too.
- **Mutex-poisoning policy is documented, not enforced.** `buffer.rs` now states
  that `lock().unwrap()` on buffer state is deliberate and a poisoned lock is
  fatal. The ~110 call sites are unchanged, so one panic while any of those
  locks is held still takes down every later access, including the save path.
- **`buf_set_cursor_rc` does not maintain curswant, and `7e260e27` moves the
  cursor with it.** The landing position is correct, but a following `j` may
  snap to a stale column — the exact class of latent bug §1.6 exists to prevent.

### 1.9 Left open by the 2026-08-03 explorer and file-discovery changes

Two changes shipped that day: `explorer.open` became a startup preference only
`:set` writes, and the explorer, both pickers and `:grep` moved onto one file
policy in `hjkl_fs::project`. What they left behind, none of it blocking.

**`:set explorer.open=…` is a string match, not a registry option.** It is
handled in `App::dispatch_ex`'s host-owned pre-pass, beside `mouse` and
`endofline`, because the left dock is host state with no engine `Settings` field
to hang it on. Two consequences follow, and they are the same ones `mouse` and
`endofline` already have — this joins that group rather than creating it:

- `hjkl_engine::options_registry` does not know the name, so `:set all` omits it
  and nothing completes it.
- Only `=true` / `=false` / `?` parse. `:set explorer.open` bare and
  `:set noexplorer.open` are not accepted; the dotted name makes the vim-style
  `no` prefix read badly, which is why it was left out rather than forgotten.

(The third consequence — headless/embed never reaching the pre-pass, and with it
the §1.8 `:set` write-through item — shipped 2026-08-04: all three modes share
one `set_tokens` interception pass, and headless/embed staying session-only is
now a stated decision; see the Record.)

**`explorer.width` still persists on interactive resize.** `<C-w><`/`<C-w>>` and
a border drag write it immediately, while `explorer.open` now only moves on an
explicit `:set`. The asymmetry is deliberate — dragging a border IS the user
stating a width preference, whereas toggling a dock says nothing about how the
next session should start — but it is an inconsistency a user can notice, and it
is recorded so it is not "fixed" by accident in either direction.

**`.gitignore` is honoured only inside a git repository.** Measured while
building `hjkl_fs::project`: with no `.git` directory above the walk root, the
`ignore` crate applies no gitignore rules at all, so an ignored path is listed
and searched. This matches ripgrep and is why `project`'s own tests create a
`.git` marker. Not a bug — recorded because it looks exactly like the policy
silently failing, and the next person to hit it will assume it is broken.
`.ignore` files are unaffected and apply everywhere.

**The explorer's ignore-stack cost per rebuild was not measured.** It previously
opened the repo once per rebuild (`git2::Repository::discover`) and asked
`is_path_ignored` per entry; it now calls `project::list_dir` per expanded
directory, and each of those builds the ignore stack for that directory
including its parents. On a deep tree with many expanded directories that is
strictly more ignore-file reading than before. Nothing was measured either way,
and no slowness was observed — but the claim "this is not slower" is not one
this pass can make. If it ever matters, the lever is a matcher built once per
rebuild and threaded through `push_children`, the shape the old `repo` argument
had.

**`I` in the explorer is deliberately wider than the shared policy.** It reveals
gitignored entries, which is the one place a path visible in the tree is NOT
findable in the pickers or searchable by `:grep`. `H` only ever narrows, so it
cannot produce the same mismatch. Intended; noted so a future consistency audit
does not read it as a leak.

**The `grep` / `findstr` search backends cannot honour gitignore.**
`hjkl_fs::project` is the one policy behind the explorer, both pickers and
`:grep`, and ripgrep reproduces it exactly via `RG_IGNORE_ARGS` (asserted by
`rg_args_match_walk_policy`). The fallbacks `detect_grep_backend` picks when
ripgrep is absent cannot: `grep` and `findstr` have no notion of ignore files,
so they search ignored paths too. Both now exclude `.git` (`--exclude-dir=.git`
for grep; findstr has no equivalent and excludes nothing), which is as close as
those tools reach. Closing it properly means enumerating with
`project::walk_builder` and passing the file list to the backend — bounded argv,
and awkward for the streaming live-grep source, so it was not attempted. Users
with ripgrep installed are unaffected.

### 1.7 Harness, coverage, and hardening

- **The Wayland mock's `reset()` does not tell the client anything
  (2026-08-02).** `MockState::reset` clears `offer_payloads` and the pending
  offers, but sends no `data_offer` teardown and no null selection, so the
  client keeps the previous test's `current_clipboard_offer` id. A `receive` on
  that id finds no payload and `dispatch_pending_receives` answers with zero
  bytes and a closed fd — the client then correctly reports `Ok([])`.
  Surprising, not a product bug: a real compositor destroys the offer it is
  replacing. The `get` tests are now written to poll for the bytes they expect
  rather than for `Ok`, so they ride through it. Making the mock send a null
  selection on `reset` would remove the state entirely; not done, because it
  changes the mock's protocol behaviour and the tests no longer depend on it.
- **Review coverage gap.** The 2026-07-29 pass covered `hjkl-vim`,
  `hjkl-engine`, `hjkl-buffer`; the 2026-08-01 pass covered `apps/hjkl`,
  `hjkl-lsp`, `hjkl-ex`, `hjkl-editor(-tui)`, `hjkl-completion`, `hjkl-prompt`,
  `hjkl-menu`, `hjkl-picker`, `hjkl-anvil`, `hjkl-app`, `hjkl-fs`,
  `hjkl-clipboard`, `hjkl-bonsai`, and touched `hjkl-quickfix`,
  `hjkl-buffer-tui`, `hjkl-statusline`, `hjkl-which-key`, `hjkl-prompt-tui`,
  `hjkl-css` only where a finding led there. The 2026-08-03 pass re-covered
  `hjkl-buffer/src/undo.rs`, `hjkl-vim/src/`, and `hjkl-engine/src/` in full and
  reached `hjkl-vim-types` and `hjkl-app`'s undofile path by call-chain tracing,
  so it widened nothing. Everything else in `crates/` has never been reviewed —
  including `hjkl-config`, `hjkl-keymap(-tui)`, `hjkl-layout`,
  `hjkl-syntax(-tui)`, `hjkl-markdown(-tui)`, `hjkl-theme(-tui)`,
  `hjkl-tabs(-tui)`, `hjkl-hover(-tui)`, `hjkl-holler(-tui)`, `hjkl-form`,
  `hjkl-fs-watch`, `hjkl-fuzzy`, `hjkl-mangler`, `hjkl-kitty`, `hjkl-lang`,
  `hjkl-icons`, `hjkl-splash(-tui)`, `hjkl-info-popup(-tui)`, `hjkl-vim-tui`,
  `hjkl-vim-types`, `hjkl-xdg`, and the remaining `-tui` siblings. Stated as a
  gap, not a plan.

  The 2026-08-04 review and perf passes re-covered `hjkl-vim`, `hjkl-vim-tui`,
  `hjkl-vim-types`, `hjkl-buffer`, `hjkl-buffer-tui`, `hjkl-picker`,
  `hjkl-completion`, `hjkl-fuzzy`, `hjkl-fs`, `hjkl-fs-watch`, `hjkl-css`,
  `hjkl-markdown(-tui)`, `hjkl-layout`, `hjkl-syntax`, `hjkl-config`,
  `hjkl-engine-tui`, `hjkl-editor-tui`, `hjkl-syntax-tui`, `hjkl-clipboard`
  core, parts of `hjkl-bonsai` (comment_markers, hex_color, rope_slice,
  predicate, rainbow, highlighter 772-1460) and parts of `hjkl-ex` (range,
  parse, global, builtins 617-1414, registry, effect, complete 187-316, setopt
  1-200); partial reads touched which-key, tabs, lsp, mangler, xdg, kitty,
  statusline, theme color, compat-oracle, hover-tui, holler-tui, prompt-tui,
  editor-tui, menu, and form. Their GAPs (never examined): the `apps/hjkl`
  non-app files — the planned review agent for that slice was never spawned —
  most of `app/`'s remainder, engine `editor.rs` / `substitute.rs` /
  `options_registry` / `policy` / `discipline` ranges, ex `builtins.rs` 1-617
  and 1415-5190 plus shell/listings/setopt remainder, bonsai highlighter 1-772
  and 1461-2583 plus `runtime/*`, clipboard backends, remaining `hjkl-buffer`
  (folds, geom, listchars, motion, search, selection, span) and engine (abbrev,
  discipline, input, keymap_motion, selection_shift, tag) modules, and every
  `tests/` directory.

- Stabilize flaky PTY e2e cases. Cache/CWD/color isolation landed in `ca3852b2`;
  the explorer `dd` tests that failed under `cargo test`'s thread pool are
  fixed. Unspecified PTY flakes may remain.
- **`CwdGuard` users audited (2026-08-02) — left as they are, with reasons.**
  After the trash fix the guard only ever `chdir`s; every remaining caller is a
  test whose subject genuinely IS the working directory:
  `apps/hjkl/src/app/explorer.rs` (the explorer roots at the cwd),
  `apps/hjkl/src/render.rs`, `apps/hjkl/src/app/tests/ex.rs` and
  `apps/hjkl/src/app/tests/splits_windows.rs`. Removing the dependency there
  means letting `App::new` take a root instead of calling `current_dir`, which
  is a real API change and not a test-harness one. The lock does not isolate
  them from unguarded readers, so a residual cwd race is possible in principle —
  none observed in 30 post-fix runs.

  Not audited: `crates/hjkl-ex/tests/fs_policy.rs` calls
  `std::env::set_current_dir` directly with no guard at all. It is a separate
  test binary, so it cannot race the `hjkl` binary's tests, but it can race
  other tests in its own.

- **Left open by the 2026-08-02 cold-cache grammar-race fix.**
  - **A failed grammar load is never retried.**
    `SyntaxLayer::poll_pending_loads` drops the `PendingLoad` on
    `LoadEvent::Failed` and nothing re-requests, so one transient failure leaves
    the buffer plain text until the file is reopened. That is what turned a
    momentary clone collision into a session-long symptom. A retry (or a
    user-visible "grammar failed, `:e` to retry") is unimplemented — decide
    whether the retry is automatic, and how it avoids hammering a genuinely
    broken manifest entry.
  - **`QuerySourceCache::resolve_highlights` was not given the same lock.** Its
    staging file is pid-suffixed, so it is safe across processes, but two
    _threads_ in one process resolving the same language would collide on that
    name. They cannot today: the only callers reach it under
    `GrammarLoader::load`'s per-grammar install lock, and the async pool dedups
    by name. It is safe by the callers' structure rather than by its own, which
    is worth closing if another caller appears.
  - **macOS and Windows were not exercised.** Everything was verified on Linux.
    `hjkl_fs::with_lock_exclusive` is `LockFileEx` on Windows and `flock`
    elsewhere; the mutual-exclusion regression test
    (`stage_and_publish_never_runs_two_populates_at_once`) uses two threads, so
    on every platform it proves the in-process wait set, and only the CI matrix
    proves the OS lock.
  - **The `grammar tests` lane has no grammar cache.** `Swatinem/rust-cache`
    covers `~/.cargo` and `target/` only, so every run re-clones and re-compiles
    every grammar from scratch — which is exactly why the lane is the place this
    class of bug surfaces, and also why it is the slowest job. Caching
    `~/.cache/bonsai` would make it faster and blind to cold-start races.
    Deliberately not done.

- **Full-buffer allocation on motions and rectangular edits (2026-08-03).** Each
  of these rebuilt the whole document into a `Vec` for an edit or query whose
  extent is small. None was a correctness bug and none was measured; the
  2026-08-07 pass fixed the whole family (see the Record) with row-range
  reads/writes:
  - `vim::text_object::sentence_step_forward` collected the entire buffer into a
    `Vec<Vec<char>>` on every `)` keystroke — fixed 2026-08-06 (`8a7f3613`): it
    now reuses the row-by-row forward scan extracted from `sentence_boundary`
    (which was already row-by-row; only `step_forward` was live), measured 31.5
    ms → 2 µs cold on a 50k-line buffer with the next boundary three rows away.
    Worst case (a buffer with no terminator at all) is 62 ms vs the old 36 ms:
    the scan must visit every row either way, and the shared per-row closures
    collect each row a few times — the same shape `(`'s backward scan already
    ships with. If it ever matters, cache each row once per loop.
  - `vim::visual_ops::transform_block_case`, `block_replace_bounds` and
    `visual_replace_char`, `vim::text_object_ops::reflow_rows` /
    `reflow_rows_keep_cursor` and the indent family (`indent_rows`,
    `indent_block`, `outdent_block`, `outdent_rows`, `auto_indent_rows`) — all
    ten fixed 2026-08-07 (`f2b5661f`): each now reads only the touched rows via
    `rope_line_to_str` and writes per-row `Edit`s (one bounded Replace where the
    row count changes, mirroring `Editor::splice_row_range`). Measured on a
    100k-line buffer: block case-op 57.7 ms → 0.125 ms, `gq` 65.4 ms → 0.041 ms,
    peak RSS 43.1 MB → 27.5 MB. Reflow splice boundaries pinned by four unit
    tests.

- **"CI green" does not include the Cron workflow.** miri / fuzz / deny / bench
  run on a separate weekly schedule and are not checked by a release. They were
  not checked for 0.40.0. `cron.yml` now says so in its header, but nothing
  enforces it: either fold the cheap ones into the release gate or add an
  explicit pre-release step that reads the last Cron result.

- **Considered and declined (2026-08-04): persisting the fuzz corpus or crash
  artifacts.** A red `fuzz` job is signal enough on its own; caching state
  across runs is not worth the machinery. The cost accepted with it is that a
  crash artifact dies with the runner — the `Base64:` line in the job log is the
  only copy, and Actions logs expire, which is why the 2026-05-04 and 2026-04-27
  fuzz failures can no longer be reproduced. Reproduce from the log while the
  run is still in retention, and fix forward.

  Old fuzz failures are settled, so the expiry costs nothing today: the eight
  `cargo-fuzz` failures between 2026-04-27 and 2026-07-27 were two real editor
  crashes, both fixed on 2026-07-28 by `bd7e6ad4` (`rope_row_range_str` slicing
  inside a multi-byte line separator) and `a11351b5` (auto-indent bracket depth
  saturating to `i32::MIN` and wrapping through `as usize`), and both have
  regression tests.

- **A fuzz artifact replayed against the wrong commit proves nothing.** Both of
  those artifacts were replayed at HEAD, passed, and then passed against
  `10e3ca45^` too — a clean run on supposedly-buggy code, which makes the HEAD
  result meaningless as well. `10e3ca45` is the CI commit; its message says the
  crashes were "fixed in the two preceding commits", so its parent already had
  both fixes. Replayed against `2ecd7c3b` each artifact reproduced its exact CI
  panic. Two preconditions for this check to mean anything: pick the commit
  before the FIX, not before the commit that mentions it, and confirm
  `arbitrary`'s version is unchanged across the window (1.4.2 throughout, here)
  or the same bytes decode to a different `FuzzInput`.

- **Ten minutes a week on one harness is the whole fuzz budget, and it is the
  job with the best hit rate.** `handle_key` is the only target;
  `-max_total_time=600` is the only budget. Everything it has ever found was a
  real, keystroke-reachable defect (five now: the two fixed 2026-07-28 and the
  three fixed 2026-08-04), and a single ad-hoc ten-minute run on 2026-08-04
  produced three more the week after a clean scheduled run. Whether to raise
  `-max_total_time`, add harnesses (an ex-command target and a `:s` target are
  the obvious gaps — neither is reachable from `handle_key`), or both, is an
  owner decision about runner minutes. Still open.

- **The sibling repos pin `runs-on` to a concrete image; hjkl does not.** infr,
  and the other siblings its `cron.yml` names, run `ubuntu-26.04`. hjkl uses
  `ubuntu-latest` in both `ci.yml` and `cron.yml`, except `cron.yml`'s
  `vim_compat`, which is pinned to `ubuntu-24.04` so a runner-image bump cannot
  move the neovim the oracle diffs against. Deliberately not changed while
  matching infr's other `cron.yml` features (2026-08-04): pinning the cron jobs
  alone would leave one workflow disagreeing with the other, and pinning
  `ci.yml` too changes glibc and every apt package the matrix installs — a
  behaviour change to the test environment, not a workflow-hygiene one. Decide
  it repo-wide or not at all.

- **`vim_compat` does not diff against the neovim the corpus was measured on.**
  Corpus expectations are taken from neovim 0.12.4 on the workstation; the CI
  job installs whatever `ubuntu-24.04`'s apt carries, which is older. Not
  measured — recorded because a corpus case that passes locally and fails (or
  passes for the wrong reason) in CI would look like flake. Check what the job
  actually reports before trusting either side.

### 1.10 Left open by the 2026-08-04 code review

- **`nvim_buf_set_text` and `nvim_buf_get_text` clamp, never error, on rows past
  end-of-buffer.** Real nvim errors E966/E1206 for out-of-range rows; hjkl
  clamps to `line_count-1` (get_text) and slices clamped (set_text). Not fixed
  because the review found no client misbehaviour from the clamping.

(The default-scope `:g`/`:v` phantom-row and `replace_all` change-log items
shipped 2026-08-04 — see the Record.)

### 1.11 Open from the 2026-08-05 code review (audit depth)

The full report is in §8. All 15 code findings with a feasible fix shipped the
same day (commits `0518ca77`..`77c2be3a` — see the Record, §7). Still open, each
with its §8 finding number:

- **`\d`/`\s`/`\w` are Unicode-wide in rust-regex, ASCII-only in vim — closed
  2026-08-07 (`cf83b533`).** The translator now rewrites them out of class (`\d`
  → `[0-9]`, `\s` → `[ \t]`, `\w` → `[0-9A-Za-z_]`, plus `\D`/`\S`/`\W`), and
  inside `[...]` all eight class escapes are emitted as the LITERAL set {`\`,
  letter} — measured against neovim 0.12.4, which also disproved the shipped
  `[\a]`/`[\A]` "alphabetic range" translation (nvim treats them literally too).
  Substitute + translation tests pin it. New adjacent finding: vim's `\b` is the
  BACKSPACE char, not a word boundary, and rust-regex's only boundary token is
  `\b` — which the `\<`/`\>` word-boundary rewrite emits. hjkl keeps `\b` = word
  boundary (a literal-backspace pattern is rare; every word search needs the
  boundary), so a user pattern containing `\b` diverges from vim. Accepted
  trade-off; recorded so a parity pass does not "fix" it blindly.
- **vim `\ze`/`\zs` (start/end of match) mis-translate** (§8 #10 half) — needs
  match-boundary rewriting rust-regex has no anchor for; currently compiles to
  nonsense or nothing rather than corrupting.
- **Windows `EmptyClipboard` runs before allocation, wiping the prior clipboard
  on failure** (§8 #19) — needs a Windows host to verify the reorder; the macOS
  NUL-panic half of that finding shipped (`4323e39d`).

(The macOS NUL fix and the X11/Wayland clipboard fixes are in `4323e39d`.)

### 1.12 Open from the 2026-08-10 sweep + user reports

Full reports in §9 (review), §10 (audit), §11 (tidy), §12 (perf). The remaining
user-reported bug and the audit findings are the top items. Worked as slices:
delegate → review → commit → push; each slice pruned from this list on
completion.

1. **Explorer sidebar render borking (user report 2026-08-10) — NEEDS
   GUIDANCE.** With the cursor in the sidebar/explorer, `eee` / `llll` motions
   that scroll the sidebar content were reported to corrupt the sidebar render.
   Investigated twice (2026-08-10): the non-animated scroll path and the
   animated-frame path both render correctly under the test harness — two
   regression tests added (`scrolling_keeps_explorer_rows_aligned`,
   `scrolling_animated_frame_keeps_explorer_rows_aligned`, commit `8682037d`)
   fail when the animated top is not written into the render viewport. Ordinary
   `e`/`l` motions never arm scroll animation (only
   `scroll_full_page`/`scroll_half_page` set the hint), so the reported scenario
   cannot animate on current main; the symptom may predate the 2026-07-16
   smooth-scroll fix (`dbc142c3`) or be a real-terminal redraw artifact the
   in-memory harness cannot see. Waiting on the user: build/commit they saw it
   on, `:set scroll_duration_ms`, and whether it still reproduces on current
   main.
2. **MEDIUM — `:Anvil` host commands un-gated in `--nvim-api`** (§10 #2). Gate
   on `fs_restricted()`/`shell_disabled()` like `:make`/`:grep`/`:!`.
3. **MEDIUM — `:DiffOrig` reads slot filename unguarded (found fixing §10 #1).**
   `apps/hjkl/src/app/diff.rs:28` (`diff_orig`) does `std::fs::read(&path)` on
   the slot filename with no policy gate, reachable in RPC mode via `:DiffOrig`
   (`ex_host_cmds.rs:625`) after `nvim_buf_set_name` renames the buffer to an
   outside path — same leak shape as the fixed HIGH. Fix: route through the new
   `resolve_read_path` helper (ex_dispatch.rs) like
   `reload_current`/`checktime_slot`; add an nvim_api regression test.
4. **RPC `:e` reads unbounded** (§10 hardening) — route the two `--embed`/
   `--nvim-api` read sites through the capped reader.
5. **Tidy §11 items 1–9** — dead SHA consts, rope→lines duplication,
   display_width / leading-vcol dedup, `feed` copies, root-finder dedup,
   `Loading(String)` payload, SHIFT divergence (§11 #8 — a decision, not
   mechanical).
6. **Perf §12 items 1–4** — picker per-keystroke label/sort, `:s` full-buffer
   materialization, O(rows × folds) scans.

## 2. Blocked on platform access

| Finding                                                      | Location                                                         | Blocker                                                                             |
| ------------------------------------------------------------ | ---------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| INCR transfer timeout signalled as completion                | `x11_thread.rs` (`prune_expired_incr_sends`), Wayland equivalent | Needs a live session; truncated transfer remains indistinguishable from completion. |
| `SELECTION_NOTIFY` refusal arm ignores selection             | `x11_thread.rs` refusal arm                                      | Needs a live X server; unrelated selection refusal can be read as ours.             |
| `CString::new(..).expect(..)` panics on NUL in a type string | `hjkl-clipboard/src/backend/macos.rs`                            | Needs a Mac.                                                                        |
| Windows FFI paths lack runtime coverage                      | `hjkl-fs/src/identity.rs`, `hjkl-fs/src/dir.rs`                  | Needs a Windows host.                                                               |

## 3. Deferred security design

### Remote grammar compilation and `dlopen` (issue #314)

`hjkl-bonsai/src/runtime/grammar.rs` and `compile.rs` download tree-sitter
grammars, compile them with `$CC`/`$CXX`, and `dlopen` them. The bundled
manifest pins `git_url`/`git_rev` but has no signature or artifact-hash
verification. This is not remotely reachable today because the manifest is
`include_str!` bundled. A signature/hash-pinning design is required before code
changes.

The 2026-08-01 review read `compile_into` and confirmed its path validation
(`runtime/compile.rs`) is correct for the traversal case — the outstanding risk
is the design one above, not a missing check.

## 4. Process reference

### Gates

Per item: run workspace clippy with warnings denied, format, full nextest
(including e2e when app code changes), and the nvim compatibility oracle. Never
edit the oracle corpus to make a change pass. Performance items require measured
before/after results.

**`cargo machete` is a CI job this list does not cover.** Moving a dependency's
last use into another crate leaves the old `Cargo.toml` entry behind; clippy,
fmt and the whole test suite stay green, and CI fails on
`cargo-machete (unused deps)`. It cost a red run on `d71ee045` when the `ignore`
walk moved from `hjkl-picker` to `hjkl-fs`. Run it after any change that moves
code between crates. The fix is deleting the dependency — never a
`[package.metadata.cargo-machete]` suppression, which would make the job blind
to the next one.

### Platform lint coverage

Linux-only lint runs do not cover platform-gated code. `hjkl-fs`,
`hjkl-clipboard`, and `hjkl-lsp` can be cross-linted for macOS and Windows;
crates pulling tree-sitter, mimalloc, or aws-lc-sys require CI runners.

### Benchmarks

| Bench                               | Measures                                               |
| ----------------------------------- | ------------------------------------------------------ |
| `hjkl-buffer-tui/benches/render.rs` | viewport render, short vs long lines                   |
| `hjkl-buffer/benches/undo.rs`       | cold `g-` jump cost vs undo depth                      |
| `hjkl-buffer/benches/budgets.rs`    | per-operation budget guards                            |
| `hjkl-app/benches/swap.rs`          | full swap write + undo serialization vs size and depth |

### Standing traps

- Preserve exact char, byte, and grapheme units in rope math.
- Read an `Edit`'s semantics before applying its inverse through `apply_edit`.
- `hjkl_driver` cannot replay `:` keys, and `cargo test -p hjkl` skips the e2e
  binary.
- **A criterion bench that takes its subject BY VALUE charges the teardown to
  the routine.** `iter_batched` hands the routine its input by value and
  `outputs.extend(inputs.into_iter().map(&mut routine))` sits between
  `Measurement::start` and `end` (confirmed by reading criterion 0.8.2's
  `bencher.rs`), so the O(N) drop of a whole `UndoTree` is measured as part of
  the jump. That is what made `single_deep_jump` look depth-scaling however
  cheap the jump got. Use `iter_batched_ref`. Any future undo bench that takes
  the tree by value has the same defect; `single_deep_jump` was deliberately
  left alone so its recorded history stays comparable, and
  `single_deep_jump_no_drop` is the one that measures the jump.
- **Name the cost before optimising it.** On `cold_jump_back/1024` the arena
  scan was named first and mattered least: the `BTreeMap` seq index bought -9%,
  the `Rope::is_instance` fast path in `set_node_state` bought -62%. A
  full-document `PartialEq` on every history step dwarfed an O(N) scan over a
  slab of small structs.
- **Workspace grep cannot prove a published crate or public API has no external
  consumers — grep the sibling projects too.** `styled_spans` read as write-only
  across all of `hjkl` and was queued for deletion on that basis; `sqeel-tui`
  pins published `hjkl-engine` and reads it in three places. The check that
  settles it is `grep -rn <symbol> ~/Projects/kryptic-sh/ --include=*.rs`
  excluding this repo, and even that only covers what is checked out locally.
  Same class as the `hjkl-css` revert.
- **A green local run is not a green CI run.** Local checks are Linux-only, so
  anything platform-shaped passes locally and fails on the matrix. A
  `#[cfg(unix)]` test that created a non-UTF-8 filename sat red on macOS (APFS
  rejects the name with `EILSEQ`) across ~15 commits during the 0.40.0 work,
  because every slice was verified locally and pushed without checking the run.
  Check `gh run list` after pushing, not only before releasing.
- **`gh run list --commit <sha>` matches nothing unless the SHA is full.** A
  short SHA returns an empty list, which is indistinguishable from "no run has
  started yet" — a poll loop written on it waits forever while the run finishes
  and fails. Filter client-side instead:
  `gh run list --limit 5 --json headSha,databaseId,status,conclusion`. This
  matters precisely because it defeats the trap above: the check that is
  supposed to catch a red CI run reports green-by-absence.
- **`cargo fuzz run <target> <dir>` does not replay a directory — it seeds a
  campaign from it and starts fuzzing.** Passing a single FILE runs that input
  once; passing a directory reads it as a corpus and mutates. Add `-- -runs=0`
  when the intent is to check a set of known inputs. Getting this wrong reads as
  "the artifacts still crash" when what crashed is something the fuzzer just
  invented.
- **`:normal!` aborts the rest of its keys on a failed motion.** Probing nvim
  with one `-c 'normal! …'` per case makes a failed first motion look like "nvim
  left the buffer unchanged", i.e. like agreement with a hjkl no-op. Split the
  probe into separate `-c 'normal! …'` calls, or drive it through `nvim_input`.
- macOS and Windows are the two platforms local work never exercises: filename
  encoding, path separators, and symlink permissions all differ there. Gate on
  the capability (probe and skip) rather than on `cfg(unix)`, which includes
  macOS.

## 5. Supporting evidence

These appendices preserve the reproductions, design constraints, and audit
method needed to complete the open work above.

### Differential audit against neovim — method

Randomised differential fuzzer over the existing oracle infrastructure: generate
a random ASCII buffer and a random normal-mode keystroke sequence, replay both
through `hjkl_driver::run_case` and `nvim_driver::run_case`, diff buffer /
cursor / mode / default register. Divergences are greedily shrunk (drop one
keystroke token at a time, then one buffer line at a time, while the divergence
survives) and printed as paste-ready corpus TOML.

- `crates/hjkl-compat-oracle/examples/difffuzz.rs` — the fuzzer.
- `crates/hjkl-compat-oracle/examples/dfcase.rs` — replay one ad-hoc case
  through both engines, for narrowing a shrunk case by hand.

```
cargo build -p hjkl-compat-oracle --release --examples   # BOTH, see note
cargo run -p hjkl-compat-oracle --release --example difffuzz -- 400 777
cargo run -p hjkl-compat-oracle --release --example dfcase -- '<buf>' <row> <col> '<keys>'
```

Build with `--examples`, not `--example dfcase`. Rebuilding only one leaves the
other stale, and a stale `difffuzz` silently reports the previous commit's
divergences — byte-identical results across a run are the tell.

Both drivers pin `shiftwidth=4`, `expandtab`, `noautoindent`,
`foldmethod=manual`, so a divergence means an engine defect rather than config
skew between hjkl and `nvim --clean`.

#### Not covered by the fuzzer

- **Ex commands** (`:`) — the in-process hjkl driver cannot replay them; they
  are fuzzed separately by `examples/exfuzz.rs` (in-process `hjkl_ex` against
  nvim RPC, 2026-08-04).
- The app / window layer, LSP, and everything above the engine.

Search (`/`, `?`, `n`, `N`), folds (`zf`/`zc`/`zo`/`zR`/`zM`) and `gq` ARE
covered as of 2026-08-04: the vim FSM resolves the search prompt in-process, the
engine applies fold ops to the in-tree view, and `textwidth` is pinned per-case
for `gq`. Undo / redo are covered too — the nvim driver clears its undo history
after RPC seeding (the seed was one undoable change), so `u` / `<C-r>` diff
against empty undo trees on both sides.

Non-ASCII is covered as of 2026-08-04: `nvim_driver` converts between nvim's
byte columns and the corpus's char columns in both directions, so `tier1.toml`
carries a wide-character group. The char-vs-grapheme divergence it surfaced is
in §1.8.

### Cursor moves carry their own curswant semantics (2026-07-27)

#### Why

Moving the cursor and maintaining `sticky_col` (vim's `curswant`) are two
separate actions today, and nothing forces the second:

| primitive           | non-test sites | maintains curswant? |
| ------------------- | -------------- | ------------------- |
| `buf_set_cursor_rc` | 106            | **no**              |
| `View::set_cursor`  | 80             | **no** (documented) |
| `jump_cursor`       | 107            | yes (`= col`)       |
| `set_sticky_col`    | 33             | manual follow-up    |
| `apply_sticky_col`  | 1              | the vim catch-all   |

That is ~186 cursor moves which do not maintain curswant, each a potential
instance of the bug fixed in `c022a3a4`: `/pattern<CR>` moved the cursor without
resetting curswant, so the next `j` snapped back to the pre-search column.

The rule was never unknown — it is written correctly in two places already
(`apply_sticky_col`: "Everything else — search, gg/G, word jumps — lands at the
match's own column"; `jump_cursor`: "every explicit jump … search hit, click").
It was **unenforced**. Four call sites reached the search advance and only the
one routed through the vim motion dispatch was right.

#### Target shape

The selected design makes curswant semantics part of every cursor move:

```rust
pub enum Move {
    Vertical   { row: usize },              // j/k: READS curswant, clamps, leaves it
    Jump       { row: usize, col: usize },  // search/gg/G/marks/click: SETS curswant
    Horizontal { col: usize },              // h/l/w/b/$/^: SETS curswant
    Raw        { row: usize, col: usize },  // must NOT disturb curswant
}

impl Editor { pub fn move_cursor(&mut self, m: Move); }
```

The reserved end state for raw primitives is crate-internal use, keeping
`buf_set_cursor_rc` and `View::set_cursor` unreachable from vim/app code. This
explains why every migrated motion requires an explicit variant.

`Raw` is deliberately conspicuous. Today the forgetful option is the one that
looks like the default; naming it inverts that, and it stays greppable for
review.

#### What the debug invariant found

The unconditional assertion the plan called for is **not enableable**: it
produces 236 violations across 158 tests. The shipped check is therefore scoped
to plain motions — no chord/count in flight, `dirty_gen` unchanged, mode
unchanged across the key, no search prompt open. Every guard is structural, not
a suppression list, so a motion added later is covered the day it is written.

Two design calls made during implementation, both kept:

- **State-based, not key-based.** Classifying the key is unsound: `j` is
  vertical bare, a target in `dj`/`fj`/`rj`, and a literal in Insert. The check
  instead tests the states the rules can produce — after a move `sticky_col`
  must be `None`, equal to the landed column, or greater with the cursor clamped
  to the row end.
- **Fires on the transition, not the state.** It triggers only when a keystroke
  goes from a legal pair to an illegal one, so stale state from an earlier key
  is not blamed on the next movement.

Violation classes still relevant to the §1.6 migration:

| Class | Count | What                                                                                                                                                           |
| ----- | ----- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A     | 170   | Insert mode: every printable char, `Tab`, `<C-w>`. One systemic site.                                                                                          |
| B     | 66    | Normal-mode operators/edits: `y{motion}`, `d{motion}`, `J`, `p`/`P`, `x`/`X`, `~`, `>`, `.`, `u`, `<C-a>`. The operator path never reaches `apply_sticky_col`. |
| C     | 12    | Visual-mode `y` — invisible to a `dirty_gen` check since yank makes no edit.                                                                                   |

The ~186 sites expose the migration's classification cost. A mechanical
translation to `Raw` would compile while preserving the bug class.

## 6. Standing constraints

Decisions from completed work that still govern new work.

### `--embed` / `--nvim-api` conventions (2026-08-04, from `docs/embed-rpc.md`)

- **Buffer ids start at 1** and increment per buffer; a `Nil`, missing, or `0`
  handle means "current buffer". Enforced since 2026-08-04 (`bf05733d`) — the
  initial buffer's id is 1, never 0.
- **Window ids are 0-based indices** into the window table; id `0` is a real
  window and is NOT remapped to "current" — only `Nil`/missing means current.
- **Tabpage ids are 0-based indices**, not stable handles — they shift when a
  tab is closed.
- **Ext-type handles**: tag `0` = buffer, `1` = window, `2` = tabpage; the
  payload is the msgpack encoding of the id integer itself. Raw integers are
  accepted anywhere a handle is expected.
- **Wire framing**: bare msgpack values, no length-prefix framing; responses
  flushed after each message; EOF on stdin → exit code `0`. Notifications (no
  `id`) are dispatched but produce no response.
- **`nvim_buf_set_lines` / `nvim_buf_set_text` rebuild the buffer and reset undo
  history** — a deliberate divergence from nvim, documented in the method table.
- **`nvim_exec2` is not a vimscript interpreter**: it splits `src` on newlines
  and runs each non-empty line as one standalone ex command; output capture is
  unimplemented (`{"output": ""}`).
- **`nvim_get_mode` emits only the five modes the engine has** — `"n"`, `"i"`,
  `"v"`, `"V"`, `"\x16"`; `blocking` is always `false`.
- **`nvim_create_buf(listed, scratch)` ignores both arguments** — always makes a
  real buffer.
- **`nvim_set_keymap` honours only `noremap`** — `silent`, `expr`, `desc`,
  `nowait`, `unique`, `callback` are ignored; unknown mode strings fall back to
  the unprefixed `map` / `noremap`.

## 7. Record — closed efforts

Shipped or disproved work, kept so a later pass does not re-report it. Full
detail (repros, cited lines, per-item status) is in `git log` for the folded
files.

### 2026-08-04 code review (`docs/code-review.md`)

All 14 findings shipped the same day (commits `09fcf484`..`0e328c36`): ex
`$`/`%` phantom-row addresses, `ensure_cursor_visible` underflow on a stale
cursor, multibyte `n` skip, `nvim_get_current_buf` id-0 collision (buffer ids
now start at 1), `N` wrap at buffer byte 0, `w`/`W`/`e` off the phantom row, `s`
unnamed-register, visual-block `A` pad, `:s` mark rebase, inverted
`nvim_buf_get_text`, comment-marker span boundaries, filler-unaware indent
guides/diag overlays, markdown table width, `HexColorPass::apply_range` left
boundary. The three hardening items shipped with them: ex ranges and word
motions now share `content_row_count`, buffer ids start at 1, and the
`replace_all` call sites document the marks/jumplist/folds invariant. What the
review left open is §1.10.

Five candidate findings were disproved against real nvim and dropped:
`[count]iw` run-counting, `dgn` one-past-match-end, visual-block empty `I`/`A`
cursor placement, fuzzy-score fast path, and `hjkl-fs-watch` debounce truncation
— each verified to match or exceed nvim's behaviour.

### 2026-08-04 performance review (`docs/performance-review.md`)

All ten ranked findings shipped the same day (commits `5a644b11`..`ffb1d481`):
O(distance) wrapped scrolloff, borrowed rope rows in search, windowed
sentence/tag text objects, picker candidate caches, viewport-math rope borrows,
per-frame fold/sign/diag precompute, early-stop comment seed scan, fold-text
cache, idle-draw skip, per-window diag overlay. Six of the ten minor items
shipped with them; the four that remain are §1.11. Two confirmed-fine notes:
`SearchState::matches` invalidation is viewport-bounded and acceptable, and the
undo-tree / `content_joined` / `wrap_segments` / `line_bytes` / lock choices
were confirmed fine. The cost figures were traced from the code, not profiled —
the §4 Gates note that performance items require measured before/after results
still applies to the shipped fixes.

### `--embed` and `--nvim-api` (2026-08-02 design record, `docs/embed-rpc.md`)

Both phases of issue #26 shipped: `hjkl --embed` (JSON-RPC 2.0 over
stdin/stdout) and `hjkl --nvim-api` (msgpack-rpc with nvim-compatible method
names, a drop-in subprocess replacement for `nvim --headless --embed`). The
`hjkl-compat-oracle` `nvim_api_tier_passes` test drives the corpus through the
nvim-api path (`HJKL_ORACLE_NVIM_API=1`); `known_divergences.toml` is empty
because those cases graduated into their own tier. The spec's binding
conventions are recorded under Standing constraints.

### Backlog-work session 2026-08-04 (the items pruned from §1 above)

- **Default-scope `:g` / `:v` phantom row** (`2cc64cca`) — no-range scope now
  uses `hjkl_engine::motions::content_row_count` (made pub; hjkl-ex's inlined
  copy deleted), so `:g/^$/d` on `"a\nb\n"` deletes nothing. Three regression
  tests.
- **`:s` host records** (`3248f9aa`) — both `replace_all` paths emit one coarse
  whole-buffer Replace on the change log plus the content-reset flag, matching
  the `set_content` whole-buffer contract; syntax/LSP/diff consumers now see
  substitutes.
- **Per-row alloc cuts** (`c8dbf8b1`) — the four §1.11 minors: substitute match
  collector, paragraph/section/paren motions (`line_bytes` + rope-slice
  borrows), `buf_line_chars`, `search_matches` borrow. Measured −18%…−40% on a
  100k-line buffer.
- **SwapRoot** (`49e1c1bc`) — `hjkl_app::swap::SwapRoot` beside `TrashRoot`; App
  carries it, all swap-dir resolution routes through it, the last `EnvVarGuard`
  is deleted. `-r` keeps the Xdg default.
- **Shared `:set` host tokens** (`c8e76ea2`) — `set_tokens` module with a
  `SetHost` trait used by the TUI, `--headless` and `--embed`; mouse /
  explorer.open are no-ops in the non-TUI modes, and their `:set` staying
  session-only is now the stated decision (was "an implementation call").
- **Oracle harness** (`fb28145e`) — nvim's undo history is cleared after RPC
  seeding (difffuzz 777: 77 → 69 divergences, the `u` noise gone); new `exfuzz`
  example (19 ex shapes, 21 divergences at its seed); `/` `?` `n` `N` and
  `zf`/`zc`/`zo`/`zR`/`zM` + `gqq`/`gqj`/`gq}` enabled in difffuzz (search
  agrees, fold/gq surface real cursor divergences); probes in
  `tests/harness_probes.rs`. The 69 difffuzz / 21 exfuzz divergences remain open
  as parity work.
- **§1.6 phase 1** (`ca31a06a`) — motion dispatch onto `Editor::move_cursor`;
  `apply_sticky_col` deleted. Phases 2–5 remain.
- **Swap base dedup** (`0e4e2978`) — `SerTree.base` is `Option<String>`;
  single-node trees write `base: None` (the body IS the base) and the reader
  re-substitutes. Undo section 1.22 MB → 26 bytes on a 20k-line tree.
- **LSP borrowed full-doc sync** (`4838886f`) — `didOpen` / full `didChange`
  serialize straight from the shared `Arc` (`*_borrowed` params + a Serialize
  envelope), attach boundary carries the Arc; wire bytes unchanged, one
  full-document copy eliminated (~1.22 MB).

### 2026-08-05 code review fixes (the §8 findings)

All 15 code findings with a feasible fix shipped the same day, in six commits
(`0518ca77`, `af6747d3`, `2d8cf784`, `4323e39d`, `e62b7bd9`, `77c2be3a`):
confirm-substitute stale-offset guards (#1), case-op register/clipboard
preservation (#2), X11 chunked `read_property` (#3), bonsai raw-query install +
`.so`-keyed artifacts cache (#4/#18), Wayland stale-fd drain (#5), explorer
type-change fresh create (#6), exact linewise-delete inverses (#7), `\a`/`\A`/
`\Z`/`\e` + escaped-`]` translation (#8/#9/#10-half), `:0r` (#12), diag/cursor
cell mapping (#13/#14), fs-watch filter + rename-fabrication (#15/#16), anvil
finish-race (#17), macOS NUL mime (#20). Still open, tracked in §1.11:
`\d`/`\s`/ `\w` ASCII semantics (#11), `\ze`/`\zs` (#10 half), Windows
`EmptyClipboard` ordering (#19). What the review disproved is in §8's Cleared
section.

### Backlog-work session 2026-08-06

- **Undo `cap` O(N) (`4e3c2bf4`)** — `lowest_offpath_leaf` and `prune_root_side`
  read the maintained `UndoNode::on_path` flag instead of building a path `Vec`
  and testing membership (O(N\*depth) → O(N)); `UndoTree::current_path` deleted.
- **Blockwise word text objects (`2be058b8`)** — `iw`/`aw`/`iW`/`aW` keep
  `FsmMode::VisualBlock` and write `block_vcol` instead of collapsing to
  charwise; `<C-v>iw<` now reaches the blockwise shift arm. Seven corpus cases
  pinned against neovim 0.12.4; the remaining blockwise text-object divergences
  (paragraph/sentence/brackets/tags) are re-measured and tracked in §1.5b.
- **`)` sentence scan row-by-row (`8a7f3613`)** — `sentence_step_forward`
  stopped collecting the whole buffer; reuses a forward scan extracted from
  `sentence_boundary` (behavior unchanged there), 31.5 ms → 2 µs per `)` on a
  50k-line buffer. New differential test pins it against a full-buffer reference
  over the corpus. §1.7's other allocation bullets remain open.

### Backlog-work session 2026-08-07

- **RPC fs-confinement closed (`21ce803c`)** — `:e`/`:cfile` in
  `--nvim-api`/`--embed` now run the `embed.rs` gate (`check_fs_path` +
  `resolve_under`), and `:cd` is refused while `fs_restricted()`; the §10 #1
  HIGH (read of arbitrary paths / write escape via `:cd`) is gone. nvim-api
  integration tests spawn the binary in a tempdir (absolute-path + symlink
  escape refused, inside path still opens); fs_policy pins the `:cd` refusal.
- **Remote grammar fetch off in RPC (`d89a4332`)** — `LanguageDirectory` gained
  `set_allow_remote(false)`; `App::new` applies it when `fs_restricted()`, so a
  true miss resolves to `Unknown` instead of clone+compile+dlopen (§10 #2).
- **§9 review findings shipped (`7f06c4c7`, `aab0cb67`)** — `:retab` clamps a
  `ts=0`-sourced tabstop (no more divide-by-zero after a modeline); literal
  numeric range addresses past EOF error E16 when a command follows (`:5d` no
  longer deletes line 3; bare `:N` goto still clamps — nvim-verified);
  `hjkl-tabs` `TabBar::close` keeps focus when a non-active tab closes. The
  pre-existing `range_followed_by_command` test pinned the old clamp and was
  updated to assert E16.
- **§11 tidy sweep shipped (`0eb9f2f8`)** — all 15 findings; §11's #16a
  (`Tracker::reset` in `mouse.rs`) deliberately kept per the author's note. One
  intended behavior change: prompt `command_line_is_runnable` now uses the ASCII
  word class via `command_word_range`.

### Backlog-work session 2026-08-07 (evening — the filetype + backlog pass)

- **Filetype detection for extensionless files (`8abf957c`, `a1a62ebb`)** —
  `LanguageDirectory::detect` seam (known basename → extension → modeline `ft=`
  → shebang), bounded modeline scan (first/last 5 lines × 500 chars, later-lines
  win, nvim-verified), live `:set filetype=` re-attaches grammar + LSP. See the
  changelog.
- **Row-range reads in the vim ops (`f2b5661f`)** — all ten `rope_to_lines_vec`
  sites (block case/char-replace, reflow, indent family) now read/write only the
  touched rows; 57.7 ms → 0.125 ms block case-op on a 100k-line buffer, peak RSS
  43.1 → 27.5 MB. See §1.7.
- **Change log opt-in (`1f062fe0`)** — the per-edit `Vec<EngineEdit>` had no
  consumer in hjkl or any checked-out sibling (all `take_changes` sites are
  tests); recording now defaults off and `mutate_edit` skips it entirely. Peak
  RSS 651 → 456 MB on the paste path. See §1.5b.
- **Fold pass cache-only grammar resolution (`45aab1cc`)** — injected folds use
  the new `LanguageDirectory::by_name_cached`, so a fold pass never triggers a
  clone+compile+fetch. See §1.4b.
- **`from_serializable` clears a `last_child` that names a non-child
  (`9dfd143a`)** — a hand-edited undofile's redo direction can no longer walk
  into the wrong subtree on the first `<C-r>`; repaired, not rejected. See §1.1.
- **Corpus pins expectations without nvim (`6cddabde`)** — `run_single` compares
  hjkl against the authored `expected_*` values when nvim is absent; 75/75
  corpus cases pass via the fallback with nvim hidden from PATH. See §1.5b.
- **vim ASCII `\d`/`\s`/`\w` (`cf83b533`)** — translator emits vim's ASCII
  classes (and literal `{\, letter}` inside `[...]`); the shipped `[\a]`/`[\A]`
  "alphabetic range" translation was disproved by nvim 0.12.4 and corrected. See
  §1.11.
- **Blockwise bracket/tag objects are no-ops (`ddc81797`)** — `ib`/`ab`/`iB`/
  `it` in `<C-v>` no longer collapse the block; nvim keeps the selection as the
  block motion made it. Four corpus cases pinned against neovim 0.12.4. See
  §1.5b.

## 8. 2026-08-05 code review (audit depth)

Scope: whole workspace (~260k lines, 57 crates + app), clean tree. Method:
read-only sub-agent sweeps per crate group, every candidate re-traced against
the real code before inclusion. Verified against the installed dependency
sources (`regex-syntax` 0.8.10, `ropey` 1.6.1, `tree-sitter` 0.26.11) and the
X11 protocol spec. Windows/macOS code (`windows.rs`, `macos.rs`) was code-read
only — not compiled on this Linux host.

### Findings — ranked

#### 1. HIGH — confirm-substitute stale match offsets panic the editor

`apps/hjkl/src/app/confirm_substitute.rs:144-145` and
`apps/hjkl/src/render.rs:1747-1749`. `jump_to_current_confirm_match` and the
per-frame highlight read the live buffer with the match's stale `row` /
`byte_start`: `rope_line_str(&rope, r)` panics for `row >= len_lines()` (ropey
contract, stated at `hjkl-buffer/src/buffer.rs:845`), and `line[..byte_start]`
panics on a stale or mid-char byte offset. The sibling first-jump is guarded
(`ex_dispatch.rs:775-783`, whose comment names the hazard); these two sites are
not.

Repro (no keypress needed): 5-line file with ≥2 matches; `:s/a/x/c`; from
another terminal `echo x > file`. `autoreload` defaults true
(`hjkl-engine/src/types.rs:494`), fs-watch is on (`main.rs:841`),
`drain_fs_watch_events` runs every tick (`event_loop.rs:383`) and reloads the
clean buffer without clearing `confirming_substitute`
(`ex_dispatch.rs:2417-2478`). Expect: prompt keeps working. Actual: on the next
draw the highlight pass reads row 4 of a 1-row rope —
`panicked at line_to_byte: byte index out of bounds`, process aborts. Variant B:
mid-confirm mouse-click another buffer line (`event_loop.rs:1589-1593`, mouse
not gated on the session) then `y`.

#### 2. MEDIUM — case operators (`gU`/`gu`/`g~`/`g?`/`gUU`/visual `U`/`u`/`~`/`gn`) clobber the OS clipboard and registers

`crates/hjkl-vim/src/vim/text_object_ops.rs:276-353` +
`crates/hjkl-vim/src/vim/command.rs:144-146`. The op transforms the range via
`cut_vim_range`, which calls `record_yank_to_host` (→ `Host::write_clipboard`,
`hjkl-engine/src/editor.rs:1757-1759`) and `record_delete` (writes unnamed,
shifts the `"1`–`"9` ring or writes `"-`, and writes any pending named target —
`hjkl-engine/src/registers.rs:141-176`). Only the unnamed slot is saved and
restored (`text_object_ops.rs:285-286,350-351`); the code's own comment says
"vim's case operators don't touch registers".

Repro: buffer `hello world\n`; `y$` (clipboard = `hello world`); `0` `gUw`.
Expect: clipboard and `"-` unchanged (vim). Actual: clipboard = `hello`, `"-` =
`hello`; linewise forms shift `"1`–`"9` and a pending `"x` prefix writes
register `x`. Every case-op caller routes through this path (`op_motion.rs:455`,
`linewise.rs:140`, `visual_ops.rs:108/210`, `operator.rs:235`,
`range_ops.rs:101`).

#### 3. MEDIUM — X11 clipboard read silently truncates oversized non-INCR properties

`crates/hjkl-clipboard/src/backend/x11_thread.rs:1162-1212` (used from `do_get`
at `:1267-1271`). `xcb_get_property` is issued once with
`long_length = u32::MAX/4`; for a property larger than the server's max request
length the reply carries the first chunk and `bytes_after > 0`. `bytes_after` is
never checked (grep: the field appears nowhere in the crate outside the struct
def), so the truncated chunk is returned as a successful `Ok`. (The sub-agent's
"delete destroys the remainder" half is wrong — per the X11 protocol spec,
`delete` only fires when `bytes_after == 0`, so the remainder survives but is
never read.)

Repro: owner writes a 256 KB non-INCR property via append-mode `ChangeProperty`;
`get(Clipboard, Text)`. Expect: full payload or an error. Actual: `Ok` with the
first ~max-request-length chunk. INCR transfers are unaffected (4-byte hint
property).

#### 4. MEDIUM — grammar install strips capture-form `(#set! @cap ...)` directives, silently killing the pre-extraction feature

`crates/hjkl-bonsai/src/runtime/loader.rs:320-333` +
`crates/hjkl-bonsai/src/query_sanitize.rs:357-364` +
`crates/hjkl-bonsai/src/highlighter.rs:1933-1935`. The installer writes the
sanitized query to `<name>.scm` (what `Grammar::load` reads,
`runtime/grammar.rs:95-101`), and the sanitizer deletes every
`(#set! @cap key @cap2)` form. The highlighter's `compile_query` only
pre-extracts those directives when `Query::new` fails on the raw text —
tree-sitter 0.26.11 rejects the two-capture form ("Unexpected second capture
name", `binding_rust/lib.rs:2897-2905`) — but the sanitized text compiles
first-try, so `pre_extracted` is always empty for installed grammars.

Repro: install markdown_inline or html_tags; highlight a URL attribute. On-disk
evidence:
`~/.cache/bonsai/query-sources/…-markdown_inline.resolved.scm:45,49,96` carry
`(#set! @_url url @_url)`; the installed
`~/.local/share/bonsai/grammars/markdown_inline.scm` has none of them. Expect:
span `metadata["url"]` (asserted by the `html_set_directive_metadata_applied`
test, `highlighter.rs:2276-2316`). Actual: all spans have `metadata: None`.

#### 5. MEDIUM — Wayland: an in-flight `source.send` fd for a replaced source is misattributed to the next paste

`crates/hjkl-clipboard/src/backend/wayland_thread.rs:850-878` + `do_set`
`:1284-1333`. `dispatch_events` pops an fd only when the message's object id
matches the _current_ `clipboard_source`; a send event for a source destroyed by
`do_set` (same thread, between dispatch cycles) is dispatched with
`opt_fd = None` and its fd stays in the FIFO `rx_fds`
(`wayland_socket.rs:96-97`). The next paste pops the stale fd.

Repro: paste (compositor queues `source.send(S1, fd1)`) racing a `set()` before
the bg thread dispatches it. Expect: each paste gets its own fd. Actual: the
paste writes the payload into the previous generation's pipe (stale fd); the
queue is permanently shifted one generation, so every later paste fails or
desyncs. A blocking write on a stale undrained pipe can stall the bg thread.

#### 6. MEDIUM — explorer file↔dir type change at an unchanged path silently reverts

`apps/hjkl/src/app/explorer_reconcile.rs:226-234` (emits `Trash(path)` +
`CreateDir/CreateFile(path)` in one batch) + `:486-515` (Trash pushes
`(basename, dest)` into the registry) + `:528-554` / `:562-598` (create ops
restore by basename only, with no original-path comparison). `apply_ops` runs
trashes before creates (`:280-284`), so the type-change create finds the entry
the same batch just trashed and moves the old _file_ back; the buffer is then
rebuilt from disk (`explorer.rs:1221`), discarding the user's edit silently.

Repro: explorer on a dir containing `a.txt`; edit the line to `a.txt/` and press
`<Esc>`. Expect: file trashed, empty dir `a.txt/` created. Actual: file restored
unchanged, tree shows a file, no error.

#### 7. LOW — linewise-delete inverse text is wrong for non-`\n` line separators

`crates/hjkl-buffer/src/edit.rs:573-581` (`rope_line_str_locked` strips only
`'\n'`), used by `do_delete_range`'s `MotionKind::Line` arm (`:298-396`).
Ropey's `line()` includes the full separator (`get_line`, ropey 1.6.1), so for
`\r`, U+000B, U+000C, U+0085, U+2028, U+2029 the removed span is stripped
correctly but `removed_joined` (`:347-352`) carries a phantom `'\n'` — the
inverse `InsertStr` re-inserts one extra line break, violating the documented
`apply_edit` round-trip contract (`:133-135`). CRLF is safe (the `\r` is
stripped and restored); the round-trip proptest corpus only generates ASCII.

Repro: buffer `a\u{2028}b`; `dd` at (0,0). Removed span = `a\u{2028}` (2 chars).
Expect: undo restores `a\u{2028}b`. Actual: inverse text `a\u{2028}\n` restores
`a\u{2028}\nb` — a line break materializes.

#### 8. LOW — vim pattern escapes `\a`/`\A` pass through as rust-regex escapes with different meanings

`crates/hjkl-engine/src/search.rs:292-298` (the passthrough arm — also every `/`
search, `:s`, `:g` pattern). The arm's comment claims `\a`/`\A` are "already
valid rust-regex syntax … and identical in vim's default magic" — false: vim
`\a` = `[A-Za-z]`, `\A` = non-alpha; rust-regex `\a` = Bell
(`regex-syntax parse.rs:1551`), `\A` = start-of-text anchor (`:1557-1560`).

Repro: `:s/\A/x/` on `"ab1"`. Expect (vim): `"abx"`. Actual: `\A` matches the
empty string at position 0, `do_replace` (`substitute.rs:856-875`) inserts →
`"xab1"` — text changed in a way vim never produces. `:s/\a/x/g` silently no-ops
(no bell chars).

#### 9. LOW — escaped `]` inside a character class closes it early → "bad pattern"

`crates/hjkl-engine/src/search.rs:258-264`: the in-bracket branch pushes `]`
verbatim and clears `in_bracket` without checking the preceding `\`, so an
escaped `]` terminates the class.

Repro: `:s/[a\]b]/x/` on `"a]b"`. Expect (vim): class `{a, ], b}`, substitution
runs. Actual: translated pattern `[a\]b\]` — rust-regex reports "unclosed
character class" and the command errors, doing nothing.

#### 10. LOW — vim `\Z`/`\e`/`\z`-family mis-translate

`crates/hjkl-engine/src/search.rs:292-298`. Vim `\Z` (ignore case) and `\e`
(ESC) pass through; rust-regex has neither →
`bad pattern: unrecognized escape sequence` (`regex-syntax parse.rs:1594`),
where vim substitutes fine. Vim `\ze`/`\zs` (start/end of match) pass through as
rust's end-of-text anchor `\z` + literal — compiles, matches nonsense or
nothing.

Repro: `:s/\Zfoo/bar/` on `"FOO"`. Expect (vim): `"bar"`. Actual: `bad pattern`
error, no change.

#### 11. LOW — `\d`/`\s`/`\w` are Unicode-wide in rust-regex, ASCII-only in vim

`crates/hjkl-engine/src/search.rs:292-298`. Vim `\d` = `[0-9]`; rust `\d` =
`\p{Nd}`. Verified: rust `\d` matches U+0663.

Repro: `:s/\d/x/` on `"٣"`. Expect (vim): no match, unchanged. Actual: `"x"`.

#### 12. LOW — `:0r file` inserts after line 1; vim inserts before line 1

`crates/hjkl-ex/src/builtins.rs:176-179` (`read_handler`), with
`crates/hjkl-ex/src/lib.rs:88-100` (the `:0put` special case exists precisely to
rescue 0-address insertion-point commands; `:r` is not in its list) and
`range.rs:196` (`clamp(1, last)` on a literal 0).

Repro: buffer `"a\nb"`, `:0r x.txt` (file content `"X\n"`). Expect: `"X\na\nb"`.
Actual: `"a\nX\nb"`.

#### 13. LOW — LSP diag overlay paints char columns as screen cells, ignoring tabs and wide chars

`crates/hjkl-buffer-tui/src/render.rs:1100-1114`. `DiagOverlay` maps
`col - top_col` straight to a cell; no tab expansion, no wide-char width. The
diagnostic columns are chars end-to-end (`lsp_glue.rs:487-504` →
`build_diag_overlays`), so any line with a tab or multibyte char before the
range underlines the wrong cells.

Repro: line `"\tfoo"`, `tab_width = 4` (cells: tab 0-3, `foo` 4-6); diag range
chars 1..4. Expect: underline on cells 4-6. Actual: underline on cells 1-3
(inside the tab).

#### 14. LOW — past-EOL cursor placeholder uses a char offset as a cell offset

`crates/hjkl-buffer-tui/src/render.rs:1789-1804`. `dx = cursor_col - seg_start`
is a char index, painted at `area.x + dx` as a cell; on a line containing a wide
char the reversed cursor cell lands on the last character instead of one cell
past it. The correct conversion exists in the same crate
(`char_col_to_visual_col`, used at `render.rs:939`).

Repro: line `"你x"` (3 cells), cursor at (0,2) past-end (the state the crate's
own tests exercise — with ASCII, so they pass). Expect: placeholder at cell 3.
Actual: cell 2 — covers `x` (or half of a trailing wide char).

#### 15. LOW — fs-watch rename `To`-merge bypasses the path filter

`crates/hjkl-fs-watch/src/lib.rs:451-476`. The `From` arm applies
`passes_filter` (`:443-445`); the `To` merge branch (`:460-468`) inserts
`Renamed { to }` with no filter check — only the no-`From` fallback filters.

Repro: filter `extension == "rs"`; `rename("a.rs", "b.txt")`. Expect: `a.rs`
events at most. Actual: `Renamed { from: "a.rs", to: "b.txt" }` — a filtered-out
path delivered as `to`; a consumer acting on `to` touches an unwanted file.

#### 16. LOW — fs-watch `Create` merges with an unrelated pending `RenameFrom`, fabricating a rename

`crates/hjkl-fs-watch/src/lib.rs:490-513`. The merge keys on recency
(`max_by_key(v.at)`), not a kernel cookie; a `Create` landing inside the
debounce window of an unmatched `RenameFrom` pairs them.

Repro: `rename("old.txt", "other.txt")` (From pending, To delayed past the
debounce boundary); 50 ms later an unrelated `create("fresh.txt")`. Expect (doc
promise, `:153-154`): separate events. Actual:
`Renamed { from: "old.txt", to: "fresh.txt" }` — a pair that never happened; the
real `To` then arrives as `Created(other.txt)`.

#### 17. LOW — anvil install dedup race reports `Failed` for a succeeded install

`crates/hjkl-anvil/src/job.rs:173-176` vs `:77-103` / `wait` `:142-150`. The
worker's terminal `broadcast` and `in_flight.remove` are separate lock
acquisitions; a caller attaching a sender in the window between them observes a
closed channel and `wait()` returns `Failed("<channel closed>")`.

Repro: three threads (UI + LSP + anvil worker); caller B calls `install(name)`
for a name whose worker just broadcast `Done` but not yet removed. Expect:
`Done`. Actual: `Failed("<channel closed>")` — a caller that re-queues on
`Failed` starts a duplicate install of an already-installed tool. Window is
microseconds but real.

#### 18. LOW — bonsai compiled-artifacts cache ignores the grammar `.so` identity

`crates/hjkl-bonsai/src/highlighter.rs:384-393` (key = name + highlights +
injections content only) + `:497-509` (global `COMPILED_CACHE`). A same-named
grammar from a different `.so` with unchanged query content (a grammar-only rev
bump) reuses a `Query` compiled against the old symbol table; tree-sitter
matches patterns by numeric symbol id with no language check, so spans change
arbitrarily with no error. The cached `Query` also holds a shallow copy of the
old language — a latent dangling pointer after the last old `Grammar` drops, not
dereferenced in release builds.

Repro: same process, two `rust` grammars with identical `.scm` built from
different revisions; highlight the same buffer with both. Expect: identical
spans. Actual: arbitrary span differences, no diagnostic.

#### 19. LOW — Windows: `EmptyClipboard` runs before allocation, wiping prior clipboard on failure

`crates/hjkl-clipboard/src/backend/windows.rs:568-580` (`set_png`; same shape in
`set_text` `:257-266` and `set_bytes` `:372-381`). If `GlobalAlloc` (or the
second format's `SetClipboardData`) fails after `EmptyClipboard`, the old
clipboard content is already destroyed. Allocating first and emptying only
immediately before `SetClipboardData` preserves it. cfg-gated; code-read only on
this host.

#### 20. LOW — macOS: NUL byte in a custom mime type panics the calling thread

`crates/hjkl-clipboard/src/backend/macos.rs:248`
(`CString::new(s).expect("NUL byte in clipboard type string")`);
`MimeType::Custom` passes the string through verbatim (`:330`). X11 and Wayland
handle the same input fine (length-based); Windows rejects it. cfg-gated;
code-read only on this host.

### Cleared

- **`hjkl-fs` atomic-write fallback dropping `preserve_mode`** — disproved.
  `File::create` on an _existing_ target reuses the inode, so the mode survives
  O_TRUNC; no caller combines `mode: Some(..)` with `nonatomic_fallback` (only
  `document()` does, with `mode: None`); the `CrossesDevices` fallback is
  unreachable because `temp_path` is always a sibling (`atomic.rs:112-123`).
- **X11 `delete=1` destroying the unread remainder** — disproved. Protocol spec:
  the property is deleted only when `bytes_after == 0`; the remainder survives
  (but is never read — that half is finding #3).
- **`shell.rs` undo leak on failed `:%!cmd`** — `push_undo()` runs after every
  error return; a failed filter leaves no undo entry.
- **Substitute `parse_flags` digit handling, huge counts,
  `chars.next().unwrap()` at `substitute.rs:702`, last-changed-row mapping** —
  all traced, guarded or clamped; no panic, no wrong-line reach.
- **Undo arena tree (diff/apply/keyframes/retarget/cap/prune/pop/by_seq)** —
  differential-tested against a full-snapshot model; char-boundary snapping in
  `diff` is sound.
- **Buffer cursor clamps, `floor_char_boundary`, wrap-scroll subtraction,
  jumplist bounds, insert-bridge autopair/soft-tab arithmetic** — safe.
- **`apply_collected_matches` staleness guards** (`substitute.rs:611-626`) —
  skips stale matches; the confirm path's _display_ is the hole (finding #1).
- **Swap/trash/nvim_api paths** — swap lengths capped, atomic temp+rename; trash
  reservation atomic; nvim_api clamps untrusted row/col params,
  char-boundary-snaps byte offsets, budgets the RPC loop.
- **Explorer path injection** — `escape_ex_path` round-trips `%`/`#`; `|` is not
  a command separator in this ex parser.
- **Confirm-session `idx` bounds and key routing** — `idx < matches.len()` while
  the session lives; keys fully consumed during confirm.
- **LSP byte/char conversions** (`col_to_wire`/`wire_to_col`), framing
  (Content-Length caps), UTF-16/UTF-8 gating — correct; the renderer's cell
  mapping is the defect (finding #13).
- **anvil atomic-install sequence, `safe_join`, URL interpolation, TOML sidecar
  escaping** — all validated.
- **Bonsai rope-provider lifetimes, parse-callback chunk lifetimes, `#offset!`
  handling, `dedup_by` comment merge** — traced against tree-sitter 0.26.11;
  safe.
- **Ropey CRLF handling in the linewise-delete inverse** — `\r\n` round-trips
  (the `\r` is stripped and restored); only non-`\n` separators break (finding
  #7).

### Hardening

- `set_yank` (`hjkl-engine/src/editor.rs:2070-2078`) rebuilds the unnamed slot
  with `..Default::default()`, dropping `blockwise`/`block_width` even on a
  correct restore.
- `ex_dispatch.rs:775-783` is the guarded pattern finding #1's two sites should
  mirror (clamp row, `.get(..byte_start)` fallback), or the session should be
  dropped on buffer reload/switch.
- X11 INCR send uses `CW_EVENT_MASK` in _replace_ mode on the requestor's window
  (`x11_thread.rs:729-739`), clobbering their event selection.
- `wayland_socket.rs`: a message advertising a huge `hdr.size` never drains
  (unbounded `rx_buf` growth); `MAX_FDS_PER_RECV = 8` silently drops extra fds
  (`MSG_CTRUNC`); `sendmsg` on a blocking socket can block.
- `escape_ex_path` (`ex_dispatch.rs:3216-3218`): a filename containing the
  literal bytes `\%` or `\#` is mis-targeted (vim has the same ambiguity).
- anvil backup-path collision: a tool named `foo.bak` collides with `foo`'s
  backup path (`installer.rs:218-241`); unreachable via the manifest (dots
  excluded) but reachable via the public install API.
- `FormatWorker::drop` (`hjkl-mangler/src/lib.rs:1001-1013`) joins the worker,
  which can block teardown up to `FORMAT_TIMEOUT` (30 s) on a hung formatter.
- Bonsai `extract_capture_set_directives` pattern-index off-by-one for
  directives placed after their pattern's closing paren
  (`query_sanitize.rs:128-132`); latent — current pinned queries place them
  inside the parens, and finding #4 currently prevents the extractor from
  running on installed grammars at all.
- `shift_byte` row-delta semantics (`folds.rs:752-788`): a nonzero row delta on
  a node starting at column ≠ 0 would diverge from tree-sitter's `#offset!`.
- Cached bonsai `Query` holding a shallow copy of an unloaded grammar's language
  — latent dangling pointer, not dereferenced in release builds.

### Coverage

Reviewed (all findings above re-traced by the reviewer against the cited lines
and the installed dependency sources): hjkl-buffer, hjkl-vim, hjkl-engine
(search/substitute/registers/buf_helpers/editor hot paths), hjkl-ex
(parse/range/shell/global/expand/complete + builtins read/write handlers),
apps/hjkl (confirm_substitute, ex_dispatch regions, event_loop regions,
explorer + reconcile, render highlight/diag regions, nvim_api regions, headless,
embed, save), hjkl-app (swap, trash, git), hjkl-clipboard (backends: x11_thread,
x11, wayland_thread, wayland_socket, wayland_wire, windows, macos, dlopen,
dib_png, uri), hjkl-fs (atomic, dir, identity, path, read), hjkl-fs-watch,
hjkl-bonsai (highlighter, folds, comment_markers, hex_color, runtime/\*,
query_sanitize, rainbow), hjkl-anvil, hjkl-buffer-tui (render), hjkl-lsp,
hjkl-mangler, hjkl-quickfix, hjkl-layout, hjkl-keymap, hjkl-menu, hjkl-css
(partial), hjkl-picker (partial), hjkl-fuzzy, hjkl-compat-oracle (partial),
hjkl-config (validate), hjkl-xdg, hjkl-kitty, hjkl-markdown (partial).

Not reviewed (GAP): the ~30 small TUI-shell crates (theme-tui, statusline-tui,
splash-tui, holler-tui, etc.) skimmed for structure only; the full bodies of
apps/hjkl
`app/{buffer_ops,mouse,fs_watch,keymap,keymap_build,count_prefix, chord_routing,diff_mode,diff,dispatch,dock,engine_actions,window, viewport_sync,syntax_glue,prompt,quickfix,types}`
and `host.rs`/`theme.rs`/ `main.rs`; `nvim_api.rs` 1126-1545 and 1785-2336;
hjkl-app
`{config,filestate,git_worker,keymap_actions,modeline,picker_git, picker_sources,undofile}`;
hjkl-engine editor.rs large spans (500-1780, 2000-2430, 2860-3624, 3730-4385,
4430-4880, 5000-7582) and
motions/types/viewport_math/selection_shift/tag/input/options_registry/
abbrev/discipline/policy; hjkl-ex `builtins.rs` 300-630 and 2210-4970
(quickfix/comment/retab), setopt/folds/listings/effect; hjkl-clipboard
`{osc52,base64,mime,selection,error,mock,ssh_aware}`; hjkl-fs
`{dirs,lock,open,project}`; the bundled tree-sitter query files' contents; crate
test/bench harnesses. Windows (`windows.rs`) and macOS (`macos.rs`) code was not
compiled on this host — findings #19/#20 are code-reading only.

Summary: 18 confirmed findings — 1 high, 5 medium, 12 low. Overall risk is
moderate: one process-abort reachable with no keypress (finding #1), one silent
user-data clobber (#2), one silent clipboard truncation (#3). Fix order: (1)
guard or drop the confirm session on buffer reload/switch; (2) stop routing case
ops through the delete funnel (or snapshot/restore clipboard + registers); (3)
check `bytes_after` in `read_property`.

## 9. 2026-08-10 code review — whole workspace (correctness)

Scope: clean tree, whole workspace (~264k lines, 57 crates + app). Method:
read-only sub-agent sweep, every candidate re-traced against the real code
before inclusion.

### Findings

**None confirmed.** The highest-risk paths (ropey index arithmetic, ex/vim input
parsing, search/substitute, undo deltas, LSP framing, swap-file parsing,
fs-watch debounce, atomic writes, tree-sitter range handling) were traced; no
defect satisfied the verification contract (guarded, reachable, traced to
return, with an expressible repro). Candidates that failed the trace are under
Cleared.

### Hardening (correct today, fragile by design)

- **fs-watch `RenameMode::To` pairing** (`hjkl-fs-watch/src/lib.rs:453-458`) —
  the merge picks the most recent pending `RenameFrom` globally; interleaved
  renames in one debounce window can mis-pair. Unreachable on Linux (inotify
  emits `Both`/cookie-matched pairs); a per-path pending map would be more
  robust.
- **`apply_collected_matches` stale-match application**
  (`hjkl-engine/src/ substitute.rs:611-631`) — a match collected
  pre-buffer-change whose byte offsets still index valid (but different) text is
  applied at the wrong spot; only char-boundary violations are skipped.
  Interactive `:s///c` applies immediately so the window is tiny; the guard is
  best-effort.
- **LSP `pending` map growth** (`hjkl-lsp/src/server.rs:161-168,515`) — an
  unanswered request leaks one `(id, app_id)` entry, reaped only on response.
  Bounded by user-initiated requests; never reaped. Server-initiated
  `workspace/configuration` auto-answers prevent the common case.
- **`read_message` exact-limit boundary** (`hjkl-lsp/src/codec.rs:52-58`) — a
  header section of exactly `MAX_HEADER_BYTES` is rejected (needs to be strictly
  under); off-by-one in a DoS guard only, harmless.

### Cleared (suspected, disproved)

- `hjkl-fuzzy` `pct * PCT_SCALE` overflow — `pct ≤ 100` on every `Some` path
  (`needle_len ≤ hay_len`).
- substitute post-split `last_changed_row` arithmetic — verified numerically
  against the multi-row `\r` test.
- undo `diff()` suffix snap — `a_end` snapped to char boundary keeps `b_end` on
  one (equal trailing bytes); mid-char cases traced to correct deltas.
- `matching_bracket_pos` backward scan — `here_pos = c - i` exact after the
  reversed walk.
- word motions vs vim's `bck_word`/`fwd_word` — virtual-EOL cell semantics
  mirrored; phantom trailing rope row unreachable.
- `search_backward` skip-current byte-stepping — `pos_at_byte(cb-1)` always
  lands strictly before the match start; byte-0 wrap branch correct.
- tag multibyte slicing — callers convert char→byte before slicing; historical
  panic fixed.
- `insert_ctrl_d_bridge` byte/char mixing — outdent only strips ASCII, so byte
  == char over the stripped span.
- shell range filter indexing, `range.rs` number/offset parsing (E493 on
  backward, not swap) — both correct.
- LSP `read_message` unbounded-line attack — header budget + `take(budget+1)`
  fail closed.
- swap.rs hostile input — header/undo/body lengths capped before allocation;
  v2-shaped files reject without panic.
- atomic.rs / config write — temp-name retry, no-fallback-after-partial-write,
  lock-file PID+mtime staleness all correct.
- fs-watch debounce — sliding-window flush correct; `upsert` last-kind-wins
  documented and tested.
- highlighter stale-tree byte ranges — every boundary use goes through
  `safe_char_range`/`floor_char_boundary`; `end > source.len()` filtered.
- edit.rs join/split/block inverses — round-trip tests cover empty-prefix/
  suffix space-eating, ragged padding, non-`\n` separators.

### Coverage

Reviewed in full: hjkl-fuzzy, hjkl-xdg, hjkl-kitty, hjkl-buffer (geom, wrap,
buffer, edit, search, undo.rs ~1200 lines incl. delta storage), hjkl-engine
(motions, search, substitute, selection_shift, buffer_impl, tag, editor.rs ~700
lines of cursor/byte math), hjkl-vim (step, count, curswant, insert_bridges),
hjkl-ex (parse, range, expand, global, shell, complete ~400 lines), hjkl-lsp
(codec, server, params, runtime), hjkl-clipboard (base64, osc52, uri), hjkl-fs
(atomic), hjkl-fs-watch, hjkl-config (write), hjkl-app (swap), hjkl-bonsai
(highlighter).

Skimmed (grep-level, not full trace): hjkl-ex/builtins.rs (5.5k lines),
folds.rs, undo.rs remainder, hjkl-engine/editor.rs remainder (7.5k),
hjkl-vim/vim.rs, hjkl-syntax, hjkl-bonsai (folds/hex_color/comment_markers),
hjkl-mangler, hjkl-compat-oracle, hjkl-anvil.

GAP (not reviewed): apps/hjkl main loop + TUI host crates (hjkl-\*-tui,
hjkl-layout, hjkl-tabs), hjkl-hover/which-key/info-popup (timing logic),
hjkl-picker/menu/prompt internals — where rendering-index bugs would most
plausibly hide next. Windows/macOS code code-read only, not compiled on this
Linux host.

## 10. 2026-08-10 security audit — whole workspace

Scope: clean tree, whole workspace. Attack surface walked: CLI args,
`--embed`/`--nvim-api` RPC, ex command layer, fs policy, swap/undofile
deserialization, X11/Wayland clipboard, LSP JSON-RPC, modelines, grammar
downloads, anvil installer, config/XDG, fs-watch. Every finding re-traced
end-to-end before inclusion.

### Findings — ranked

#### 1. HIGH — arbitrary file read in `--nvim-api` mode, bypassing the RPC filesystem confinement

`apps/hjkl/src/nvim_api.rs:731` → `app.nvim_set_buffer_name(id, &name)` stores
an RPC-supplied path verbatim, unvalidated, as the slot filename
(`apps/hjkl/src/app/buffer_ops.rs:512-519`). A bare `:e` then reloads that
filename with no fs-policy check: `nvim_command` → `dispatch_ex` →
`ExEffect::EditFile` (`app/ex_dispatch.rs:714`) → `do_edit("")` returns to
`reload_current` before the `check_fs_path`/`resolve_under` block that only
guards the non-empty-arg path (`ex_dispatch.rs:2151-2153` skips; the check is at
`2174-2187`). `reload_current` reads `self.active().filename` via
`read_to_string_unbounded` (`ex_dispatch.rs:2309`) — no policy call anywhere in
the function. `:checktime` → `checktime_slot` (`ex_dispatch.rs:2385`, read at
`2457`) is a second trigger with the same gap. The write side is still blocked
(`:w` → `save_file_durable`), so the bypass is read-only.

`restrict_fs()` is active in this mode (`main.rs:536-538`); its stated purpose
is exactly "a remote/automated caller cannot read … arbitrary filesystem
locations via `:w`/`:e`/`:r`". The read half fails.

```
Repro (any client that can drive the msgpack-RPC pipe):
  nvim_buf_set_name(<buf>, "/home/user/.ssh/id_rsa")   # or any readable path
  nvim_command("e")                                    # :e! also works
  nvim_buf_get_lines(<buf>, 0, -1, false)              # returns the file contents
```

`--embed` is NOT affected: its `EditFile` arm resolves the path through
`check_fs_path` + `resolve_under` (`embed.rs:209-224`, verified).

Fix direction: `reload_current`/`checktime_slot` must run the same
`check_fs_path` + `resolve_under` gate as `do_edit`'s open path (or
`nvim_buf_set_name` must validate/reject escapes while `fs_restricted()`).

#### 2. MEDIUM — `:Anvil install/update/uninstall` reachable from `--nvim-api` RPC with no policy gate

`nvim_command` (`nvim_api.rs:1144`) → `app.dispatch_ex` → host registry, where
`AnvilCmd` is registered (`app/ex_host_cmds.rs:1599`) and its `run`
(`1352-1379`) calls `app.anvil_install/uninstall/update`
(`app/ex_dispatch.rs: 3128-3198`) with no `fs_restricted`/`shell_disabled` check
— unlike `:make`/`:grep`/`:!`. `anvil_install` hands the spec to
`InstallPool::install`, which spawns worker threads running `install_blocking`
(`hjkl-anvil/src/job.rs: 65,77,166`) — a live GitHub download + extract +
chmod + symlink, or a cargo/npm/pip/go install that runs the package's build
scripts.

Impact: an untrusted RPC client can trigger arbitrary network fetches,
out-of-cwd disk writes (store lives under XDG data), and native build/script
execution at will; on the TOFU path a MITM on a first fetch becomes the trusted
baseline for every later install. Tool name/version is pinned by the embedded
registry, so this is not arbitrary-code injection by choice of payload — the
attacker controls that a pinned artifact is fetched/built, and when. `--embed`
is not affected (dispatches through `hjkl_ex::default_registry`, no host
commands).

### Cleared (suspected, disproved)

- Embed `:w <abs-path>` / `:saveas` arbitrary write — `save_file_durable`
  re-checks policy (`save.rs:197,205-217`); nvim-api `:w` hits the same seam.
- `nvim_buf_set_name` + `:w` write bypass — write path is the checked
  `save_file_durable`; only the read side (finding 1) leaks.
- Modeline RCE — only whitelisted options apply; `makeprg`/`errorformat`
  rejected with a pinned test; same whitelist on RPC-set content.
- Shell-out from RPC — all four surfaces gate on `shell_disabled()`: `:!`,
  `:r !cmd`, range filter, `:make`/`:grep`. `:Anvil`'s builders are the only
  un-gated spawns (finding 2).
- `%`/`<cword>`/`<cfile>` expansion — pure string substitution; expanded results
  still pass through the gated dispatch.
- Undo/swap deserialization — magic + version + capped lengths (header 1 MiB,
  undo 256 MiB, body 64 MiB); postcard fail-safe; `from_serializable` validates
  root/current bounds, parent/child indices, seq uniqueness, child partition and
  reachability before use.
- X11 clipboard wire — libxcb does the wire parsing; INCR capped (256 MiB +
  timeouts), TARGETS len%4 checked.
- Wayland socket — hostile size field handled (`total < 8` → drop).
- LSP JSON-RPC — 16 MiB message / 64 KiB header caps; parse errors logged not
  panicked; `workspace/applyEdit` declined; diagnostics ranges bounds-guarded.
- Grammar auto-install in RPC — disabled when `fs_restricted()`; git clone args
  validated.
- `:Anvil uninstall` traversal — `validate_name` rejects `..`/absolute
  (regression test).
- TOCTOU on saves — canonicalize + `resolve_under`, `O_NOFOLLOW` probe,
  rename-replaces-symlink.
- RPC `:set` config write-back — `persist_set_options` no-ops without
  `config_path`, only set in the TUI path.

### Hardening (correct today, fragile)

- `reload_current`/`checktime_slot` read paths — shared by TUI (policy off) and
  RPC (policy on, bypassed — finding 1); after the fix, any future read of
  `slot.filename` should route through one checked seam.
- `--embed`/`--nvim-api` `:e` reads are unbounded (`read_to_string_unbounded`,
  `embed.rs:225`, `ex_dispatch.rs:2309`) — a client that can write a multi-GB
  file into cwd forces a matching allocation; the crate's own doc reserves
  capped reads for that case.
- TOFU first-install in anvil — a MITM on a first download becomes the permanent
  trusted baseline; documented trade-off, now reachable remotely via finding 2.
- TUI grammar auto-install — clones a pinned rev and `dlopen`s a freshly
  compiled `.so`; supply-chain exposure acknowledged in the docs.
- RPC has no auth at all — by design (pipe ownership = trust); worth restating
  in `docs/embed-rpc.md` since finding 1 makes the confinement the only read
  control.

### Coverage

Walked: CLI (`main.rs`), JSON-RPC (`embed.rs`), msgpack-RPC (`nvim_api.rs`), ex
layer (`builtins.rs` write/read/edit/saveas/cd, `shell.rs`, `range.rs`,
`parse.rs`, `expand.rs`, `global.rs`), app ex dispatch (`ex_dispatch.rs`,
`ex_host_cmds.rs`, `quickfix.rs`), fs policy (`policy.rs`,
`hjkl-fs/src/ {path,read,open,atomic,lock,dirs}.rs`), swap/undofile, clipboard
backends, LSP, modeline, anvil installer/store/job, bonsai
loader/source/grammar, hjkl-lang directory gating, config/XDG, fs-watch.

GAPs: Windows/macOS clipboard backends + `identity.rs` Win32 path code-reading
only (not compiled on this host); ~30 small TUI crates audited for
unsafe/forbid-unsafe + shell spawns, not line-by-line; hjkl-buffer/engine
arithmetic hot paths trusted to ropey + the ~185-test/miri harness;
`builtins.rs` (5.3k lines) walked for shell/fs/panic effects, not every option.

**Summary:** 1 high, 1 medium. Risk low for the interactive TUI (local user,
vim-parity shell intentionally on), moderate for the RPC modes whose confinement
is the only thing between a pipe-writing client and the user's files. Fix order:
(1) gate `reload_current`/`checktime_slot` — closes the arbitrary-file-read; (2)
gate or remove the `:Anvil` surface while `fs_restricted()`; (3) route the two
RPC `:e` reads through the capped reader.

## 11. 2026-08-10 tidy pass — whole workspace

Scope: clean tree, whole workspace. Cleanups only, each verified
behavior-preserving; compiler-caught dead code excluded (clippy `-D warnings`
green). All `pub` API items cross-checked workspace-wide — zero dead public API
exists.

### Findings — ranked by value

1. **~70 identical inline rope→lines snapshots → call
   `hjkl_engine::rope_util::rope_to_lines_vec`** (`rope_util.rs:52`). The block
   `.rope().lines().map(|s| { let s = s.to_string(); s.strip_suffix('\n').map(str::to_string).unwrap_or(s) }).collect::<Vec<_>>()`
   is repeated: 17× `app/tests/ex.rs`, 17× `app/tests/marks_registers.rs`, 12×
   `app/tests/keymap.rs`, 8× `app/tests/visual.rs`, 6× `app/tests/lsp.rs`, 5×
   `app/tests/formatter.rs`, 2× `app/tests/pickers.rs`, 1×
   `app/tests/splits_windows.rs`, plus `tests/tag_rename.rs:62`,
   `tests/smartindent_html.rs:68`. (Paths are under `app/tests/`, not `app/`.)
   `rope_to_lines_vec` is the exact existing implementation. Action: replace
   each copy with the call.
2. **Dead SHA constants + stale `#[allow(dead_code)]`** —
   `hjkl-anvil/src/installer.rs:1380,1382`,
   `hjkl-anvil/tests/install_tests.rs: 53,55`. `HELLO_GZ_SHA` and
   `HELLO_RAW_SHA` are defined but never referenced (grep: definitions only);
   `HELLO_ZIP_SHA` is live (`installer.rs:2028`). The `#[allow(dead_code)]` at
   `installer.rs:1379` is stale. Action: delete the dead pair in both files,
   drop the allow. Secondary: the four fixture hashes are duplicated verbatim
   between the two files — one shared home would desync-proof them.
3. **`display_width` duplicates public `char_col_to_visual_col`** —
   `hjkl-buffer-tui/src/render.rs:479-489` vs `hjkl-buffer/src/geom.rs:150-160`.
   `display_width(line, tab_width)` implements exactly geom's `cell_width` tab
   rule; `char_col_to_visual_col(line, usize::MAX, tab_width)` computes the
   identical value. Both callers pass `effective_tab_width()` (never 0), and the
   swap also removes a latent `col % 0` panic in `display_width`. Action: delete
   `display_width`, call `char_col_to_visual_col(&line, usize::MAX, tab_width)`.
4. **Identical leading-whitespace width loops ×2 → extract to hjkl-buffer** —
   `hjkl-buffer-tui/src/render.rs:1032-1041` and
   `apps/hjkl/src/render.rs: 1094-1103`, byte-for-byte same body (' ' +1, '\t'
   +tab_width - col%tab_width, else break); both crates depend on hjkl-buffer.
   Action: add `leading_visual_width(line, tab_width)` to
   `hjkl-buffer/src/geom.rs` and call it from both loops.
5. **`feed`/`feed_insert` forked 4× in integration tests** —
   `tests/{tag_rename.rs:28,45, autopair.rs:36,70, comment_continuation.rs:28,56, smartindent_html.rs:34,51}`.
   Four drifted copies of the chars→`Input` mapping (key sets differ: Esc only
   vs Enter+Backspace+Esc vs Enter+Esc). Action: one shared helper in e.g.
   `tests/common/mod.rs` handling the union; each file keeps only its `editor()`
   options.
6. **Two ancestor-marker project-root finders** — `app/types.rs:493-516`
   (`find_project_root`) vs `hjkl-lsp/src/workspace.rs:10-25` (`find_root`).
   Same walk; a file `start` never matches in the app version either, so both
   effectively begin at the parent. Action: `find_project_root` delegates to
   `hjkl_lsp::workspace::find_root(start, MARKERS).unwrap_or_else(|| start.to_owned())`.
   Single call sites: `app/mod.rs:2106`, `lsp/runtime.rs: 132`.
7. **Dead payload in a pub enum** — `hjkl-syntax/src/lib.rs:283`
   `Loading(#[allow(dead_code)] String)`: constructed (`lib.rs:553,593`) but
   never read; `is_known()` only matches the variant shape. Action: drop the
   payload (unit variant) and the allow; public-API shape change if an external
   consumer reads the payload.
8. **"Mirrors" claim is two drifted SHIFT-normalization copies** —
   `app/keymap.rs:175-194` (`chord_event_to_input`) vs
   `hjkl-keymap-tui/src/ lib.rs:33-40` (`from_crossterm`). keymap.rs documents
   the SHIFT rule as mirroring `from_crossterm`, but they diverged: keymap-tui
   drops SHIFT for EVERY `Char`; the app copy folds only ASCII letters and keeps
   SHIFT on other chars (`<S-1>` etc.). Both paths run (event_loop.rs:49
   `from_crossterm`; event_loop.rs:1040 `crossterm_to_input`), so the same
   physical key can produce different `Input`s. **This is a real behavior
   inconsistency, not a mechanical cleanup** — unify on one rule (a decision) or
   correct the comment. Cross-cutting with correctness.
9. **`truncate_desc` ≡ `truncate_to_width(s, max−1) + "…"`** —
   `hjkl-which-key/src/lib.rs:80-97` vs `hjkl-statusline/src/lib.rs:18-30`; same
   display-width loop, different ellipsis budget, no dep edge between the
   crates. Only worth sharing if a third consumer appears.

### Coverage

Read in depth: hjkl-engine rope_util, hjkl-buffer geom/buffer/search/folds/
viewport/listchars/wrap, hjkl-xdg, hjkl-config, hjkl-fs dirs, hjkl-statusline,
hjkl-which-key(-tui), hjkl-keymap + keymap-tui, hjkl-engine-tui, hjkl-theme-tui,
hjkl-picker source/rg, hjkl-lsp workspace/runtime, hjkl-mangler, hjkl-anvil
installer + tests, hjkl-syntax (Loading), hjkl-ex expand/folds/complete, app
keymap/types/event_loop/render (sampled)/mod (sampled)/tests.

Mechanical scan: every `pub fn`/`const`/`struct`/`enum`/`trait`/`pub use … as`
in all 59 packages checked for workspace-wide references — zero dead public API.
All `#[allow(dead_code)]` sites inventoried; only the anvil + syntax ones were
actionable.

GAPs (skimmed, not read line-by-line): hjkl-bonsai (largest crate), hjkl-css,
hjkl-markdown(-tui), hjkl-layout, hjkl-form, hjkl-completion(-tui),
hjkl-hover/holler/prompt/menu/tabs/splash families, hjkl-vim FSM internals,
hjkl-clipboard platform backends (`#[allow(dead_code)]` there is platform-gated,
unverifiable on this Linux host), and the bulk of `apps/hjkl/src/render.rs` +
`hjkl-buffer-tui/src/render.rs` (4452/5338 lines — sampled around every cited
symbol). Duplicated logic could exist inside those untouched regions.

## 12. 2026-08-10 perf pass — whole workspace

Scope: clean tree, whole workspace. Overall the codebase is aggressively
performance-engineered (viewport-bounded span caches, incremental tree-sitter
parse, delta undo with keyframes, per-row search caches, thread-local wrap
scratch). The four findings below are the verified outliers; the two picker ones
are the only per-keystroke items of real size.

### Findings — ranked by impact

1. **Picker list rebuilt from scratch on every draw — ~1000 `label()` calls + a
   String alloc per character** (`apps/hjkl/src/render.rs:2897-2934`,
   `hjkl-picker/src/picker.rs:471-499`). Per draw, `visible_entries()` and
   `visible_entry_styles()` each call `source.label(idx)` for ALL ≤500 filtered
   entries (~1000 label() calls/frame, each a Mutex lock + two allocs for
   FileSource), then every char of every label becomes an individual `String`
   (`render.rs:2916` `ch.to_string()`). The event loop sets `needs_draw` before
   every dispatched key (`app/event_loop.rs:2229`), so this is once per
   keystroke — ~1500+ heap allocations per keystroke. Fix: build `ListItem`s for
   only the visible window (~15-20 rows); merge the two passes into one
   `label()` call per row; use `Span::raw` over borrowed label slices instead of
   per-char strings.
2. **Picker re-scores and fully sorts the entire candidate set every keystroke**
   (`hjkl-picker/src/picker.rs:329-357`). Per keystroke, `score()` runs over all
   N candidates (FileSource caps at 50,000, `source/file.rs:119`), each
   allocating a fresh positions `Vec` (`hjkl-fuzzy/src/lib.rs:53,67`), then a
   full O(N log N) sort of all N before `.truncate(500)` (`picker.rs:357`). For
   an empty query every score ties, so opening the picker (and every streaming
   batch growth, which re-triggers refresh at `picker.rs:263`) sorts 50k entries
   by full lowercased text. Fix: a size-500 `BinaryHeap` keyed
   `(score, lowercased_text)` (or `select_nth_unstable` + partial sort) → O(N
   log 500); reuse one scratch positions `Vec` across candidates.
3. **`:s` materializes the whole buffer for a single-line range**
   (`hjkl-engine/src/substitute.rs:344,415-417,422` +
   `hjkl-ex/src/builtins.rs: 1311-1313`). `apply_substitute` calls
   `rope_to_lines_vec` on the ENTIRE buffer (344), then `new_lines.join("\n")`
   (415), a full-buffer `replace_all` (416, rope rebuild + cache invalidation),
   and `rebase_marks_after_row_growth` over all rows (422). A bare `:s/foo/bar/`
   defaults to the cursor line only (builtins.rs:1311-1313): a 1M-line file pays
   two full copies + 1M small allocs + a full rope rebuild to edit one line. The
   sibling `collect_substitute_matches` already walks only `start..=clamp_end`
   with a borrowed `rope_line_slice` per row (substitute.rs:539-543). Fix:
   materialize only `start..=clamp_end`, splice back with a range remove+insert,
   rebase marks only for the changed range → O(range) instead of O(N).
4. **Systemic O(rows × folds) scans on per-keystroke / per-frame paths**
   (`app/syntax_glue.rs:536`; `hjkl-buffer/src/buffer.rs:237,249,281,306,454`;
   `hjkl-buffer/src/folds.rs:542,555`; `hjkl-engine/src/buffer_impl.rs:594`;
   `apps/hjkl/src/render.rs:1436`). `folds.iter().any(|f| f.hides(row))` — a
   linear scan of the whole fold list per row — runs on per-keystroke paths
   (scroll-follow, syntax effective-height, engine `j`/`k` via
   `SnapshotFoldProvider`) and per-frame paths (cursor screen-row, explorer
   paint). Folds are kept sorted by `start_row` (`folds.rs:140-145`), so each
   query can be O(log F); the TUI renderer already built exactly this
   (`FoldIndex`, `hjkl-buffer-tui/src/render.rs:610-616,721-725`) while these
   call sites stayed linear. Fix: expose a `hides(row)` using `partition_point`
   over `start_row`; have `SnapshotFoldProvider` and the syntax pass use it.
   `SnapshotFoldProvider::from_buffer` (`buffer_impl.rs:581-588`) also
   deep-clones the fold `Vec` per `j`/`k` and per frame — avoid with a
   borrow-style provider or a gen-checked snapshot cache.

### Coverage

Traced in depth: per-keystroke insert/edit path, cursor motion (LineCache),
search + per-row cache, substitute (both paths), render loop + draw gating,
per-frame widget render, syntax pipeline, git signs/blame workers, LSP glue,
picker + sources + preview, completion, explorer render cache, RPC server mode.
All already well-hardened; the four findings above are what remains.

Not read: hjkl-ex dispatch beyond substitute, hjkl-keymap trie, hjkl-vim FSM,
hjkl-fs-watch, hjkl-clipboard, hjkl-layout, hjkl-markdown, hjkl-kitty,
hjkl-hover, most ~30 small TUI crates, hjkl-bonsai folds/hex_color internals.

Needs profiling to settle: actual per-keystroke cost of incremental tree-sitter
reparse + viewport highlight on very large files; the `lsp_render_fingerprint`
full-diagnostic scan on idle poll (`event_loop.rs:2043-2048`, ~8/s — cheap
unless tens of thousands of diagnostics); `handle_publish_diagnostics` calling
`current_dir()` per slot per message and allocating a `String` per diagnostic
line (message-frequency bounded). Note: buffer-tui render's per-row
`conceals.iter().filter(|c| c.row == doc_row)` (`render.rs:803-808`) is O(rows ×
conceals) per frame, but only bites while the explorer pane with a large tree is
open and is dirty_gen-cached — low priority.
