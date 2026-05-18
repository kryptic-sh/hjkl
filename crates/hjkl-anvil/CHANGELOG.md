# Changelog

All notable changes to `hjkl-anvil` are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.0.0/). Versioning:
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.4] - 2026-05-17

### Fixed

- Mark `tests/install_tests.rs`'s `ENV_LOCK` static and `HELLO_ZIP_SHA` constant
  `#[allow(dead_code)]` — they're only referenced by the unix-gated integration
  tests, so the Windows build of the test target rejected them under
  `RUSTFLAGS=-D warnings`. Missed in 0.2.3 (which only patched the
  `src/installer.rs` siblings of the same shape).

## [0.2.3] - 2026-05-17

_Tagged but never published (test target in `tests/install_tests.rs` had the
same dead_code issue as 0.2.2; superseded by 0.2.4._

### Fixed

- Gate 8 symlink-dependent tests (6 in `installer::tests`, 2 in
  `tests/install_tests.rs`) behind `#[cfg(unix)]`. The runtime `atomic_symlink`
  returns `UnsupportedPlatform` on non-unix, so these tests were guaranteed-fail
  on Windows once #110 un-ignored them. Restores Windows CI without re-ignoring;
  the unix paths still get full coverage via the `anvil-env` nextest group.
- Mark `src/installer.rs`'s `HELLO_ZIP_SHA` and `make_cargo_spec`
  `#[allow(dead_code)]` since they're only referenced from the now-unix-gated
  tests; otherwise `RUSTFLAGS=-D warnings` fails the Windows build.

## [0.2.2] - 2026-05-17

_Tagged but never published (Windows test compilation rejected `dead_code`
warnings under `-D warnings` after the test-gating; superseded by 0.2.3._

## [0.2.1] - 2026-05-17

### Changed

- Un-ignored 16 env-mutating tests (9 in `store`, 7 in `installer`) that were
  previously guarded by `#[ignore]` and required a manual
  `--include-ignored --test-threads=1` invocation. They now run automatically
  under the `anvil-env` nextest group (`max-threads = 1`) defined in the
  umbrella workspace's `.config/nextest.toml`, giving CI real coverage of the
  XDG path-resolution and full-pipeline paths.

## [0.2.0] - 2026-05-12

### Added

- **`ChecksumSidecar`** (`store`) — public type persisting per-tool TOFU hashes
  at `$XDG_DATA_HOME/anvil/checksums/<tool>.toml`. Format:
  `[versions."<v>".sha256] "<triple>" = "<64-hex>"`. Supports multiple versions
  and triples per file.
- **`checksums_dir()`** (`store`) — path helper mirroring `packages_dir`;
  creates the directory on first call.
- **`ExpectedSha`** (`installer`) — enum with three variants: `Pinned(String)`,
  `Cached(String)`, `Tofu`. Returned by `resolve_expected_sha`.
- **`resolve_expected_sha(gh, tool, version, triple)`** (`installer`) — resolves
  which checksum strategy to use: manifest pin → sidecar cache → TOFU first-use.
- **`InstallStatus::TofuRecorded { triple, sha256 }`** — emitted when a TOFU
  hash is recorded for the first time.

### Changed

- **TOFU is now the default** for github installs when the manifest `sha256`
  entry for a triple is `""` or the triple is absent from the map. The first
  download's hash is recorded in `ChecksumSidecar`; subsequent installs enforce
  it (prevents substitution attacks after first use).
- **Manifest validation** (`manifest`) now enforces that each `sha256` value in
  a `[tool.X.sha256]` table is either `""` (TOFU opt-in) or exactly 64 lowercase
  hex chars that are not all-zero. All-zero placeholder strings (`"000...0"`)
  and junk values are rejected at parse/validate time with the new
  `ManifestError::InvalidSha256 { tool, triple, value }` variant.
- **`anvil.toml`** — replaced 3 all-zero sha256 placeholder values with `""`
  (empty string) so the manifest validator accepts them and the installer falls
  through to TOFU on first use.

### Fixed

- `rust-analyzer`, `lua-language-server` installs no longer fail with
  `MissingChecksum` or `ChecksumMismatch` due to placeholder zero SHAs. TOFU
  records the real hash on first download and enforces it on subsequent
  installs.
- `sync-anvil` already emitted `""` for triples with no upstream checksum. A
  test now explicitly asserts this (preventing future regression to
  `"000...0"`).

### Removed

- `InstallError::MissingChecksum` is **no longer reachable** via the github
  install path (the variant is kept for backward ABI compatibility with any
  external code that matches on it, but the github installer never emits it).

## [0.1.1] - 2026-05-10

### Fixed

- **Windows build**: `atomic_symlink` no longer triggers `unreachable_code` and
  `unused_variables` lints under `RUSTFLAGS=-D warnings` on Windows. The
  `std::fs::rename` call is now gated inside the `#[cfg(unix)]` block; the
  `#[cfg(not(unix))]` arm suppresses the unused `target` binding with
  `let _ = target` and exits directly with `Err(InstallError::Archive(...))`.
  Behaviour on Unix is unchanged.
- **cargo-deny**: extended `[licenses] allow` to cover `CDLA-Permissive-2.0`,
  which is required by `webpki-roots v1.0.7` (transitive dependency via
  `reqwest → hyper-rustls → webpki-roots`). No other new licenses were added.

## [0.1.0] - 2026-05-10

### Added

#### `manifest`

- `Manifest` — top-level container parsed from `anvil.toml`; holds
  `ManifestMeta` and a list of `ToolSpec` entries.
- `ManifestMeta` — `name` + `description` header fields.
- `ToolSpec` — per-tool record: `name`, `description`, `category`,
  `install_methods`, optional `bin` override.
- `ToolCategory` — enum classifying tools: `Lsp`, `Linter`, `Formatter`, `Dap`,
  `Runtime`, `Other`.
- `InstallMethod` — six variants:
  - `Github { repo, asset_pattern, strip_prefix, bin }` — download a GitHub
    release asset (tarball, zip, or raw binary), SHA-256 verify, unpack, and
    symlink.
  - `Cargo { package, bin }` — `cargo install`.
  - `Npm { package, bin }` — `npm install -g`.
  - `Pip { package, bin }` — `pip install`.
  - `GoInstall { package, bin }` — `go install`.
  - `Script { script }` — reserved; always returns `UnsupportedMethod` until the
    vetting policy ships.
- Per-method structs for `Github`, `Cargo`, `Npm`, `Pip`, `GoInstall`, `Script`
  holding their respective fields.
- `ManifestError` — six variants: `Io`, `ParseToml`, `MissingField`,
  `InvalidCategory`, `InvalidMethod`, `Validation`.
- `parse_str(&str) -> Result<Manifest, ManifestError>` — parse TOML string.
- `load(path: &Path) -> Result<Manifest, ManifestError>` — read + parse file.
- `validate(manifest: &Manifest) -> Result<(), ManifestError>` — semantic checks
  (duplicate names, empty asset patterns, …).

#### `registry`

- `Registry` — in-process tool catalogue.
  - `Registry::new(manifest: Manifest)` — construct from a parsed manifest.
  - `Registry::embedded()` — construct from the in-tree `anvil.toml` via
    `include_str!`; ships a curated subset of mason-org tools.
  - `Registry::names() -> &[String]` — sorted tool names.
  - `Registry::get(name: &str) -> Option<&ToolSpec>` — exact-match lookup.
  - `Registry::by_category(cat: ToolCategory) -> Vec<&ToolSpec>` — filter by
    category.
  - `Registry::len() -> usize` — total tool count.
- `RegistryError` — wraps `ManifestError` for registry-construction failures.

#### `store`

- Path layout helpers (resolves XDG dirs via `hjkl-xdg`):
  - `data_root() -> PathBuf` — `$XDG_DATA_HOME/hjkl/anvil`.
  - `cache_root() -> PathBuf` — `$XDG_CACHE_HOME/hjkl/anvil`.
  - `packages_dir() -> PathBuf` — `data_root()/packages`.
  - `package_dir(name: &str) -> PathBuf` — per-tool directory.
  - `bin_dir() -> PathBuf` — `data_root()/bin` (symlink farm).
  - `rev_file(name: &str) -> PathBuf` — `package_dir(name)/.rev`.
- `RevSidecar` — typed wrapper for the installed version string; `parse` /
  `to_string` round-trip.
- `read_rev(name: &str) -> Option<RevSidecar>` — reads `.rev` if present.
- `write_rev(name: &str, rev: &RevSidecar)` — atomic staging-file + rename.

#### `installer`

- `Install` trait — `fn install(&self, tool: &ToolSpec) -> InstallStatus`.
- `InstallStatus` — channel-status enum: `Pending`, `Downloading`, `Unpacking`,
  `Linking`, `Done`, `Failed(String)`, `UnsupportedMethod`.
- `install_blocking(tool: &ToolSpec) -> Result<(), String>` — dispatches to the
  correct backend, blocks until completion.
- Backends:
  - **Github** — downloads release asset matching `asset_pattern`, SHA-256
    verifies (via `sha2` + `hex`), unpacks tarball / zip / raw binary, two-stage
    atomic rename, creates symlink in `bin_dir`, writes `.rev` sidecar.
  - **Cargo** — shells out to `cargo install`.
  - **Npm** — shells out to `npm install -g`.
  - **Pip** — shells out to `pip install`.
  - **GoInstall** — shells out to `go install`.
  - **Script** — returns `UnsupportedMethod` (pending vetting policy).
- `safe_join(base: &Path, untrusted: &str) -> Result<PathBuf, String>` —
  path-traversal guard; rejects `..` components.

#### `job`

- `InstallPool` — 2-thread worker pool for concurrent installations.
- `InstallHandle` — future-like handle returned by `InstallPool::submit`.
- Per-key deduplication via
  `Mutex<HashMap<String, Vec<Sender<InstallStatus>>>>`; concurrent requests for
  the same tool key share one install run.

#### `sync` feature (maintainer-only)

- `sync-anvil` binary (`src/bin/sync_anvil.rs`) — scrapes
  `mason-org/mason-registry` release manifests, filters supported backends, and
  rewrites `anvil.toml`. Invoked as:

  ```bash
  cargo run --features sync --bin sync-anvil -- --pin <tag>
  ```

  Gated behind the `sync` Cargo feature; not compiled for downstream consumers.

[0.2.4]: https://github.com/kryptic-sh/hjkl-anvil/releases/tag/v0.2.4
[0.2.3]: https://github.com/kryptic-sh/hjkl-anvil/releases/tag/v0.2.3
[0.2.2]: https://github.com/kryptic-sh/hjkl-anvil/releases/tag/v0.2.2
[0.2.1]: https://github.com/kryptic-sh/hjkl-anvil/releases/tag/v0.2.1
[0.2.0]: https://github.com/kryptic-sh/hjkl-anvil/releases/tag/v0.2.0
[0.1.1]: https://github.com/kryptic-sh/hjkl-anvil/releases/tag/v0.1.1
[0.1.0]: https://github.com/kryptic-sh/hjkl-anvil/releases/tag/v0.1.0
