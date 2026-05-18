# Contributing to `hjkl-css-gui`

Thanks for considering a contribution. `hjkl-css-gui` is pre-1.0 and the public
API is still in motion — please open an issue before starting any non-trivial PR
so the design can be sanity-checked early.

## Development setup

```bash
git clone git@github.com:kryptic-sh/hjkl-css-gui.git
cd hjkl-css-gui
rustup toolchain install stable
cargo test --all-features
```

Note: building against the layer-shell floem fork requires the workspace-root
`[patch.crates-io]` block documented in the README. Stock `floem 0.2` from
crates.io is sufficient for adapter-side tests.

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

Patch bumps follow the BCTP flow (Bump → Commit → Tag → Push). The `hjkl-css`
dep is pinned by `tag = "vX.Y.Z"`; bump the tag in lockstep with upstream
`hjkl-css` releases that touch the AST or cascade surface.

To **yank** a broken release:

```bash
cargo yank --version X.Y.Z
```

## Pre-1.0 stability

Pre-1.0, breaking changes may land on minor bumps per Cargo's SemVer rules for
`0.x`.

## Reporting bugs / requesting features

Open a GitHub issue. For security issues, see `SECURITY.md` — do not file public
issues.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md).
