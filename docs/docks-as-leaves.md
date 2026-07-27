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

## Target shape

1. **`hjkl-layout` learns fixed sizing and pinning.** A split may allocate one
   child a fixed cell count instead of a ratio; a leaf may be pinned so `:only`
   / `equalize_all` / `swap_with_sibling` leave it alone.
2. **Docks become ordinary pinned, fixed-size leaves**, woven into each tab's
   tree. Navigation, `:q`, `<C-w>c`/`<C-w>q`, `<C-w>w` cycling and rect
   computation then all flow through the single existing path.
3. **Docks become per-tab**, matching vim: each tab has its own explorer and its
   own quickfix window. This is a deliberate behaviour change — dock state is
   currently global across tabs.
4. **Buffer-list exclusion stays a buffer property.** `is_explorer: bool` and
   the derived qf-dock-slot check become one `BufKind` on `BufferSlot`, which is
   where vim puts it (`buftype`). This is not a layout concern.

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

**Per-tab docks replace global docks.** `App.left_dock` / `App.bottom_dock`
(global) become per-tab state. Opening the explorer in tab 2 does not disturb
tab 1. Closing a tab disposes of its docks with the rest of its windows.

## Phases

Each phase is one commit, gated (`clippy -D warnings`, `fmt`, full `nextest`
incl. the pty e2e suite, compat oracle ALL-pass) and pushed with CI green before
the next begins.

| #   | Phase                                | Scope                                                                                                                                                                                               |
| --- | ------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | Layout primitives (done, `081d9151`) | `hjkl-layout`: `Fixed`, `fixed` field, `collect_rects` honouring it, pinned-aware `only`/`equalize_all`/`swap_with_sibling`, unit tests. No app behaviour change.                                   |
| 2   | Render through the tree              | `render::frame` stops carving dock rects by hand; docks get their geometry from `window_rects` like every other window.                                                                             |
| 3   | Docks into the tree, per tab         | Explorer/quickfix become pinned fixed leaves in the active tab's tree; `left_dock`/`bottom_dock` move from `App` to `Tab`. Delete `dock_neighbor_*`; navigation flows through `neighbor_direction`. |
| 4   | `BufKind` on the slot                | Replace `is_explorer` + qf-slot derivation with one enum; `slot_is_special` becomes `slot.kind != Normal`.                                                                                          |
| 5   | Cleanup                              | Delete what `dock.rs` no longer needs; drop dock branches from `:q`, `close_focused_window`, `QuitOrClose`, `H`/`L`; update docs + changelog.                                                       |

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
`winfixwidth`/`winfixheight`. Docks keep drag-to-resize through the existing
`resize_dock_width_by` path, which Phase 3 must preserve rather than routing
dock borders through the tree resize path.

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
editor, and fails if any of them strands the user in a dock. Phase 5 should
consider a single `close_window_checked` chokepoint so the count cannot be
forgotten at a new call site.

## Invariants that must hold at every phase

- The nvim compat oracle stays **ALL-pass**; its corpus is never edited to make
  a change pass.
- The pty e2e suite is the real safety net for window behaviour — unit tests did
  not catch the `<C-h>` dock trap, and a synthetic `App` reproduction of the
  3-window navigation report disagreed with the pty until the status line was
  used as the focus signal.
- Dock windows must never become the _last_ window: closing the final regular
  window with a dock open must still quit (regression from 2026-07-27).
- `:only` keeps pinned leaves; `<C-w>w` cycling includes docks (vim includes
  special windows in the cycle).
