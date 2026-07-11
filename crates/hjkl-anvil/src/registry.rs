//! In-memory registry built from a parsed [`Manifest`].

use thiserror::Error;

use crate::manifest::{Manifest, ManifestError, ToolCategory, ToolSpec};

// ── Error ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
}

// ── Registry ─────────────────────────────────────────────────────────────────

/// In-memory registry built from a parsed [`Manifest`].
///
/// Tool names are stored in their original `BTreeMap` order (alphabetical),
/// so all iteration methods return deterministic alphabetical results.
pub struct Registry {
    manifest: Manifest,
}

impl Registry {
    /// Build from an already-parsed manifest.
    pub fn new(manifest: Manifest) -> Self {
        Self { manifest }
    }

    /// Build from the bundled in-tree `anvil.toml` (compile-time embed).
    ///
    /// The catalog is baked into the binary via `include_str!` so no
    /// file-system access is needed at runtime — same pattern used by
    /// `hjkl-bonsai`'s `GrammarRegistry::embedded()`.
    ///
    /// The parsed manifest is run through [`Manifest::validate`] before use so
    /// no tool with a malformed name, repo, URL, or checksum can reach the
    /// installer. (See also [`Self::from_manifest`] for the file-load path.)
    pub fn embedded() -> Result<Self, RegistryError> {
        let s = include_str!("../anvil.toml");
        let manifest = crate::manifest::parse_str(s)?;
        Self::from_manifest(manifest)
    }

    /// Build from an already-parsed manifest, running [`Manifest::validate`]
    /// first. Prefer this over [`Self::new`] for any manifest that did not
    /// originate from a trusted, already-validated source: it rejects entries
    /// whose name, github repo, script URL, asset pattern, or checksum is
    /// malformed before they can reach the installer.
    pub fn from_manifest(manifest: Manifest) -> Result<Self, RegistryError> {
        manifest.validate()?;
        Ok(Self::new(manifest))
    }

    /// All tool names in deterministic alphabetical order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.manifest.tool.keys().map(|s| s.as_str())
    }

    /// Look up a tool by name. Returns `None` for unknown names.
    pub fn get(&self, name: &str) -> Option<&ToolSpec> {
        self.manifest.tool.get(name)
    }

    /// All tool names in the given category, in alphabetical order.
    pub fn by_category(&self, c: ToolCategory) -> Vec<&str> {
        self.manifest
            .tool
            .iter()
            .filter(|(_, spec)| spec.category == c)
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Number of tools in the registry.
    pub fn len(&self) -> usize {
        self.manifest.tool.len()
    }

    /// True if the registry contains no tools.
    pub fn is_empty(&self) -> bool {
        self.manifest.tool.is_empty()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn embedded() -> Registry {
        Registry::embedded().expect("embedded anvil.toml must build a registry")
    }

    #[test]
    fn embedded_succeeds() {
        let r = embedded();
        assert!(!r.is_empty());
    }

    #[test]
    fn names_alphabetical() {
        let r = embedded();
        let names: Vec<_> = r.names().collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "names() must be alphabetical");
    }

    #[test]
    fn get_known_tool() {
        let r = embedded();
        assert!(r.get("rust-analyzer").is_some());
    }

    #[test]
    fn get_unknown_tool_is_none() {
        let r = embedded();
        assert!(r.get("definitely-not-a-tool").is_none());
    }

    #[test]
    fn by_category_lsp_contains_expected() {
        let r = embedded();
        let lsp_tools = r.by_category(ToolCategory::Lsp);
        // The embedded catalog has: rust-analyzer, gopls, lua-language-server,
        // pyright, taplo — all LSP.
        assert!(lsp_tools.contains(&"rust-analyzer"), "{lsp_tools:?}");
        assert!(lsp_tools.contains(&"gopls"), "{lsp_tools:?}");
        assert!(lsp_tools.contains(&"lua-language-server"), "{lsp_tools:?}");
        assert!(lsp_tools.contains(&"pyright"), "{lsp_tools:?}");
        assert!(lsp_tools.contains(&"taplo"), "{lsp_tools:?}");
    }

    #[test]
    fn by_category_formatter_contains_shfmt() {
        let r = embedded();
        let fmt_tools = r.by_category(ToolCategory::Formatter);
        assert!(fmt_tools.contains(&"shfmt"), "{fmt_tools:?}");
    }

    #[test]
    fn by_category_alphabetical() {
        let r = embedded();
        let lsp_tools = r.by_category(ToolCategory::Lsp);
        let mut sorted = lsp_tools.clone();
        sorted.sort_unstable();
        assert_eq!(lsp_tools, sorted, "by_category() must be alphabetical");
    }

    #[test]
    fn len_matches_tool_count() {
        let r = embedded();
        assert_eq!(r.len(), 6);
    }

    #[test]
    fn embedded_is_validated() {
        // `embedded()` must run the manifest through validate() — the shipped
        // catalog passes, so this simply confirms the path is wired.
        assert!(Registry::embedded().is_ok());
    }

    #[test]
    fn from_manifest_rejects_invalid_entry() {
        // A github entry with a malformed repo must be rejected at
        // construction, not silently accepted.
        let mut manifest = crate::manifest::parse_str("[meta]\nschema_version = 1\n").unwrap();
        let mut sha256 = std::collections::BTreeMap::new();
        sha256.insert("x86_64-unknown-linux-gnu".to_string(), String::new());
        manifest.tool.insert(
            "bad".to_string(),
            crate::manifest::ToolSpec {
                category: ToolCategory::Lsp,
                description: "d".to_string(),
                version: "1.0".to_string(),
                bin: "bad".to_string(),
                method: crate::manifest::InstallMethod::Github(crate::manifest::GithubMethod {
                    repo: "no-slash-here".to_string(),
                    asset_pattern: "bad-{triple}.gz".to_string(),
                    sha256,
                }),
            },
        );
        assert!(
            Registry::from_manifest(manifest).is_err(),
            "from_manifest must reject an invalid repo"
        );
    }

    #[test]
    fn registry_from_custom_manifest() {
        let toml = r#"
            [meta]
            schema_version = 1

            [tool.my-lsp]
            category = "lsp"
            description = "My custom LSP"
            version = "1.0.0"
            bin = "my-lsp"
            method = "cargo"
            crate_name = "my-lsp"
        "#;
        let manifest = crate::manifest::parse_str(toml).unwrap();
        let r = Registry::new(manifest);
        assert_eq!(r.len(), 1);
        assert!(r.get("my-lsp").is_some());
        assert!(r.get("other").is_none());
    }
}
