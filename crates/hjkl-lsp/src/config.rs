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
    /// Server-specific `initializationOptions` sent in the `initialize`
    /// request. When unset, a sensible per-server default is applied (e.g.
    /// rust-analyzer is configured to run `cargo clippy` on save). Set this in
    /// config to override the default.
    #[serde(default)]
    pub initialization_options: Option<serde_json::Value>,
}

impl ServerConfig {
    /// Validate the config before it is used to spawn a process.
    ///
    /// # Security
    ///
    /// `command` is **trusted user configuration** accessed from the user's
    /// own config file. Absolute paths to language servers are legitimate;
    /// bare names (e.g. `"rust-analyzer"`) rely on `$PATH` and are also
    /// intentional — they match every editor's LSP config convention.
    ///
    /// If **project-local config** (`.hjkl.toml`, `.editorconfig`, etc.) is
    /// ever supported, this `validate` method MUST become the enforcement
    /// point for untrusted-command restrictions. At minimum, that means:
    ///
    /// * Reject the command when the config source is not the user's global
    ///   config (add a `trusted: bool` parameter or source-tracking).
    /// * Reject relative paths (prevent `./malicious-binary` from a cloned
    ///   repo).  Today relative paths are allowed because trusted user config
    ///   is the only source; that MUST change when project-local config lands.
    /// * Consider an allowlist of known LSP binary names, or at minimum
    ///   reject commands containing path separators (`/`, `\\`) from
    ///   project-local sources.
    ///
    /// For now, as a defense-in-depth measure even for user config, reject
    /// commands with `..` path components — a typo like `"../bin/foo"` has
    /// no legitimate use for a language server and is likely a path traversal.
    pub fn validate(&self, language_id: &str) -> Result<(), String> {
        if self.command.trim().is_empty() {
            return Err(format!(
                "lsp: server for {language_id:?} has an empty `command`"
            ));
        }
        // Defense-in-depth: reject `..` path traversal in the command even
        // for trusted user config — no legitimate LSP binary lives in a
        // parent directory relative to cwd.
        if self.command.contains("..") {
            return Err(format!(
                "lsp: server command for {language_id:?} contains path traversal (`..`)"
            ));
        }
        Ok(())
    }
}
