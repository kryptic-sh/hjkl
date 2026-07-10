# Contributing to hjkl

`hjkl` is a Cargo workspace (monorepo) of ~60 `hjkl-*` crates plus the `hjkl`
and `hjkl-gui` apps. Everything lives in this one repository — the crates are no
longer separate repos, though each still publishes independently to crates.io.

Most crates are pre-1.0 and their public APIs are still in motion — please open
an issue before starting any non-trivial PR so the design can be sanity-checked
early.

## Development setup

```bash
git clone git@github.com:kryptic-sh/hjkl.git
cd hjkl
rustup toolchain install stable   # rust-toolchain.toml pins the exact version
cargo test --workspace
```

To work on a single crate, scope the commands with `-p`:

```bash
cargo test -p hjkl-clipboard
cargo clippy -p hjkl-clipboard --all-targets -- -D warnings
```

`cargo nextest run` is the canonical test runner in CI (`.config/nextest.toml`
serializes the few tests that touch process-global state); plain `cargo test`
also works.

## Pull requests

- Branch from `main`. One logical change per PR.
- Commits use [Conventional Commits](https://www.conventionalcommits.org/):
  `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`, `ci`, `build`.
  Scope optional (e.g. `fix(hjkl-buffer): …`).
- Run before pushing:
  - `cargo fmt --all --check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test --workspace`
  - `prettier --write` on any Markdown you touch.
- New public API needs rustdoc and, where applicable, a `///` example.
  `#![deny(missing_docs)]` is enforced on `hjkl-engine`.

## MSRV policy

`rust-version` in each `Cargo.toml` tracks current stable Rust. It is a floor,
not a ceiling — bumps land freely when new features are useful, and each bump is
logged in the affected crate's `CHANGELOG.md`.

## Releases

Per-crate patch bumps follow the BCTP flow (Bump → Commit → Tag → Push). Crates
share the workspace version; lockstep bumps are preferred when a change touches
shared types.

To **yank** a broken release:

```bash
cargo yank --version X.Y.Z --package hjkl-<name>
```

Yank ≠ delete: consumers pinned to `=X.Y.Z` still resolve. Document the reason
in that crate's `CHANGELOG.md` under a `### Yanked` heading.

## Pre-1.0 stability

Pre-1.0, breaking changes may land on minor bumps per Cargo's SemVer rules for
`0.x`. Consumers can pin tighter if needed.

## Code of Conduct

This project follows the
[Contributor Covenant](https://www.contributor-covenant.org/version/2/1/code_of_conduct/).

## Security

Do **not** file public issues for vulnerabilities. See the org-wide
[SECURITY policy](https://github.com/kryptic-sh/.github/blob/main/.github/SECURITY.md),
or email `mxaddict@kryptic.sh` directly.
