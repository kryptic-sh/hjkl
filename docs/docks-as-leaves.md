# Docks as real layout leaves — remaining defects

Docks now participate in `LayoutTree`, but two app-layer guards still identify
only explorer buffers where they mean every special scratch buffer. This is the
same silently-forgotten-special-case failure mode the layout migration removed
elsewhere.

- `write_swap_for_slot` guards on `is_explorer()` only, so `:copen` and `q:`
  scratch buffers get swap files; a crash can then offer to “recover” a quickfix
  listing.
- `quit_all` blocks on `dirty && !is_explorer()`, so a dirty quickfix or cmdline
  scratch slot makes `:qa` refuse with unsatisfiable `E37 ... "[No Name]"`.

Actions are tracked in `docs/backlog.md`.
