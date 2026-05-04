//! Path → grammar resolution.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use super::manifest::{LangSpec, Manifest};

/// Resolves a path or language name to a [`LangSpec`].
///
/// Extension lookups are first-match-wins by alphabetical language name. This
/// is how the C/C++ overlap on `.c`/`.h` resolves: `c` < `cpp`, so a bare
/// header file gets the C grammar by default. The editor layer is responsible
/// for honoring user overrides (modeline, `:set ft=`, project config).
#[derive(Debug, Clone)]
pub struct GrammarRegistry {
    manifest: Manifest,
    /// Lower-cased extension → canonical language name. Built once at
    /// construction; alphabetical iteration order of the manifest gives
    /// deterministic precedence.
    by_ext: HashMap<String, String>,
}

impl GrammarRegistry {
    /// Build a registry from an in-memory manifest.
    pub fn new(manifest: Manifest) -> Self {
        let mut by_ext: HashMap<String, String> = HashMap::new();
        for (name, spec) in manifest.iter() {
            for ext in &spec.extensions {
                let key = ext.to_ascii_lowercase();
                by_ext.entry(key).or_insert_with(|| name.to_string());
            }
        }
        Self { manifest, by_ext }
    }

    /// Build the default registry from the embedded `bonsai.toml`.
    pub fn embedded() -> Result<Self> {
        let s = include_str!("../../bonsai.toml");
        Ok(Self::new(Manifest::from_toml_str(s)?))
    }

    /// Direct lookup by canonical language name.
    pub fn by_name(&self, name: &str) -> Option<&LangSpec> {
        self.manifest.get(name)
    }

    /// Resolve a path to its default grammar by extension. Returns `None` for
    /// extensionless paths or unknown extensions.
    pub fn detect_for_path(&self, path: &Path) -> Option<&LangSpec> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        let name = self.by_ext.get(&ext)?;
        self.manifest.get(name)
    }

    /// Resolve a path to the canonical language name (without returning the
    /// full spec). Useful for callers that just want the lookup key.
    pub fn name_for_path(&self, path: &Path) -> Option<&str> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        self.by_ext.get(&ext).map(|s| s.as_str())
    }

    /// Underlying manifest reference, for callers that need to iterate.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Manifest meta (pinned query-source revisions).
    pub fn meta(&self) -> &super::manifest::ManifestMeta {
        &self.manifest.meta
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn embedded() -> GrammarRegistry {
        GrammarRegistry::embedded().expect("embedded manifest must build")
    }

    #[test]
    fn rust_path_resolves() {
        let r = embedded();
        let spec = r.detect_for_path(&PathBuf::from("src/main.rs")).unwrap();
        assert!(spec.git_url.contains("rust"));
    }

    #[test]
    fn python_path_resolves() {
        let r = embedded();
        assert_eq!(
            r.name_for_path(&PathBuf::from("foo/bar.py")),
            Some("python")
        );
    }

    #[test]
    fn c_extension_picks_c_over_cpp() {
        // Both grammars claim `.c` (cpp via its uppercase `C` variant after
        // lower-casing). Alphabetical first-match must give us C.
        let r = embedded();
        assert_eq!(r.name_for_path(&PathBuf::from("foo.c")), Some("c"));
    }

    #[test]
    fn cpp_specific_extensions_still_route_to_cpp() {
        let r = embedded();
        // `.cpp` is unambiguously C++.
        assert_eq!(r.name_for_path(&PathBuf::from("foo.cpp")), Some("cpp"));
        // The C grammar in the manifest doesn't claim `.h` — only cpp does
        // (via the lowercased `H`). Distinguishing C vs C++ headers is the
        // editor layer's job (modeline / project config).
        assert_eq!(r.name_for_path(&PathBuf::from("foo.h")), Some("cpp"));
    }

    #[test]
    fn case_insensitive_extension() {
        let r = embedded();
        assert_eq!(
            r.name_for_path(&PathBuf::from("README.MD")),
            Some("markdown")
        );
    }

    #[test]
    fn unknown_extension_returns_none() {
        let r = embedded();
        assert!(r.detect_for_path(&PathBuf::from("foo.zzznope")).is_none());
    }

    #[test]
    fn extensionless_returns_none() {
        let r = embedded();
        assert!(r.detect_for_path(&PathBuf::from("Makefile")).is_none());
    }

    #[test]
    fn by_name_direct_lookup() {
        let r = embedded();
        assert!(r.by_name("rust").is_some());
        assert!(r.by_name("definitely-not-a-language").is_none());
    }

    #[test]
    fn handcrafted_alphabetical_precedence() {
        // Two grammars claiming the same extension; alphabetically first wins.
        let toml = r#"
            [meta]
            helix_repo = "https://github.com/helix-editor/helix"
            helix_rev = "aaaa0000bbbb1111cccc2222dddd3333eeee4444"
            nvim_treesitter_repo = "https://github.com/nvim-treesitter/nvim-treesitter"
            nvim_treesitter_rev = "ffff5555aaaa0000bbbb1111cccc2222dddd3333"

            [language.aaa]
            git_url = "https://example/aaa"
            git_rev = "1"
            extensions = ["x"]
            c_files = ["src/parser.c"]
            query_source = "helix"

            [language.bbb]
            git_url = "https://example/bbb"
            git_rev = "2"
            extensions = ["x"]
            c_files = ["src/parser.c"]
            query_source = "helix"
        "#;
        let r = GrammarRegistry::new(Manifest::from_toml_str(toml).unwrap());
        assert_eq!(r.name_for_path(&PathBuf::from("foo.x")), Some("aaa"));
    }
}
