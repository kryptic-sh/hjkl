//! Mason-style LSP and developer-tool installer.
//!
//! `hjkl-anvil` is the tool-installation sibling of `hjkl-bonsai`. Where
//! bonsai manages tree-sitter grammars, anvil manages language servers,
//! formatters, linters, and debug adapters — using the same XDG path
//! conventions and compile-time embedded catalog pattern.
//!
//! # Features
//!
//! ## `sync` (dev-only)
//!
//! Enables the `sync-anvil` xtask binary that regenerates `anvil.toml` from
//! the upstream `mason-org/mason-registry` JSON artifact. Not needed by
//! downstream consumers — it pulls in `reqwest`, `zip`, and `serde_json`.
//!
//! Maintainers run:
//! ```text
//! cargo run -p hjkl-anvil --features sync --bin sync-anvil -- --pin <tag>
//! ```
//!
//! # Quick start
//!
//! ```rust
//! use hjkl_anvil::{Registry, ToolCategory};
//!
//! let registry = Registry::embedded().expect("embedded catalog must load");
//! println!("catalog has {} tools", registry.len());
//!
//! if let Some(spec) = registry.get("rust-analyzer") {
//!     println!("rust-analyzer v{}", spec.version);
//! }
//!
//! for name in registry.by_category(ToolCategory::Lsp) {
//!     println!("  lsp: {name}");
//! }
//! ```

pub mod installer;
pub mod job;
pub mod manifest;
pub mod registry;
pub mod store;

pub use installer::{Install, InstallError, InstallStatus, install_blocking};
pub use job::{InstallHandle, InstallPool};
pub use manifest::{
    CargoMethod, GithubMethod, GoMethod, InstallMethod, Manifest, ManifestError, ManifestMeta,
    NpmMethod, PipMethod, ScriptMethod, ToolCategory, ToolSpec,
};
pub use registry::{Registry, RegistryError};
pub use store::{RevSidecar, StoreError};
