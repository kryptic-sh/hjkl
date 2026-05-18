# hjkl-anvil

Mason-style LSP and developer-tool installer for the
[hjkl](https://hjkl.kryptic.sh) editor stack.

[![crates.io](https://img.shields.io/crates/v/hjkl-anvil.svg)](https://crates.io/crates/hjkl-anvil)
[![docs.rs](https://docs.rs/hjkl-anvil/badge.svg)](https://docs.rs/hjkl-anvil)
[![CI](https://github.com/kryptic-sh/hjkl-anvil/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl-anvil/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

## Overview

`hjkl-anvil` provides a self-contained tool-installation pipeline compatible
with [mason-org/mason-registry](https://github.com/mason-org/mason-registry)
manifests. It supports GitHub release tarballs/zips, `cargo install`, `npm
install`, `pip install`, and `go install` backends with SHA-256 verification,
atomic rename + symlink finalization, and a `.rev` sidecar for version
tracking.

## Modules

- **`manifest`** — parse and validate `anvil.toml` manifests (`Manifest`,
  `ToolSpec`, `InstallMethod`, `ManifestError`).
- **`registry`** — in-process tool registry backed by the embedded
  `anvil.toml` (`Registry`, `RegistryError`).
- **`store`** — XDG-aware path layout helpers and atomic `rev` sidecar
  read/write (`RevSidecar`, `data_root`, `packages_dir`, `bin_dir`, …).
- **`installer`** — `Install` trait, per-backend pipelines, `install_blocking`
  dispatcher, path-traversal guard (`safe_join`), `InstallStatus` enum.
- **`job`** — `InstallPool` (2-thread), `InstallHandle`, per-key deduplication.

## Feature flags

| Flag   | Description                                                         |
|--------|---------------------------------------------------------------------|
| `sync` | Enables the `sync-anvil` maintainer binary that syncs the embedded `anvil.toml` from upstream mason-org releases. **Not for downstream consumers.** |

## Usage

```toml
[dependencies]
hjkl-anvil = "0.1"
```

```rust
use hjkl_anvil::{registry::Registry, installer::install_blocking};

let reg = Registry::embedded();
let tool = reg.get("lua-language-server").unwrap();
install_blocking(&tool).unwrap();
```

## License

MIT — see [LICENSE](LICENSE).
