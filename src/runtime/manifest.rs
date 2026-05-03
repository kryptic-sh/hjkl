//! `bonsai.toml` schema + parser.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Deserialize;

/// One `[language.<name>]` entry in `bonsai.toml`.
///
/// Field set mirrors the union of helix's `languages.toml` and
/// nvim-treesitter's `parsers.lua`. Fields hold facts (URLs, revs, file paths)
/// — not implementation, not copyrightable.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LangSpec {
    /// Upstream git repository for the grammar source.
    pub git_url: String,
    /// Pinned revision — exact commit SHA so rebuilds are reproducible.
    pub git_rev: String,
    /// Optional subdirectory inside the repo. Some monorepos host multiple
    /// grammars (e.g. `tree-sitter-typescript/{tsx,typescript}`).
    #[serde(default)]
    pub subpath: Option<String>,
    /// File extensions (no leading dot) that map to this grammar.
    pub extensions: Vec<String>,
    /// C source files (relative to grammar root, including `subpath`) that
    /// must be compiled by the runtime loader.
    pub c_files: Vec<String>,
    /// Directory (relative to grammar root) holding `.scm` query files.
    pub query_dir: String,
    /// Provenance tag — `"helix"`, `"nvim-treesitter"`, or
    /// `"helix+nvim-treesitter"`. Informational; not used by the loader.
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ManifestRaw {
    language: BTreeMap<String, LangSpec>,
}

/// Parsed `bonsai.toml`. Languages stored in a `BTreeMap` so iteration is
/// alphabetical by name — the registry relies on that ordering for
/// deterministic first-match-wins extension resolution.
#[derive(Debug, Clone)]
pub struct Manifest {
    languages: BTreeMap<String, LangSpec>,
}

impl Manifest {
    /// Parse a TOML manifest string.
    pub fn from_toml_str(s: &str) -> Result<Self> {
        let raw: ManifestRaw = toml::from_str(s).context("parse bonsai.toml")?;
        Ok(Self {
            languages: raw.language,
        })
    }

    /// Iterator over `(name, spec)` pairs in alphabetical name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &LangSpec)> {
        self.languages.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Direct lookup by canonical language name.
    pub fn get(&self, name: &str) -> Option<&LangSpec> {
        self.languages.get(name)
    }

    /// Number of languages in the manifest.
    pub fn len(&self) -> usize {
        self.languages.len()
    }

    /// True if no languages are present.
    pub fn is_empty(&self) -> bool {
        self.languages.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        [language.rust]
        git_url = "https://example/rust"
        git_rev = "deadbeef"
        extensions = ["rs"]
        c_files = ["src/parser.c"]
        query_dir = "queries"

        [language.typescript]
        git_url = "https://example/ts"
        git_rev = "cafef00d"
        subpath = "typescript"
        extensions = ["ts"]
        c_files = ["src/parser.c", "src/scanner.c"]
        query_dir = "queries"
        source = "helix+nvim-treesitter"
    "#;

    #[test]
    fn parses_two_entries() {
        let m = Manifest::from_toml_str(SAMPLE).unwrap();
        assert_eq!(m.len(), 2);
        let rust = m.get("rust").unwrap();
        assert_eq!(rust.extensions, vec!["rs"]);
        assert_eq!(rust.subpath, None);
        let ts = m.get("typescript").unwrap();
        assert_eq!(ts.subpath.as_deref(), Some("typescript"));
        assert_eq!(ts.source.as_deref(), Some("helix+nvim-treesitter"));
    }

    #[test]
    fn iter_is_alphabetical() {
        let m = Manifest::from_toml_str(SAMPLE).unwrap();
        let names: Vec<_> = m.iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["rust", "typescript"]);
    }

    #[test]
    fn embedded_manifest_parses() {
        let s = include_str!("../../bonsai.toml");
        let m = Manifest::from_toml_str(s).expect("embedded bonsai.toml must parse");
        assert!(m.len() > 100, "embedded manifest looks empty: {}", m.len());
        assert!(m.get("rust").is_some());
        assert!(m.get("python").is_some());
    }
}
