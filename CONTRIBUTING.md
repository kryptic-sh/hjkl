# Contributing to hjkl

Thanks for considering a contribution. hjkl is pre-1.0 and the trait surface is
still in motion — please open an issue before starting any non-trivial PR so the
design can be sanity-checked early.

## Development setup

```bash
git clone git@github.com:kryptic-sh/hjkl.git
cd hjkl
rustup toolchain install stable    # rust-toolchain.toml pins this for you
cargo test --workspace
```

## MSRV policy

`rust-version` in `Cargo.toml` tracks current stable Rust. Floor, not ceiling —
bumps land freely when new features are useful. Any bump must be logged in
`CHANGELOG.md` under the version that introduces it.

CI runs stable + beta on every PR. Nightly is reserved for `cargo fuzz` runs in
cron jobs only.

## Pull requests

- Branch from `main`. One logical change per PR.
- Commits: [Conventional Commits](https://www.conventionalcommits.org/) format.
  `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`, `ci`, `build`.
  Scope optional.
- Run before pushing:
  - `cargo fmt`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test --all-features`
- New public API needs rustdoc and (where applicable) a `///` example.
  `#![deny(missing_docs)]` is enforced on `hjkl-engine`.
- Performance-sensitive changes: include a criterion bench in
  `crates/<crate>/benches/`. CI fails if budgets regress (see `MIGRATION.md`
  "Performance Budgets").

## Snapshot tests

Golden tests use [`insta`](https://insta.rs/). After intentional output changes:

```bash
INSTA_UPDATE=always cargo test
# or, interactively:
cargo insta review
```

## Property + fuzz tests

- proptest regressions live in `proptest-regressions/`. Commit failing seeds so
  CI replays them.
- `cargo fuzz` harnesses run on cron with the nightly toolchain. Local
  reproduction:
  ```bash
  cargo +nightly fuzz run <target>
  ```

## Releases

`release-plz` automates lockstep version bumps + changelog + crates.io publish.
Manual approval gate on the release PR.

To **yank** a broken release:

```bash
cargo yank --version X.Y.Z -p <crate>
```

Yank ≠ delete: consumers pinned to `=X.Y.Z` still resolve. Document the reason
in `CHANGELOG.md` under a `### Yanked` heading for that version.

## Pre-1.0 stability

The 0.0.x series is a churn phase — breaking changes may land on patch bumps.
Lockstep workspace versions; all four crates publish together. Consumers should
pin with `=0.0.X`.

`cargo public-api` baseline is taken at the 0.1.0 release; from then on,
breaking changes require a minor bump and `cargo semver-checks` gates PRs.

## Reporting bugs / requesting features

Use the issue templates in `.github/ISSUE_TEMPLATE/`. For security issues, see
`SECURITY.md` — do not file public issues.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md).
