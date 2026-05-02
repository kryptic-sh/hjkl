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

Golden tests use [`insta`](https://insta.rs/) and live next to the unit tests
under `tests/snapshots/`. The first batch covers ex-command output
(`crates/hjkl-editor/tests/golden_ex.rs`): `:registers`, `:marks`, bare `:set`
listing. Add new snapshots there when you ship a user-visible text format
change.

After intentional output changes:

```bash
INSTA_UPDATE=always cargo test
# or, interactively:
cargo insta review
```

Commit the updated `*.snap` files alongside the change.

## Property + fuzz tests

- proptest regressions live in `proptest-regressions/`. Commit failing seeds so
  CI replays them.
- `cargo fuzz` harnesses live under each crate's `fuzz/` directory and run on
  cron with the nightly toolchain. Today the only target is `hjkl-engine/fuzz`
  :: `handle_key` — feeds an arbitrary keystroke stream into a fresh `Editor`
  and asserts no panics. Local reproduction:
  ```bash
  cd crates/hjkl-engine/fuzz
  cargo +nightly fuzz run handle_key
  ```

## Releases

Each `hjkl-*` crate lives in its own submodule and ships independently. Cutting
a release is the **BCTP** flow: bump the patch in `Cargo.toml`, regenerate
`Cargo.lock`, commit `chore: bump version`, tag `vX.Y.Z`, push commit + tag. The
tag triggers `release.yml` which publishes to crates.io.

Patch for bug fixes / docs; minor for additive public API; major for breaking
changes.

To **yank** a broken release:

```bash
cargo yank --version X.Y.Z -p <crate>
```

Yank ≠ delete: consumers pinned to `=X.Y.Z` still resolve. Document the reason
in `CHANGELOG.md` under a `### Yanked` heading for that version.

## Reporting bugs / requesting features

Use the issue templates in `.github/ISSUE_TEMPLATE/`. For security issues, see
`SECURITY.md` — do not file public issues.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md).
