//! Path → grammar resolution.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use super::manifest::{LangSpec, Manifest};

/// Resolves a path or language name to a [`LangSpec`].
///
/// Extension lookups are first-match-wins by alphabetical language name only
/// for identical, exact-case extension keys. This is how duplicate `.c` keys
/// resolve: `c` < `cpp`, so a bare C source file gets the C grammar by default.
/// The editor layer is responsible for honoring user overrides (modeline,
/// `:set ft=`, project config).
#[derive(Debug, Clone)]
pub struct GrammarRegistry {
    manifest: Manifest,
    /// Exact-case extension → canonical language name. Built once at
    /// construction; alphabetical manifest iteration gives deterministic
    /// precedence for duplicate keys.
    by_ext: HashMap<String, String>,
}

impl GrammarRegistry {
    /// Build a registry from an in-memory manifest.
    pub fn new(manifest: Manifest) -> Self {
        let mut by_ext: HashMap<String, String> = HashMap::new();
        for (name, spec) in manifest.iter() {
            for ext in &spec.extensions {
                by_ext
                    .entry(ext.clone())
                    .or_insert_with(|| name.to_string());
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
        let ext = path.extension()?.to_str()?;
        let name = self.by_ext.get(ext)?;
        self.manifest.get(name)
    }

    /// Resolve a path to the canonical language name (without returning the
    /// full spec). Useful for callers that just want the lookup key.
    pub fn name_for_path(&self, path: &Path) -> Option<&str> {
        let ext = path.extension()?.to_str()?;
        self.name_for_ext(ext)
    }

    /// Resolve a bare, exact-case file extension (no leading dot) to the
    /// canonical language name. Same lookup [`Self::name_for_path`] performs,
    /// for callers that already carry an extension string and have no path.
    /// Undeclared case variants return `None`.
    pub fn name_for_ext(&self, ext: &str) -> Option<&str> {
        self.by_ext.get(ext).map(|s| s.as_str())
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
    fn c_and_cpp_extensions_preserve_case() {
        let r = embedded();
        assert_eq!(r.name_for_path(&PathBuf::from("foo.c")), Some("c"));
        assert_eq!(r.name_for_path(&PathBuf::from("foo.C")), Some("cpp"));
        assert_eq!(r.name_for_ext("c"), Some("c"));
        assert_eq!(r.name_for_ext("C"), Some("cpp"));
    }

    #[test]
    fn undeclared_uppercase_extensions_return_none() {
        let r = embedded();
        assert_eq!(r.name_for_path(&PathBuf::from("README.MD")), None);
        assert_eq!(r.name_for_path(&PathBuf::from("plugin.ZSH")), None);
        assert_eq!(r.name_for_ext("MD"), None);
        assert_eq!(r.name_for_ext("ZSH"), None);
    }

    #[test]
    fn cpp_specific_extensions_still_route_to_cpp() {
        let r = embedded();
        // `.cpp` is unambiguously C++.
        assert_eq!(r.name_for_path(&PathBuf::from("foo.cpp")), Some("cpp"));
        // The C grammar does not claim `.h` or `.H`; both exact-case extension
        // keys belong to cpp. Distinguishing C vs C++ headers is the editor
        // layer's job (modeline / project config).
        assert_eq!(r.name_for_path(&PathBuf::from("foo.h")), Some("cpp"));
        assert_eq!(r.name_for_path(&PathBuf::from("foo.H")), Some("cpp"));
    }

    #[test]
    fn declared_lowercase_extension_resolves() {
        let r = embedded();
        assert_eq!(
            r.name_for_path(&PathBuf::from("README.md")),
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
    fn handcrafted_exact_case_precedence() {
        // Identical extension keys use alphabetical first-match precedence;
        // differently cased keys remain distinct.
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
            extensions = ["x", "X"]
            c_files = ["src/parser.c"]
            query_source = "helix"
        "#;
        let r = GrammarRegistry::new(Manifest::from_toml_str(toml).unwrap());
        assert_eq!(r.name_for_path(&PathBuf::from("foo.x")), Some("aaa"));
        assert_eq!(r.name_for_path(&PathBuf::from("foo.X")), Some("bbb"));
    }
}
