# hjkl — review backlog

Single source for open findings, deferred decisions, and blocked work. Findings
use symbol names rather than line numbers so references survive refactors.

## 1. Open work — ranked

### 1.0 v0.41.0 is published everywhere except AUR

`hjkl-bin` on the AUR is still `0.40.0-1`. The `Publish hjkl-bin to AUR` job in
run `30858075595` failed twice — both times at `git clone`, on AUR's
`The AUR is down due to maintenance. We will be back soon.`, still down an hour
later. Nothing in this repo is wrong and nothing is half-written: the job clones
before it stages, and it guards its own push with `git diff --cached --quiet` →
`exit 0`, so it is safe to re-run any number of times.

To finish: `gh run rerun 30858075595 --failed`, once
`git ls-remote ssh://aur@aur.archlinux.org/hjkl-bin.git` succeeds. Check THAT,
not `https://aur.archlinux.org/` and not `ssh aur@aur.archlinux.org` — both
answer normally while the git service is in maintenance, which is what made the
first re-run premature. Verify after with the RPC:
`curl 'https://aur.archlinux.org/rpc/v5/info?arg[]=hjkl-bin'`.

Everything else in v0.41.0 landed and was verified against the registries rather
than the job statuses: 59/59 crates at 0.41.0 on crates.io, the GitHub release
live with 24 assets, Homebrew published, Alpine apk built, all seven build
targets green.

### 1.1 Undo-tree follow-ups

- `lowest_offpath_leaf` / `prune_root_side` still test path membership with
  `current_path().contains(..)` rather than the `UndoNode::on_path` flag that
  now exists, which would make `cap` O(N) instead of O(N\*depth). `cap` is not
  benched, so this was left out to keep the change confined.
- `retarget_current` is O(tree distance), not O(1): a `g-` crossing to a far
  branch still walks to the fork. Those ancestors genuinely have to change, so
  the worst case stays O(depth) for an adversarial branch layout. Every benched
  case, and `u` / `<C-r>`, are distance 1.
- **A freshly deserialized undofile has no keyframes**, so its first deep jump
  is O(depth). Eager construction may waste work and memory; not measured.
- `from_serializable` normalises `last_child` along the loaded root->current
  chain. A file written by `to_serializable` already agrees, so it is a no-op
  there — but a hand-edited or truncated undofile used to be repaired by the
  next full-chain rewrite, and nothing repairs it now.
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

| Item                                                | Where                                       | Note                                                                                                                                                                                 |
| --------------------------------------------------- | ------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| LSP full-sync still copies once                     | `hjkl-lsp/src/runtime.rs`, `server.rs`      | `Buffer::content_joined()` caches the `Arc`, so `Arc::unwrap_or_clone` cannot move. Avoiding the copy requires direct serialization instead of an intermediate `serde_json::Value`.  |
| `attach_buffer` copies at the boundary              | `hjkl-lsp/src/manager.rs` (`attach_buffer`) | Takes `text: &str` and calls `text.to_string()`. Change the boundary ownership model.                                                                                                |
| `styled_spans` cannot be removed — `sqeel` reads it | `hjkl-engine/src/editor.rs`                 | `sqeel-tui` pins published `hjkl-engine` and reads the field (one `mem::take`, two `clone`s). Removing it needs a supported accessor in `sqeel` first. The field documents this now. |

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

### 1.5 Remaining differential-oracle divergences

Every entry below was reproduced through
`cargo run -p hjkl-compat-oracle --release --example dfcase` against neovim
0.12.4 and left unfixed on purpose. Preserve each fixed case in the tier-2
compatibility corpus and verify it against nvim before changing expectations.

### 1.5b Left open by the motion- and blockwise-parity passes

**A text object in blockwise visual collapses the block (2026-08-04).**
`editor_ext::visual_text_obj_extend` sends EVERY text object down one path —
collapse to charwise (or linewise), set `visual_anchor`, move the cursor — and
never writes `block_anchor` / `block_vcol`, which are what `block_bounds` reads.

Measured on neovim 0.12.4, entering with `<C-v>j` and then the object, there are
THREE behaviours:

| Objects                       | neovim                                                 |
| ----------------------------- | ------------------------------------------------------ |
| `iw` `aw` `iW` `aW` `ip` `is` | stays BLOCKWISE; rows kept, cursor extends the columns |
| `ib` `ab` `iB`                | collapses to charwise AND to the cursor's single row   |
| `i"` `it`                     | no-op — the object does not apply                      |

hjkl does the middle one for all of them. For the word objects the cursor
already lands exactly where nvim puts it (verified for `<C-v>iw` and `<C-v>jiw`)
— only the MODE is wrong, so that part is small: keep `FsmMode::VisualBlock` and
write `block_vcol`. The bracket objects have a second, separate divergence:
`<C-v>jib` leaves hjkl's cursor on row 1 where nvim puts it on row 0.

This is what stops `<C-v>iw<` from reaching the blockwise `>` / `<` arm, so it
still outdents the whole line even though the shift geometry underneath is
correct:

```
`<C-v>iw<` on "\t(x).[y]", cursor (0,6)
hjkl: "(x).[y]"     ← outdented the whole line
nvim: "\t(x).[y]"   ← unchanged
```

Not attempted: it needs per-object routing plus a re-verification of every case
in `corpus/tier2_block_textobj.toml`, which is its own change.

**`H` / `L` / `gE` in blockwise visual still diverge on motion and cursor.**
`<C-v>H>` is the standing repro. Blockwise `~` is correct.

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

**Left open by the blockwise-paste rewrite (2026-08-02).**

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

**The engine message queue is deliberately dumb.** `Editor::push_error` /
`take_errors` carries vim's `E342` out of `do_paste` and `apps/hjkl` drains it
in both post-mutation sync paths (`App::drain_engine_errors`). An empty register
is still silent (vim says nothing there either), and nothing rate-limits the
queue, so code that pushes per-iteration would flood the toast bar.

**A corpus case cannot pin a value when nvim is absent.** Every oracle test
skips wholesale without nvim, so the corpus expectations guard nothing on a
machine that has no neovim — including CI lanes that do not install it. The
`expected_*` fields are all checked against nvim's outcome
(`diff.rs::run_single`), but nothing compares hjkl against the authored values
on its own.

**The `:s` curswant fix has no oracle case.** It is covered by two unit tests in
`hjkl-engine`'s `substitute` module, because the corpus driver cannot replay `:`
keys. A corpus case would need the ex layer driven some other way.

**The differential fuzzer still reports 77 divergences at seed 777** (89 before
the 2026-08-02 pass, 84 after it, then 83 with the counted-`$` failure rule, 78
with the backward-word-motion rewrite and 77 with the blockwise-shift geometry,
all 2026-08-04). The bulk are the known-excluded classes named in §5 (`u` / undo
against the seeded nvim fixture, blockwise `<C-v>` non-delete operators) plus
the entries above.
`cargo run -p hjkl-compat-oracle --release --example difffuzz -- 400 777`
reproduces the list; build with `--examples`, not `--example difffuzz`, or the
other binary goes stale.

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

- Clear nvim undo history after fixture seeding, then fuzz undo/redo. Add
  ex/search, fold, and `gq` coverage.
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

- **Ex commands** (`:`), search prompts (`/`, `?`) — the in-process hjkl driver
  cannot replay them.
- **Undo / redo** — nvim fixture seeding over RPC creates an undoable change, so
  `u` rolls back the fixture rather than the generated operation.
- **Folds** (`z`) and `gq` — excluded to avoid config-skew noise.
- The app / window layer, LSP, and everything above the engine.

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
