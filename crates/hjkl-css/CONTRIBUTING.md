# Contributing to `hjkl-css`

Thanks for considering a contribution. `hjkl-css` is pre-1.0 and the public API
is still in motion — please open an issue before starting any non-trivial PR so
the design can be sanity-checked early.

## Development setup

```bash
git clone git@github.com:kryptic-sh/hjkl-css.git
cd hjkl-css
rustup toolchain install stable
cargo test --all-features
```

## MSRV policy

`rust-version` in `Cargo.toml` tracks current stable Rust. Floor, not ceiling —
bumps land freely when new features are useful. Any bump must be logged in
`CHANGELOG.md` under the version that introduces it.

## Pull requests

- Branch from `main`. One logical change per PR.
- Commits: [Conventional Commits](https://www.conventionalcommits.org/) format.
  `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`, `ci`, `build`.
  Scope optional.
- Run before pushing:
  - `cargo fmt --all --check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test --all-features`
- New public API needs rustdoc and (where applicable) a `///` example.

## Releases

Patch bumps follow the BCTP flow (Bump → Commit → Tag → Push). Adapter crates
(`hjkl-css-floem`) track this crate's minor cadence when changes touch the AST
or cascade surface.

To **yank** a broken release:

```bash
cargo yank --version X.Y.Z
```

Yank ≠ delete: consumers pinned to `=X.Y.Z` still resolve. Document the reason
in `CHANGELOG.md` under a `### Yanked` heading for that version.

## Pre-1.0 stability

Pre-1.0, breaking changes may land on minor bumps per Cargo's SemVer rules for
`0.x`. Adapter crates should pin `=X.Y` when the cascade or AST shape matters;
caret (`"0.x"`) is fine when consumers only depend on the public parser API.

## Reporting bugs / requesting features

Open a GitHub issue. For security issues, see `SECURITY.md` — do not file public
issues.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md).
