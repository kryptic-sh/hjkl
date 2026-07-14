# hjkl-helix

Helix-style, **selection-first** keyboard discipline for the
[hjkl](https://hjkl.kryptic.sh) editor stack.

The second discipline to run on `hjkl-engine`, after `hjkl-vim` — and the proof
that the engine is genuinely grammar-agnostic. It implements one trait
(`hjkl_engine::DisciplineState`) and drives the editor through its public API.
Nothing in `hjkl-engine` knows this crate exists.

## Selection model

Helix is selection-first: every motion produces a selection `(anchor, head)`,
and operators act on it. `w` _selects_ the word rather than just moving the
caret; `d` deletes whatever is selected.

The engine stores the **heads** — the primary cursor plus its secondary
selections. This crate owns the primary's **anchor**; the engine owns the
secondaries' anchors, so an anchor can never desync from its head across an
edit.

Normal vs Select is one rule: after a motion, Normal collapses the anchor onto
the head (the selection is _replaced_), Select leaves it (the selection is
_extended_).

## Multi-cursor

Multi-cursor is not a bolt-on — every motion and operator runs at **every**
selection. `C` adds a cursor below, typing lands at all of them, and one `u`
undoes the lot.

```
C C        three cursors down the file
w          selects a word at each
d          deletes all three
```

## Status

Pre-1.0, and not yet feature-parity with Helix. Implemented: counts, `h j k l`,
`w b e W B E`, `f t F T`, goto mode (`gg ge gh gl gs`, `G`), `v`, `x`/`X`, `%`,
`;` `,`, `d c y p P i a I A o O r ~ J > <`, `u`/`U`, `C`/`Alt-C`, `(`/`)`.

Not implemented: the select-regex family (`s` `S` `&` `_`), match mode (`mi`
`ma` `mm`), search (`/` `?` `n` `N`), scrolling, sub-word motions.

## License

MIT
