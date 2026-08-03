# hjkl — review backlog

Single source for open findings, deferred decisions, and blocked work. Findings
use symbol names rather than line numbers so references survive refactors.

## 1. Open work — ranked

### 1.1 Undo-tree step cost — resolved 2026-08-02

All three costs named by this item are gone. Holding `g-` over 1024 states went
4.378 ms -> ~555 us, and a single step no longer scales with depth at all (2.206
us -> 101.7 ns at depth 1024, verified by an independent A/B of the new bench
against the pre-change code). Details in the `hjkl-buffer` changelog.

Two findings worth keeping, because both inverted an assumption in this entry:

- **The arena scan was named first and mattered least.** Measured attribution on
  `cold_jump_back/1024`: the `BTreeMap` seq index bought -9% (4.378 -> 3.968
  ms), the `Rope::is_instance` fast path in `set_node_state` bought -62% (3.968
  -> 1.659 ms). A full-document `PartialEq` on every history step dwarfed an
  O(N) scan over a slab of small structs.
- **`single_deep_jump` was never measuring the jump.** criterion 0.8.2's
  `iter_batched` hands the routine its input BY VALUE, and
  `outputs.extend(inputs.into_iter().map(&mut routine))` sits between
  `Measurement::start` and `end` (confirmed by reading `bencher.rs`), so the
  O(N) teardown of the whole `UndoTree` is charged to the jump. That is what
  made the case look depth-scaling however cheap the jump got.
  `single_deep_jump_no_drop` uses `iter_batched_ref` and measures the jump;
  `single_deep_jump` was deliberately left alone so its recorded history stays
  comparable. **Any future undo bench that takes the tree by value has the same
  defect.**

Left open, small:

- `lowest_offpath_leaf` / `prune_root_side` still test path membership with
  `current_path().contains(..)` rather than the `UndoNode::on_path` flag that
  now exists, which would make `cap` O(N) instead of O(N\*depth). `cap` is not
  benched, so this was left out to keep the change confined.
- `retarget_current` is O(tree distance), not O(1): a `g-` crossing to a far
  branch still walks to the fork. Those ancestors genuinely have to change, so
  the worst case stays O(depth) for an adversarial branch layout. Every benched
  case, and `u` / `<C-r>`, are distance 1.
- **A freshly deserialized undofile still has no keyframes**, so its first deep
  jump remains O(depth). Eager construction may waste work and memory; not
  measured, unchanged by this pass.
- `from_serializable` now normalises `last_child` along the loaded root->current
  chain. A file written by `to_serializable` already agrees, so it is a no-op
  there — but a hand-edited or truncated undofile used to be repaired by the
  next full-chain rewrite, and nothing repairs it now.

### 1.2 Swap `SerTree.base` duplicates the document

`crates/hjkl-app/src/swap.rs` stores the document roughly twice: once as the
streamed body and once as `SerTree.base`. A single-node tree on a 20 000-line
document serializes in ~99 µs; marginal node cost is only ~30–40 ns.

De-duplicate `base` against the streamed body. Do not implement the append-only
delta log paired with issue #302: it attacks node count, not the base copy or
`fsync`. Worst measured cell is 457 µs, so urgency is low.

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

| Item                                                | Where                                       | Note                                                                                                                                                                                                                                |
| --------------------------------------------------- | ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| LSP full-sync still copies once                     | `hjkl-lsp/src/runtime.rs`, `server.rs`      | `Buffer::content_joined()` caches the `Arc`, so `Arc::unwrap_or_clone` cannot move. Avoiding the copy requires direct serialization instead of an intermediate `serde_json::Value`.                                                 |
| `attach_buffer` copies at the boundary              | `hjkl-lsp/src/manager.rs` (`attach_buffer`) | Takes `text: &str` and calls `text.to_string()`. Change the boundary ownership model.                                                                                                                                               |
| `styled_spans` cannot be removed — `sqeel` reads it | `hjkl-engine/src/editor.rs`                 | RESOLVED 2026-08-02: not write-only after all. `sqeel-tui` pins published `hjkl-engine` and reads the field (one `mem::take`, two `clone`s). Removing it needs a supported accessor in `sqeel` first. The field documents this now. |

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
- **`resolve` can block.** `SyntaxLayer::extract_fold_ranges` passes
  `LanguageDirectory::by_name`, which clones + compiles a grammar on first use.
  Same exposure the highlight path already has (`walk_rows` passes the same
  closure), and it is gated behind `builtin_folds(lang).is_some()` so only
  foldable languages can trigger it — but a fold pass CAN now be what fetches a
  grammar. A cache-only lookup would need a new `LanguageDirectory` method.
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

Only one range bug was found in that second pass, and it was total rather than
off-by-one: **C# folded nothing at all** (fixed — see the CHANGELOG; guarded by
`folds::every_bundled_fold_query_is_reachable_by_extension`, which is
grammar-free and runs in the normal lane). The lesson generalises: a bundled
fold query is keyed by the name `GrammarRegistry` resolves the _extension_ to,
which is not necessarily the `.scm`'s file name, and a query that never runs
looks exactly like a language with nothing foldable.

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

`incremental_path_matches_cold_for_small_edit` was failing at HEAD for the whole
of that pass and is fixed (2026-08-04): `SyntaxLayer::apply_edits` cleared the
parse, row-start and sign caches but not the SPAN cache, leaving that one to the
`dirty_gen` mismatch in `render_viewport`. The test builds two fresh `View`s,
which share a `dirty_gen`, so the mismatch never fired and the incremental
render returned the pre-edit span table — hence "byte ranges one short" and the
missing capture, with the tree itself correctly reparsed. Not reachable from the
app, where every edit advances `dirty_gen`.

**No CI lane runs it.** Every grammar-backed test in `hjkl_syntax` is
`#[ignore]`d because CI has no grammars, so this class of defect can only be
caught by running `cargo test -p hjkl-syntax --lib -- --ignored` by hand.

### 1.4c Swap still derives its directory from the environment (2026-08-02)

The trash half of this is done: `hjkl_app::trash::TrashRoot` carries an explicit
root through `trash_path_in`, the explorer pane owns one
(`ExplorerPane:: trash_root`), and no explorer test touches `XDG_CACHE_HOME` any
more. `CwdGuard::set_env` is gone with them.

`hjkl_app::swap` was NOT converted and still resolves
`$XDG_CACHE_HOME/hjkl/ swap` on every call (`swap_dir`, `swap_path_for`,
`scratch_swap_path`). One test still overrides the variable for it:
`scratch_buffer_writes_swap_when_dirty` in `apps/hjkl/src/app/tests/ex.rs`, via
`EnvVarGuard` — the only remaining `EnvVarGuard` user in the `hjkl` binary.

It is the same shape and the same fix (a `SwapRoot` beside `TrashRoot`), but a
wider one: the swap directory is resolved from `App::build_slot`,
`write_swap_for_slot`, `arm_swap_on_open`, `recover_orphan_scratch_buffers` and
`main`'s `-r` handler, so the root has to reach `App` itself rather than one
pane. Deliberately left out of the trash change to keep its blast radius to the
explorer.

Not currently known to cause a flake: unlike the trash case, the swap override
points at a `TempDir` that is not also an explorer root, so the directory a
concurrent test creates inside it is never enumerated by anything. The hazard is
the reverse direction — while that test holds the override, any other test's
swap is written under its `TempDir` and vanishes when it drops.

### 1.5 Remaining differential-oracle and code-review fixes

Fixed by `9a156885`, `b97e9bce`, `76cfb459`, and earlier commits. Detailed
reproductions for resolved entries are preserved in the supporting-evidence
appendix below, marked as fixed. Preserve each fixed case in the tier-2
compatibility corpus and verify it against nvim before changing expectations.

| Priority | Task                                                                                                                                                                           | Where / acceptance criterion                                                            |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------- |
| Medium   | Remaining blockwise non-delete divergences: a TEXT OBJECT in blockwise visual, and `H` / `L` / `gE` motion + cursor behaviour. Blockwise `>` / `<` geometry landed 2026-08-04. | `<C-v>iw` leaves hjkl in charwise `visual` where nvim stays `visual_block` — see §1.5b. |

The paragraph/WORD landings, and the `V}u2)1gUiW` composite, were fixed on
2026-08-02 (`motions::move_paragraph_next` / `move_paragraph_prev` rewritten on
vim's `findpar`, `motions::prev_word_start`'s empty-line stop, the operator
adjustments in `vim::op_motion::apply_op_with_motion`, and the all-or-nothing
counted `(`/`)` in `vim::motion` + `vim::text_object::sentence_step_forward`).
Their cases live in `corpus/tier2_paragraph_word.toml` and
`corpus/tier2_sentence.toml`. The backlog's recorded expectation for `B` had its
two columns swapped: nvim moves to `(0,0)`, hjkl was the one that stayed on
row 1. What those fixes deliberately left open is listed in §1.5b.

### 1.5b Left open by the 2026-08-02 motion-parity pass

Each entry below was reproduced through
`cargo run -p hjkl-compat-oracle --release --example dfcase` against neovim
0.12.4 and left unfixed on purpose.

**Every edit clones its payload for the change log.** `editor::edit_to_editops`
does `replacement: text.clone()` for `InsertStr` / `Replace`, so a paste holds
the payload roughly three times at peak: the `yank.repeat(count)` (or the
linewise `join` + `format!`), this clone, and the rope's own copy. That is the
~3.1x charwise / ~4.1x linewise peak-RSS multiple measured on 2026-08-02 —
linewise pays the extra copy because it builds the repeated text and then
re-wraps it with a newline. `ContentEdit` fan-out is coordinates only and costs
nothing, so this clone is the one lever worth pulling. Making the change log
carry an `Arc<str>` (or borrow) would remove one whole payload copy from every
edit, not just paste. Not attempted: it changes a type the change log's
consumers read, so it is its own change rather than a paste fix.

**Left open by the blockwise-paste rewrite (2026-08-02).** The rewrite itself
shipped — see the `hjkl-vim` changelog for what changed and the measurements.

- **`visual_paste` was not examined.** Its blockwise branch may still carry the
  whole-document `Vec<String>` rebuild that was removed from `do_block_paste`;
  nothing there was read or measured.
- **`content()` hides trailing-newline bugs from unit tests.** It appends a
  newline when the rope lacks one, so a paste that eats the buffer's terminator
  compares equal through it. That is why the first version of the rewrite passed
  every hand-written geometry case while the nvim oracle failed on
  `visual_block_paste_past_eof`, which reads the rope directly. Any assertion
  about buffer termination must read the rope, as
  `block_paste_past_eof_preserves_the_trailing_newline` does. Whether other
  buffer-shape assertions in the suite are blunted the same way was not audited.

**The engine has a message channel now (2026-08-04).** `Editor::push_error` /
`take_errors` is the queue the `take_lsp_intent` / `take_fold_ops` precedent
suggested, `do_paste` pushes vim's `E342: Out of memory!  (allocating N bytes)`
on every over-budget path, and `apps/hjkl` drains it in both post-mutation sync
paths (`App::drain_engine_errors`). It is one channel, not a paste-specific one
— any engine or discipline code that needs to say something to the user can push
onto it now.

Two things it deliberately does NOT do: an empty register is still silent (vim
says nothing there either, and that was the case the rejection used to be
indistinguishable from), and nothing rate-limits the queue, so code that pushes
per-iteration would flood the toast bar.

**A corpus case cannot pin a value when nvim is absent.** Every oracle test
skips wholesale without nvim, so the corpus expectations guard nothing on a
machine that has no neovim — including CI lanes that do not install it. The
`expected_*` fields are now all checked against nvim's outcome
(`diff.rs::run_single`), which closed the "documentation-only field" half of
this, but nothing compares hjkl against the authored values on its own.

**A text object in blockwise visual collapses the block to charwise
(2026-08-04).** `<C-v>iw` leaves hjkl in charwise `visual` where nvim stays
`visual_block`: `editor_ext::visual_text_obj_extend` sets `visual_anchor` and
the cursor but never `block_anchor` / `block_vcol`, which are what
`block_bounds` reads, and the charwise arm of `apply_visual_operator` then runs
the operator. That is why `<C-v>iw<` on `"\t(x).[y]"` still outdents the whole
line — the blockwise `>` / `<` fix underneath it is correct and reached by every
other blockwise path, but this case never gets there.

Found while fixing the block-column shift; it is the other half of the
"blockwise non-delete operators" item and wants its own change, since a text
object has to yield a RECTANGLE (vim keeps the block's rows and takes the
object's columns) rather than a charwise range.

**The differential fuzzer still reports 77 divergences at seed 777** (89 before
the 2026-08-02 pass, 84 after it, then 83 with the counted-`$` failure rule, 78
with the backward-word-motion rewrite and 77 with the blockwise-shift geometry,
all 2026-08-04). The bulk are the known-excluded classes named in §5 (`u` / undo
against the seeded nvim fixture, blockwise `<C-v>` non-delete operators) plus
the entries above.
`cargo run -p hjkl-compat-oracle --release --example difffuzz -- 400 777`
reproduces the list; build with `--examples`, not `--example difffuzz`, or the
other binary goes stale.

### 1.5c Left open by the 2026-08-03 review fixes

The two divergences this section recorded — `:s` leaving `curswant` stale, and
`.` losing the register the change named — are fixed, with the register fix
widened to `LastChange::OpMotion` / `OpTextObj` / `DeleteToEol` after nvim
confirmed the same semantics for `"adw.`, `"adiw.` and `"aD.`. Four cases in
`corpus/tier2_registers.toml` pin the register behaviour against nvim. What is
still open:

- **The `:s` curswant fix has no oracle case.** It is covered by two unit tests
  in `hjkl-engine`'s `substitute` module, because the corpus driver cannot
  replay `:` keys. A corpus case would need the ex layer driven some other way.
- **The whole `LastChange` register sweep is done (2026-08-04).** `CharDel`,
  `GnOp` and `VisualOp` (charwise, linewise and blockwise) all carry a
  `register` now, so every `.`-repeatable change repeats into the register it
  named. Two things the original entry got wrong, recorded because they are the
  kind of claim that gets re-derived: the live `x` / `X` path already honoured
  `"reg` (it routes through `record_delete`, not `set_yank`), and the visual
  variants had a bigger defect underneath — `"` armed the register chord in
  Normal mode ONLY, so `vll"ad` did nothing at all. `VisualReplace` and the
  visual-block replace variant are deliberately not in the sweep: `r` writes no
  register in vim.

### 1.6 Cursor-move API migration

`Move` and the debug invariant shipped; remaining phases are:

1. Migrate remaining `hjkl-engine` motions to `Editor::move_cursor`.
2. Migrate `hjkl-vim` motion, command, bridge, visual, and operator paths. Fix
   insert paths first, then visual yank, then normal operators/edits; widen the
   invariant after each class is clean.
3. Migrate `apps/hjkl` cursor writes.
4. Make raw cursor primitives crate-internal, remove public `set_sticky_col`,
   and reduce `apply_sticky_col` to the vertical clamp.
5. Report counts by `Move` variant and justify every `Move::Raw` site. Keep the
   compat oracle and PTY e2e behavior unchanged.

### 1.8 Open from the 2026-08-01 review and the 0.40.0 cut

The 2026-08-01 pass was a read-only full-codebase review targeting the areas the
2026-07-29 pass had listed as uncovered: `apps/hjkl`, `hjkl-lsp`, `hjkl-ex`,
`hjkl-editor(-tui)`, `hjkl-completion`, `hjkl-prompt`, `hjkl-menu`,
`hjkl-picker`, plus `hjkl-anvil`, `hjkl-app`, `hjkl-fs`, `hjkl-clipboard`, and
`hjkl-bonsai`. Everything actionable it found was fixed and shipped in 0.40.0 —
the git non-UTF-8 pathspec, duplicate-`WorkspaceEdit` groups, both modeline
parser bugs plus the marker-ordering divergence, the `listchars` width
approximation, silently-literal `errorformat` specifiers, the anvil `.bak`
collision and unvalidated release-URL fields, `pid_is_alive(0)`, the explorer's
re-implemented filesystem seam (and with it the fifo hang), the triplicated
`is_safe_component`, `find_bin`'s traversal-order pick, and the `:s///c` byte
slice. Each of those was re-verified as still fixed on 2026-08-02. What follows
is what was NOT tackled.

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

`hjkl_buffer::geom` is now `unicode-width`-aware and `Editor`'s duplicate
`visual_col_for_char` delegates to it. Three things were deliberately NOT done.

- **Wide-char cursor cases are expressible now (2026-08-04), and they found a
  divergence.** `nvim_driver` converts between nvim's byte columns and the
  corpus's char columns in both directions, so `tier1.toml` carries a
  wide-character group and the strongest available oracle guards the column
  math. What it immediately surfaced is the char-vs-grapheme split: hjkl's `l`
  moves one CHAR, nvim's moves one GRAPHEME. On `"e\u{301}abc"` hjkl lands on
  the combining mark (char 1) where nvim lands on `a`; on
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

- **`:set` write-through is TUI-only.** `--headless` and `--embed` call
  `hjkl_ex::try_dispatch` directly rather than going through `App::dispatch_ex`,
  so a `:set` in those modes applies to the session and is never persisted.
  Defensible for non-interactive modes, but it was an implementation call, not a
  stated decision.
- **Bare `:!cmd` gives the child no tty.** `hjkl-ex/src/shell.rs` runs it under
  `Command::output()`, which captures stdout and hands the child a null stdin,
  so `:!git commit` or `:!less` cannot work. Vim suspends the TUI and passes the
  terminal through. Either implement the suspend or document the limitation on
  `:!`.
- **The trash directory has no reaper.** `$XDG_CACHE_HOME/hjkl/trash/` grows
  without bound and `MAX_RETRIES = 1000` means the 1001st deletion of a
  same-named file fails rather than recycling a slot. 0.40.0 documented this;
  nothing reclaims it.
- **Mutex-poisoning policy is documented, not enforced.** `buffer.rs` now states
  that `lock().unwrap()` on buffer state is deliberate and a poisoned lock is
  fatal. The ~110 call sites are unchanged, so one panic while any of those
  locks is held still takes down every later access, including the save path.

### 1.9 Left open by the 2026-08-03 explorer and file-discovery changes

Two changes shipped that day: `explorer.open` became a startup preference only
`:set` writes, and the explorer, both pickers and `:grep` moved onto one file
policy in `hjkl_fs::project`. What they left behind, none of it blocking.

**`:set explorer.open=…` is a string match, not a registry option.** It is
handled in `App::dispatch_ex`'s host-owned pre-pass, beside `mouse` and
`endofline`, because the left dock is host state with no engine `Settings` field
to hang it on. Three consequences follow, and they are the same ones `mouse` and
`endofline` already have — this joins that group rather than creating it:

- `hjkl_engine::options_registry` does not know the name, so `:set all` omits it
  and nothing completes it.
- Only `=true` / `=false` / `?` parse. `:set explorer.open` bare and
  `:set noexplorer.open` are not accepted; the dotted name makes the vim-style
  `no` prefix read badly, which is why it was left out rather than forgotten.
- **`--headless` and `--embed` never reach the pre-pass at all.** `headless.rs`
  and `embed.rs` call `hjkl_ex::try_dispatch` against an `Editor` directly, so
  the token falls through to the engine's `:set` and is rejected as unknown.
  Neither mode has an explorer, so nothing is lost today — but it is the same
  root cause as "`:set` write-through is TUI-only" in §1.8, and one fix (routing
  those modes through a shared host pre-pass) would close both.

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

### 1.7 Harness, coverage, and hardening

- Clear nvim undo history after fixture seeding, then fuzz undo/redo. Extend
  cursor comparison beyond ASCII and add ex/search, fold, and `gq` coverage.
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
- Stabilize flaky PTY e2e cases. Cache/CWD/color isolation landed in `ca3852b2`;
  the explorer `dd` tests that failed under `cargo test`'s thread pool are fixed
  (see below). Unspecified PTY flakes may remain.
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

- **Left open by the 2026-08-02 cold-cache grammar-race fix.** The fix itself
  (`stage_and_publish` in `hjkl-bonsai/src/runtime/source.rs`) is verified: the
  `grammar tests` filter against an empty `XDG_CACHE_HOME` went from 13 failures
  to 70/70 twice. What it did not settle:
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
- **The undofile loader now rejects a corrupt projection instead of hanging on
  it (2026-08-03) — one claim from that review did not survive.** The cycle and
  duplicate-`seq` gaps are fixed in `UndoTree::from_serializable`. The review
  also said `retarget_current`'s first loop hangs on a cycle; it does not. That
  loop writes `on_path = true` as it walks, so it revisits a marked node after
  at most N steps and stops. Its reachable failure was the _second_ loop's
  `expect` panicking when the fork is not an ancestor of `current` — a
  corrupt-input crash, not a hang — and the loader change makes that unreachable
  too. Recorded because the distinction is easy to re-derive wrong.

- **Considered and declined: renumbering `depth` in `prune_root_side`
  (2026-08-03).** When the root's on-path child is promoted, it keeps its
  original `depth` instead of restarting at 0 the way `clear_all` does. That is
  a real inconsistency, and it is harmless: `depth` is read in exactly two
  places — `is_keyframe` (a multiple-of-`KEYFRAME_INTERVAL` test) and
  `child_depth = parent.depth + 1` — so the keyframe ladder keeps its spacing
  and only its offset from the new root moves, leaving the root-to-nearest-
  keyframe distance bounded by `KEYFRAME_INTERVAL` as before. Serialization
  fixes the offset anyway: `depths_from_root` recomputes from the new root on
  load. The `depth` field's own comment already covers this ("assigned once at
  creation and never renumbered", "a wrong depth costs speed, never content").
  Not worth a change.

- **Full-buffer allocation on motions and rectangular edits (2026-08-03).** Each
  of these rebuilds the whole document into a `Vec` for an edit or query whose
  extent is small. None is a correctness bug and none is measured; they are
  recorded so the next perf pass has the list.
  - `vim::text_object::sentence_boundary` and
    `vim::text_object::sentence_step_forward` each collect the entire buffer
    into a `Vec<Vec<char>>` on every `(` / `)` keystroke. Both were rewritten
    for vim parity, not for allocation discipline.
  - `vim::visual_ops::transform_block_case` and
    `vim::visual_ops::block_replace_bounds` collect a full `Vec<String>` via
    `rope_to_lines_vec` to edit one rectangle.
  - `vim::text_object_ops`' `reflow_rows` and its bounds helper do the same for
    a `gq` over a row range.

- **The `grep` / `findstr` search backends cannot honour gitignore
  (2026-08-03).** `hjkl_fs::project` is the one policy behind the explorer, both
  pickers and `:grep`, and ripgrep reproduces it exactly via `RG_IGNORE_ARGS`
  (asserted by `rg_args_match_walk_policy`). The fallbacks `detect_grep_backend`
  picks when ripgrep is absent cannot: `grep` and `findstr` have no notion of
  ignore files, so they search ignored paths too. Both now exclude `.git`
  (`--exclude-dir=.git` for grep; findstr has no equivalent and excludes
  nothing), which is as close as those tools reach. Closing it properly means
  enumerating with `project::walk_builder` and passing the file list to the
  backend — bounded argv, and awkward for the streaming live-grep source, so it
  was not attempted. Users with ripgrep installed are unaffected.

- **"CI green" does not include the Cron workflow.** miri / fuzz / deny / bench
  run on a separate weekly schedule and are not checked by a release. They were
  not checked for 0.40.0. Either fold the cheap ones into the release gate or
  add an explicit pre-release step that reads the last Cron result.

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
- macOS and Windows are the two platforms local work never exercises: filename
  encoding, path separators, and symlink permissions all differ there. Gate on
  the capability (probe and skip) rather than on `cfg(unix)`, which includes
  macOS.

## 5. Supporting evidence

These appendices preserve the reproductions, design constraints, and audit
method needed to complete the open work above.

### Differential audit against neovim (2026-07-28, revised 2026-07-29)

#### Method

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

Every entry below was verified by hand through `dfcase`; the fuzzer only located
them.

#### Evidence for differential-audit backlog items

##### R4. `dk` / `dj` / `d+` / `d-` destroy the line at a buffer edge

Introduced by `0107e2e8`, which fixed R1 by exempting linewise motions from the
`start == end` guard in `apply_op_with_motion`:

```rust
if start == end && !matches!(kind, RangeKind::Linewise) {
    return;
}
```

The exemption is too broad. It is correct for `_`, whose count-1 form covers the
current row by definition, but `j` / `k` / `+` / `-` are also linewise and must
**fail** when there is no row to move to. All four now delete:

```
`dk` (also `dj`, `d+`, `d-`) on "only line", cursor (0,3)
hjkl: ""              ← line destroyed
nvim: "only line"     ← unchanged
```

This regresses the earlier one-line `dk` edge fix from `b4458135`. The fuzzer
independently surfaced `dk` as a new divergence in the latest pass. Multi-line
`dk` (row 2 of 3) is still correct.

This distinguishes failed motions from zero-distance successful motions: `_`
does not fail at count 1; `k` at row 0 does. Fixed by `b97e9bce`.

##### 11. Unbounded memory on large paste counts — improved, not closed

`62bd4853` replaced the ineffective count cap with a 10 MiB byte budget in
`do_paste`. The payload is now bounded, but applying it still peaks far above
the budget:

| case                                        | `ulimit -v 2 GB` |
| ------------------------------------------- | ---------------- |
| `yy999999999p`, 10-byte register            | abort (134)      |
| `yy999999999p`, 2000-byte register          | abort (134)      |
| `yy5000p`, 10-byte register (50 KB payload) | ok               |

Same input succeeds at `ulimit -v 8 GB`. The cost tracks total payload bytes,
not iteration count — ~5200 iterations of a 2000-byte register (10 MiB, the
clamp ceiling) aborts, while 5000 iterations of a 10-byte register (50 KB) does
not. So a paste sitting exactly at the permitted ceiling needs >2 GB peak RSS,
roughly 200× amplification.

The budget is therefore too generous relative to per-byte overhead. Fixed by
`b97e9bce`: budget lowered to 1 MiB with batched, pre-allocated edits. Silent
rejection of over-budget pastes is tracked in the ranked backlog above.

The weekly cron fuzz job runs with libFuzzer's default 2048 MB rss limit, so
this remains reachable there.

**Every number above predates the batching in `b97e9bce` and no longer describes
the code.** They were measured against the per-iteration loop that commit
deleted, which is why cost tracked iteration count rather than payload bytes.
`b97e9bce` batched the edits AND lowered the cap to 1 MiB in one commit, and the
cap was never re-derived from the batched path's real cost. Re-measured
2026-08-02 — see below.

##### 5 (residual). Blockwise visual — non-delete operators

`ba813ca0` fixed blockwise + text object (`<C-v>iwd`, `<C-v>i(d` now match). 21
blockwise divergences remain, unchanged across the last two passes, concentrated
on the indent operators and on `H` / `L` / `gE` motions:

```
`<C-v>iw<` on "\t(x).[y]", cursor (0,6)
hjkl: "(x).[y]"     ← outdented the whole line
nvim: "\t(x).[y]"   ← unchanged
```

This exact case is now a TEXT-OBJECT bug, not a shift-geometry one: blockwise
`>` / `<` shift at the block's left column since 2026-08-04, but `<C-v>iw`
collapses hjkl to charwise visual before the operator runs, so this case never
reaches the blockwise arm. See §1.5b.

`<C-v>H>` also still diverges on cursor. Blockwise `~` is correct.

##### 8 (residual). Two motion landings — FIXED 2026-08-02

Re-measured before fixing; the `B` row's two columns were swapped when this
table was written.

| case                                                       | hjkl (was) | nvim   |
| ---------------------------------------------------------- | ---------- | ------ |
| `}` at EOF, buffer `"    it's.{foo}.A-B.{A-B}"` from (0,0) | (0,0)      | (0,23) |
| `B` at col 0 of row 1, buffer `"\nabc"`                    | (1,0)      | (0,0)  |

`}` was landing on column 0 of whatever row it stopped on; vim's `findpar` puts
the cursor on the **last character** of the last line when the scan ends there.
`B` was failing outright (no move) whenever the backward whitespace skip hit an
empty line or ran off the front of the buffer from a row other than row 0.

Two further defects in the same two functions were found while fixing these and
fixed with them: `}`/`{` stepped over a blank line adjacent to the cursor
(skipping a whole paragraph), and a counted `9}` clamped to the edge row instead
of failing. Cases: `corpus/tier2_paragraph_word.toml`.

##### T1. Composite case-op sequence — FIXED 2026-08-02

```
`V}u2)1gUiW` on "'qux'  A-B", cursor (0,9)
hjkl: "'qux'  A-B"   (unchanged)
nvim: "'QUX'  a-b"
```

Cause confirmed: the cursor placement after `2)`, as suspected. `V}u` lowercases
the line and leaves the cursor at (0,0) in both engines; the line has no
sentence terminator, so vim's `findsent` fails on the first repetition with a
second still owed and leaves the cursor at (0,0), while hjkl moved as far as it
could and landed on (0,9). `1gUiW` then uppercased the WORD under a cursor that
had drifted to `A-B`, and `u` — read as undo of the case change rather than as
part of the composite — reverted it. Minimal repro without the case operators:
`2)` on `"'qux'  A-B"` from (0,0), hjkl (0,9) vs nvim (0,0). Counted `(` had the
same defect in reverse (`3(` from (0,10) on `"One. Two. Three."`: hjkl (0,0),
nvim (0,10)). Cases: `corpus/tier2_sentence.toml`.

#### Watch items

**W1. `7e260e27` moves the cursor with `buf_set_cursor_rc`.** The landing
position is correct, but per the cursor-move design appendix below that
primitive does not maintain curswant — the exact class of latent bug that
document exists to prevent. A following `j` may snap to a stale column.

**W2. `79e024d5` distinguishes a no-op delete by undo-stack depth.** It reads
`ed.undo_stack_len() > 1` as "the buffer was modified by a prior operation",
which is a proxy for the real rule rather than the rule itself. It makes the
oracle cases pass, but the register outcome of a no-op `dd` now depends on
whether _any_ prior undoable action occurred in the session, which is not what
vim keys off. Fixed by `b97e9bce`: `cut_vim_range` now uses the actual inverse
edit payload; an empty inverse leaves registers untouched.

**W3. `62bd4853` silently truncates oversized pastes.** A user asking for N
copies of a large register gets fewer, with no message. Silent partial execution
is arguably worse than refusing. Superseded by `b97e9bce`: oversized pastes are
now rejected outright rather than silently truncated, and no longer silently: it
reports vim's `E342` through the engine's message queue (2026-08-04).

#### Not covered by this pass

- **Non-ASCII.** `HjklOutcome.cursor` is a char index and `NvimOutcome.cursor`
  is a byte index, so the comparison is only sound on ASCII. This is exactly
  where the char-vs-grapheme column trap lives, and it is unaudited.
- **Ex commands** (`:`), search prompts (`/`, `?`) — the in-process hjkl driver
  cannot replay them.
- **Undo / redo** — nvim fixture seeding over RPC creates an undoable change, so
  `u` rolls back the fixture rather than the generated operation.
- **Folds** (`z`) and `gq` — excluded to avoid config-skew noise.
- The app / window layer, LSP, and everything above the engine.

### Code review — full-codebase (2026-07-29)

Tree clean, v0.39.0 + 13 commits. Reviewed via three read-only `explore`
sub-agents (hjkl-vim, hjkl-engine, hjkl-buffer) plus direct review of the recent
diff, curswant invariant, rope_util, and Move API. Every cited file:line was
re-read and every failure scenario traced end-to-end by hand.

#### Evidence for code-review backlog items

##### 1. `Move::Vertical` mixes display-column `sticky_col` with char-column cursor math

`crates/hjkl-engine/src/cursor_move.rs:86-98` — in production via `scroll_line`
(`crates/hjkl-engine/src/editor.rs:3122`):

`jump_cursor` stores `sticky_col` as a display column (line 2386:
`char_col_to_visual_col`). `Move::Vertical` reads it and uses it directly as a
char column to clamp against `max_col` (`buf_line_chars(...) - 1`, a char
count). On tab-indented lines, display col ≠ char col, so the cursor lands on
the wrong character.

`apply_sticky_col` in the vim motion path (`motion.rs:383`) correctly converts
back via `visual_col_to_char_col`; the `Move` API path does not, because it used
`want.min(max_col)` without the conversion.

```
Repro: tabstop=4, buffer "\tabcdef\n\txyz", cursor (0,2)='b'
        sticky_col = visual col 5 (from jump_cursor)
        <C-e> pushes cursor to row 1 via scroll_line
        → move_cursor(Move::Vertical { row: 1 })
        → want=5 (display), max_col=3 (chars-1), want.min(3)=3
        → cursor (1,3)='z'
Expect: cursor (1,2)='y' (visual col 5 on a tab+line = char col 2)
```

Remediation is tracked in the ranked backlog above. Fixed by `9a156885`.

##### 2. `outdent_rows` strips by character count, not visual column width

`crates/hjkl-vim/src/vim/text_object_ops.rs:403-407`:

`width` is computed in visual columns (`shiftwidth * count`), but
`line.chars().take(width)` limits by CHARACTER count. Tabs consume 1 char but
represent `tabstop` visual columns, so lines with tabs are over-stripped.

```
Repro: << on "\t\tfoo" (tabstop=4, shiftwidth=4, noexpandtab)
        width=4 (visual cols), line.chars().take(4)=['\t','\t','f','o']
        take_while(is_whitespace) → strip=2 → both tabs removed → "foo"
Expect: "\tfoo" (vim strips 4 visual cols = 1 tab)
```

Fixed by `9a156885`.

##### 3. `adjust_number_visual` ignores hex literals

`crates/hjkl-vim/src/vim/command.rs:1122-1123`:

Visual-mode `<C-a>`/`<C-x>` (`g<C-a>`, `g<C-x>`) scans for `is_ascii_digit()`
only — the `0` in `0xFF` matches, making it treat the hex prefix as a decimal
`0`. Normal-mode `adjust_number` (line 284) checks `is_hex_prefix(i)` first.

```
Repro: Vg<C-a> on "0xFF", cursor row 0
        → finds digit '0' at col 0, span_end stops at 'x'
        → s="0", n=0, replaces "0" with "1" → "1xFF"
Expect: hex increment → "0x100"
```

Fixed. The first pass added a hex branch to `adjust_number_visual` but left two
divergences that the normal-mode path did not have; both are closed by folding
the two implementations into one shared `adjusted_number_at` helper, so the
modes can no longer drift:

- Hex digit case was always lowercased (`0xAB` `<C-a>` → `0xac`). vim takes the
  case of the **last letter digit** of the original (`0xaB` → `0xAC`, `0xAb` →
  `0xac`), falling back to the `x`/`X` prefix's own case when the number has no
  letter digit (`0X19` → `0X1A`, `0x19` → `0x1a`). This one was wrong in normal
  mode too.
- Visual decimal dropped zero-padding (`007` `<C-a>` → `8` instead of `008`),
  which normal-mode `adjust_number` had handled since it was written.

##### 4. `:s` `/i` and `/I` flags ignore inline `\c`/`\C` overrides

`crates/hjkl-engine/src/substitute.rs:273-283`:

The `/i` and `/I` paths pass `CaseMode::Sensitive` as a dummy base to
`resolve_case_mode` and discard the returned mode (`(stripped, _)`), then
force-sensitise or wrap with `(?i)` unconditionally. The comment on line 272
says "matching vim's documented precedence: flag > inline override", but vim's
actual precedence is the reverse: inline `\c`/`\C` wins over the `/i`/`/I` flag.
(`:help /ignorecase`: `\c` overrides `'ignorecase'`, and `/I`/`/i` map to the
same toggle.)

```
Repro: :s/\cFOO/bar/I
        → \c stripped, /I path returns pattern as-is → case-sensitive
Expect: \c forces case-insensitive despite the I flag

Repro: :s/\CFOO/bar/i
        → \C stripped, /i wraps with (?i) → case-insensitive
Expect: \C forces case-sensitive despite the i flag
```

The same bug exists in `collect_substitute_matches` (lines 442-458). Fixed by
`9a156885`.

##### 5. `toggle_case_str` discards multi-character case mappings

`crates/hjkl-vim/src/vim/text_object_ops.rs:597-609` and
`crates/hjkl-vim/src/vim/command.rs:407-411`:

`to_uppercase()` and `to_lowercase()` return iterators that may yield multiple
chars (e.g. `ß` → `SS`, `İ` → `i\u{307}`). The code uses `.next().unwrap_or(c)`,
silently dropping all but the first output.

```
Repro: g~ on "Straße", cursor over 'ß'
        ß.to_uppercase() → ['S','S'], .next() → 'S'
        → "StraSe" (one character lost)
Expect: "STRASSE" (or "STRAẞE")
```

Minor — affects users whose text contains precomposed characters with multi-char
case mappings. Fixed by `9a156885`.

#### Hardening evidence

- **`Move::Vertical` bootstrap path** (`cursor_move.rs:89`): when `sticky_col`
  is `None`, the bootstrap uses `self.cursor().1` (char column) as `want`, but
  `want` is later compared against `max_col` (char count, ok) and used directly
  as cursor column (ok for the bootstrap case). The real bug is when
  `sticky_col` IS `Some` (display column) — see finding 1 (fixed by `9a156885`).

- **`prune_root_side` depth inconsistency with `clear_all`** (`undo.rs`):
  `clear_all` resets survivor depth to 0 (line 1105); `prune_root_side` does not
  (line 1032-1068). The naming remediation is tracked in the ranked backlog
  above.

- **`rope_line_char_count` / `rope_line_bytes` OOB panic** (`buffer.rs:806-815`,
  `794-803`): public functions without bounds checks. Current callers clamp the
  row; API hardening is tracked in the ranked backlog above.

- **`ensure_cursor_visible` top_row stale on shrink from other view**
  (`buffer.rs:189-191`): when `cursor_screen_row_from` returns `None`, only
  `top_col` is zeroed; `top_row` stays where it was, potentially leaving cursor
  invisible. Fixed. `e219e664` first assigned the raw `cursor.row`, which does
  not repair the shrink case at all: the guard above already establishes
  `cursor.row >= top_row`, so on a shrink (`last < top_row <= cursor.row`) that
  assignment can only move `top_row` further past the rope's end. `top_row` is
  now set to `cursor.row.min(last_row)`, which pulls it back into the live rope.
  The other `None` path — the cursor's row hidden inside a closed fold — is
  covered by the same assignment, and both paths now have a regression test.

### Code review — pending changes (2026-07-29)

**Scope:** pending unstaged change: a new proptest regression entry in
`crates/hjkl-vim-tui/tests/proptest_fsm.proptest-regressions` (+1 line, hash
`37433df2`). The regression was discovered by the `esc_returns_to_normal`
property test and also triggers in `no_panic_on_random_keys`.

**Method:** Traced each failing input from `handle_key` through
`crossterm_to_input`, `dispatch_input`, `step_insert`/`step_normal`,
`replay_last_change`, `finish_insert_session`, and the curswant invariant check.
Verified the same code-path reproduction with the exact shrunk sequences.

#### Evidence for pending-change backlog item

`crates/hjkl-vim/src/vim/dot_repeat.rs:205–226`

Dot-replay of an empty `ReplaceMode` session (user typed `R<Esc>` then `.`)
calls `push_undo()` (no buffer-content change → `dirty_gen` unchanged) and then
`move_left` (cursor moves but `sticky_col` is left stale). The debug-only
curswant invariant (`crates/hjkl-vim/src/curswant.rs:181`) catches this and
panics. In release builds the bug is silent but leaves `sticky_col` wrong,
causing `j`/`k` to snap to the pre-dot-repeat column instead of the current one.

The sequence `R` → `Esc` → `e` → `.` on `"hello world\nsecond line\n"`:

- `R` enters replace mode (`VimMode::Insert`)
- `Esc` exits without typing → `finish_insert_session` sets
  `last_change = ReplaceMode { text: "" }`, `sticky_col = Some(0)`
- `e` (word-end motion) moves cursor to (0,4), sets `sticky_col = Some(4)`
- `.` (dot-repeat) enters `replay_last_change`:
  - `push_undo()` — no dirty_gen change
  - `for ch in "".chars()` — loop body never executes, no dirty_gen change
  - `cursor.1 > 0` (4 > 0) → `move_left(buf, 1)` — cursor moves to (0,3)
  - `sticky_col` remains `Some(4)` ← **stale**

Curswant check fires: cursor moved from (0,4) to (0,3), dirty_gen unchanged,
mode unchanged, but `sticky_col == Some(4)` while `display_col == 3` — not a
vertical clamp (line has 5 chars, col 3 < 4).

```
Repro: replay_last_change with last_change = ReplaceMode { text: "" },
       cursor at (0, 4), sticky_col = Some(4)
Expect: sticky_col = Some(3) (or cursor unchanged)
Actual: sticky_col = Some(4), cursor at (0, 3)
       → debug-only panic in curswant::assert_invariant
       → release: stale sticky_col, next j/k snaps to column 4
```

The same bug also manifests when modifiers are present on the `.` key
(`KeyModifiers::ALT` or `KeyModifiers::SHIFT`) because the dot-repeat gate in
`step_normal` (`crates/hjkl-vim/src/normal.rs:463`) only checks `!input.ctrl`
and `input.key == Key::Char('.')` — it does not reject `alt` or `shift`.

#### Hardening evidence

- **Dot-repeat gate doesn't filter `alt`/`shift`** — `step_normal` line 463
  checks only `!input.ctrl && input.key == Key::Char('.')`. Real vim does not
  trigger dot-repeat on `Alt-.` or `Shift-.`. This divergence increases the
  input surface hitting this bug but is not itself a correctness defect.

- **Wasteful `push_undo()` on empty ReplaceMode replay** — `dot_repeat.rs:207`
  pushes an undo entry for a replay that performs zero buffer mutations. This is
  harmless but creates pointless undo-tree entries.

Both actions are tracked in the ranked backlog above. Fixed by `9a156885` (empty
replay) and `b97e9bce` (modifier rejection).

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
explains why every migrated motion requires an explicit variant; unfinished work
is tracked in the ranked backlog above.

`Raw` is deliberately conspicuous. Today the forgetful option is the one that
looks like the default; naming it inverts that, and it stays greppable for
review.

#### Phase-0 safety net

The unfinished migration and its acceptance criteria are tracked in the ranked
backlog above. Phase 0 shipped the debug invariant that guards that work: a site
classified into the wrong variant trips the assertion instead of shipping a
silent behavior change.

#### What phase 0 found

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

Observed phase-0 violation classes still relevant to migration:

| Class | Count | What                                                                                                                                                           |
| ----- | ----- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A     | 170   | Insert mode: every printable char, `Tab`, `<C-w>`. One systemic site.                                                                                          |
| B     | 66    | Normal-mode operators/edits: `y{motion}`, `d{motion}`, `J`, `p`/`P`, `x`/`X`, `~`, `>`, `.`, `u`, `<C-a>`. The operator path never reaches `apply_sticky_col`. |
| C     | 12    | Visual-mode `y` — invisible to a `dirty_gen` check since yank makes no edit.                                                                                   |

#### The real cost

The ~186 sites expose the migration's classification cost. A mechanical
translation to `Raw` would compile while preserving the bug class. Variant-count
reporting and raw-move justification are tracked in the ranked backlog above.

#### Invariants established by phases 0–1

- The compat oracle remained ALL-pass; its corpus was not edited to make either
  phase pass. It covers search/curswant directly with five expectations taken
  from headless nvim.
- The pty e2e suite served as the cursor-behavior safety net.
- Phase 0 was the only phase allowed to change behavior; later assertion changes
  indicate a migration misclassification.
