# Docks as real layout leaves (per-tab), 2026-07-27

## Why

Docks (explorer, quickfix, location list) own a real `WindowId` + slot but are
deliberately **absent from every `LayoutTree`**. The stated benefit is that tree
operations "can't touch them by construction". The cost is that every consumer
of "windows" or "buffers" must re-derive dock-ness by hand:

- **76 non-test call sites** referencing `is_explorer` / `slot_is_special` /
  `is_dock_window`.
- **529 lines** of `dock.rs`, including `dock_neighbor_left/right/up/down` — a
  hand-rolled reimplementation of adjacency the tree already computes.
- `hjkl-layout` has no fixed-size or pinned concept, so there was nowhere else
  to put the specialness.

The failure mode is silent: forget a branch and nothing fails to compile, the
behaviour is just wrong. Six defects landed from this single root on 2026-07-27
alone — `:q` needing two presses, `:q` in the explorer deleting its buffer,
`<C-w>q` in a dock quitting the editor, `H`/`L` losing viewport motion, the
cmdline slot counting as a real buffer, and `<C-h>` trapped in the quickfix
dock. The last exists _only_ because adjacency was reimplemented; the tree's own
`neighbor_direction` handles the equivalent tree case correctly.

Vim does not work this way. Its quickfix window and netrw/nvim-tree are ordinary
windows; what makes them special is per-window/per-buffer attributes (`buftype`,
`winfixwidth`/`winfixheight`), not exile from the layout. Put the specialness in
attributes and the layout stays uniform.

## Implemented shape

1. **`hjkl-layout` learned fixed sizing and pinning.** A split can allocate one
   child a fixed cell count instead of a ratio; a leaf can be pinned so `:only`
   / `equalize_all` / `swap_with_sibling` leave it alone.
2. **Docks became ordinary pinned, fixed-size leaves**, woven into each tab's
   tree. Navigation, `:q`, `<C-w>c`/`<C-w>q`, `<C-w>w` cycling and rect
   computation flow through the single existing path.
3. **Docks became per-tab**, matching vim: each tab has its own explorer and its
   own quickfix window. This deliberately changed dock state from global.
4. **Buffer-list exclusion stayed a buffer property.** `is_explorer: bool` and
   the derived qf-dock-slot check became one `BufKind` on `BufferSlot`, where
   vim puts it (`buftype`).

## Design decisions

**Fixed sizing goes on the `Split`, not the `Leaf`.** `LayoutTree::Leaf` is a
tuple variant; giving it fields would touch every construction and match in the
workspace. The split already owns the geometry decision (`ratio`), so it is the
natural home:

```rust
pub enum Fixed { First(u16), Second(u16) }   // cells for `a` / for `b`

Split { dir, ratio, fixed: Option<Fixed>, a, b, last_rect }
```

`ratio` is retained for ordinary splits — replacing it wholesale would touch
**412 `ratio` references** across the workspace for no behavioural gain.
`fixed`, when set, wins over `ratio` in `collect_rects`.

**Adding the field is deliberately a breaking change.** All 19 construction
sites get a compile error until they pass `fixed: None`. That is the point: this
refactor exists because dock-ness was _silently_ forgettable, so the replacement
must be loud. A `LayoutTree::split(dir, ratio, a, b)` helper keeps future sites
short.

**Pinning is passed to the ops that need it**, not stored per leaf, for the same
tuple-variant reason: `only(keep, pinned)`, `equalize_all(pinned)`,
`swap_with_sibling(id, pinned)`. Signature changes are compiler-enforced.

**Per-tab docks replaced global docks.** `App.left_dock` / `App.bottom_dock`
(global) became per-tab state. Opening the explorer in tab 2 does not disturb
tab 1. Closing a tab disposes of its docks with the rest of its windows.

## Phases

All phases shipped as separate gated commits. The table records their scope.

| #   | Phase                                | Scope                                                                                                                                                             |
| --- | ------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | Layout primitives (done, `081d9151`) | `hjkl-layout`: `Fixed`, `fixed` field, `collect_rects` honouring it, pinned-aware `only`/`equalize_all`/`swap_with_sibling`, unit tests. No app behaviour change. |
| 2   | Render through the tree              | `render::frame` stopped carving dock rects by hand; docks received geometry from `window_rects` like every other window.                                          |
| 3   | Docks into the tree, per tab         | Explorer/quickfix became pinned fixed leaves in each tab's tree; `left_dock`/`bottom_dock` moved from `App` to `Tab`; navigation moved to `neighbor_direction`.   |
| 4   | `BufKind` on the slot                | `is_explorer` plus qf-slot derivation became one enum; `slot_is_special` became `slot.kind != Normal`.                                                            |
| 5   | Cleanup                              | Obsolete `dock.rs` and dock branches were deleted; docs and changelog were updated.                                                                               |

## Settled during execution

**`explorer.width` / `panel.height` become content sizes.** Today's hand-carved
dock rect treats the config value as the _allocation_: `explorer.width = 30`
renders a 29-column explorer plus a 1-column separator. Phase 1 defined
`Fixed(n)` as "renders exactly n cells", so Phase 3 has two options — pass
`width - 1` and keep today's pixels, or pass `width` and let the config value
mean what it says.

Decision: **pass `width`.** The explorer gains one column versus today. Keeping
the old pixels would mean re-introducing a `- 1` compensation at the dock site,
which is precisely the implicit, silently-forgettable knowledge this refactor
exists to delete — the same shape as the dock special-casing being removed. This
is a user-visible one-column change and belongs in the changelog as a fix: a
config value named `width` should produce that width.

**Fixed splits are not resizable.** `<C-w>+`/`<C-w><`/`<C-w>_` and border drags
skip a fixed split and move the nearest resizable ancestor instead — vim's
`winfixwidth`/`winfixheight`. Phase 3 preserved dock drag-to-resize through the
existing `resize_dock_width_by` path rather than routing dock borders through
the tree resize path.

## The hazard the refactor relocated (found in phase 3)

Docks used to be invisible to `layout().leaves()`, which made
`leaves().len() > 1` a safe proxy for "more than one editor". Now that docks are
leaves it counts them, so `:q` in the last editor with `:copen` open would close
the editor and strand the user in a quickfix list. Four sites now use
`regular_leaf_count()` instead, and `close_focused_window` needed an explicit
`<= 1 -> E444` pre-check because `remove_leaf` now succeeds where it used to
fail.

**This is the same silent-forgetting shape the refactor set out to delete,
relocated rather than removed** — the invariant is no longer structural, it is
four counting sites. Mitigation for now is
`no_close_path_leaves_a_dock_as_the_last_window`, which sweeps `:q`, `:q!`,
`:close`, `:only`, `<C-w>c`, `<C-w>q` and `<C-w>o` with both docks open and one
editor, and fails if any of them strands the user in a dock. **Resolved in phase
5** by `App::detach_focused_leaf() -> Result<WindowId, CloseRefused>`, now the
_only_ production reader of `regular_leaf_count` (verified by grep: one call
site). `move_window_to_new_tab` asks the same question without closing anything,
so the chokepoint is the leaf **detach**, not the close. The four sites
genuinely do want different responses to a refusal (E444 / E1 / quit / quit) —
the `Result` split centralises the decision while leaving the response local,
and `CloseRefused` is an enum so a second refusal reason becomes a compile error
at every site rather than a silently-wrong branch.

## Invariants preserved across the phases

- The nvim compat oracle stayed **ALL-pass**; its corpus was not edited to make
  a change pass.
- The pty e2e suite was the window-behavior safety net; unit tests had not
  caught the `<C-h>` dock trap, and a synthetic `App` reproduction disagreed
  with the pty until the status line was used as the focus signal.
- Dock windows never became the _last_ window: closing the final regular window
  with a dock open still quits.
- `:only` keeps pinned leaves; `<C-w>w` cycling includes docks, matching vim.

## Outcome

All five phases shipped 2026-07-27. Docks are ordinary pinned, fixed-size,
per-tab leaves; `dock_neighbor_*`, `focus_cycle_order` and the dock loop in
`hit_test_window` are deleted; `slot_is_special` is one field read.

Bugs found and fixed _because_ of the migration, none of which the original five
phases set out to fix:

- `:e <file>` from the quickfix panel loaded the file into the dock window while
  the dock record still claimed it — closing the panel then disposed of the
  user's buffer. Data loss, pre-existing.
- The command-line window removed its slot by an index recorded at open time,
  leaking the buffer after any earlier slot was removed — and the same stale
  index made the history buffer count as a real one (`:ls`, buffer line,
  `H`/`L`).
- `screen_rect` counted scratch buffers as real, assuming a tab bar the renderer
  had not drawn, so first-frame popups misjudged the height by a row.
- `q:` from a focused explorer would have split the dock's own leaf.
- Two positional slot scans would have silently rewritten the _other_ tab's tree
  once two tabs each had an explorer.

Two same-class defects found during phase 5 remain open:

- `write_swap_for_slot` guards on `is_explorer()` only, so `:copen` and `q:`
  scratch buffers get swap files written; a crash then offers to "recover" a
  quickfix listing.
- `quit_all` blocks on `dirty && !is_explorer()`, so a dirty quickfix or cmdline
  scratch slot makes `:qa` refuse with an unsatisfiable `E37 ... "[No Name]"`.

Their actions are tracked only in `docs/backlog.md`.
