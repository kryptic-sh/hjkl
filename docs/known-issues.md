# Known issues

Actionable, unresolved items for `hjkl` are now tracked as
[GitHub issues](https://github.com/kryptic-sh/hjkl/issues). Every vim-parity
divergence found during the compatibility rounds has been fixed and shipped on
`main`; the remaining security items were migrated to the tracker:

- [#314](https://github.com/kryptic-sh/hjkl/issues/314) — S1 [HIGH] arbitrary
  native code execution via grammar dlopen (not remotely reachable while the
  manifest is bundled; harden before allowing a user-supplied `bonsai.toml`).
- [#313](https://github.com/kryptic-sh/hjkl/issues/313) — S3 [LOW]
  `Buffer::line()` panics on an out-of-bounds row.

Intentional trade-offs (deliberate no-ops, safer-than-nvim choices) and
engine-limited impossibilities are **not** tracked — only things worth actually
fixing.
