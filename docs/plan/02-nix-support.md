# 02 — Nix support

Requested in [issue #324](https://github.com/kryptic-sh/hjkl/issues/324): Nix
and NixOS users have no way to build, install or develop hjkl from the repo.
This phase adds a flake at the repo root. It is purely additive: no existing
build script, CI job or release step changes.

## Outputs

For `x86_64-linux`, `aarch64-linux` and `aarch64-darwin`. Not `x86_64-darwin`:
nixpkgs-unstable no longer evaluates for it, and the last release branch that
does only gets fixes until the end of 2026.

- `packages.default` / `packages.hjkl` — the `hjkl` binary built with
  [crane](https://github.com/ipetkov/crane) from `Cargo.lock`, version read from
  the workspace manifest rather than written into the flake.
- `apps.default` — `nix run github:kryptic-sh/hjkl`.
- `overlays.default` — adds `hjkl` to a nixpkgs set.
- `devShells.default` — the Rust toolchain from
  [fenix](https://github.com/nix-community/fenix) (stable, with clippy, rustfmt,
  rust-src and rust-analyzer, at least the workspace `rust-version`), plus the
  tools the README's development section and CI use (cargo-nextest,
  cargo-machete, prettier, git, a C compiler).
- `nixosModules.default` — `programs.hjkl.{enable, package, defaultEditor}`.
- `homeManagerModules.default` —
  `programs.hjkl.{enable, package, settings, defaultEditor}`, where `settings`
  is written to `$XDG_CONFIG_HOME/hjkl/config.toml` through `pkgs.formats.toml`.
- `checks` — the package build, a smoke test running the built binary, and an
  evaluation of each module.
- `formatter` — `nixfmt`.

## Runtime dependencies

hjkl shells out at runtime, so the package must provide what it calls:

- `git` — the git pickers, hunk staging and commit (`hjkl-app` `git.rs`), and
  the grammar fetcher (`hjkl-bonsai` `runtime/source.rs`).
- a C/C++ compiler — `hjkl-bonsai` compiles tree-sitter grammars on first use
  (`runtime/compile.rs`).

The binary is wrapped so both are on its `PATH`, without overriding a user's own
`$CC`.

## Build notes

- Source filtering must keep the non-Rust files compiled in with `include_str!`
  (`bonsai.toml`, `anvil.toml`, the `.scm` queries, `art/`), or the build fails
  with a missing file.
- The package build skips the test suite: CI's `test` job already runs it on
  every OS, and parts of it need a terminal and network that a Nix sandbox does
  not have.

## CI

A separate `nix.yml` workflow runs `nix flake check` and builds the package on
Linux and macOS for pull requests and pushes to `main`. It is not a dependency
of the release jobs in `ci.yml`, so a Nix-only failure never blocks a release.

## Known limits

- `hjkl-anvil` installs language servers by downloading prebuilt binaries. Those
  are dynamically linked for a conventional Linux layout and will not run on
  NixOS without `nix-ld`. Nix users install language servers through Nix
  instead.
- Grammars are still fetched and compiled at runtime, into the user's cache, as
  on every other platform. Prebuilding them in the Nix store is possible later
  but out of scope here.

## Done when

- `nix build`, `nix run` and `nix develop` work from a clean checkout.
- `nix flake check` passes on Linux and macOS, locally and in CI.
- Both modules evaluate, and the home-manager `settings` option produces the
  expected `config.toml`.
- The README documents installing with Nix; the changelog records the flake.
