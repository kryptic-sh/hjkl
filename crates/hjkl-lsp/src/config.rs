//! LSP configuration types.

use std::collections::HashMap;

/// Top-level LSP configuration. Defaults to disabled with no servers.
#[derive(Clone, Debug, serde::Deserialize, Default)]
pub struct LspConfig {
    /// Whether the LSP subsystem is active. Defaults to `false` (opt-in).
    #[serde(default)]
    pub enabled: bool,
    /// Map of language id (e.g. `"rust"`) to server configuration.
    #[serde(default)]
    pub servers: HashMap<String, ServerConfig>,
}

/// Configuration for a single language server.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct ServerConfig {
    /// Executable name or path (e.g. `"rust-analyzer"`).
    pub command: String,
    /// Extra arguments passed to the server.
    #[serde(default)]
    pub args: Vec<String>,
    /// Files/dirs whose presence marks the workspace root.
    #[serde(default)]
    pub root_markers: Vec<String>,
    /// Shut down a server that has had no attached buffers for this many
    /// seconds. `0` means never (Phase 1 keeps servers alive anyway).
    #[serde(default)]
    pub shutdown_idle_after_secs: u64,
}
