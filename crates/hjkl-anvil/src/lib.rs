//! Mason-style LSP and developer-tool installer.
//!
//! `hjkl-anvil` is the tool-installation sibling of `hjkl-bonsai`. Where
//! bonsai manages tree-sitter grammars, anvil manages language servers,
//! formatters, linters, and debug adapters — using the same XDG path
//! conventions and compile-time embedded catalog pattern.
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

pub mod manifest;
pub mod registry;
pub mod store;

pub use manifest::{
    CargoMethod, GithubMethod, GoMethod, InstallMethod, Manifest, ManifestError, ManifestMeta,
    NpmMethod, PipMethod, ScriptMethod, ToolCategory, ToolSpec,
};
pub use registry::{Registry, RegistryError};
pub use store::{RevSidecar, StoreError};
