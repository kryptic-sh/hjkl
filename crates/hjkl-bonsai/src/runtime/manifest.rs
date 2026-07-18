//! `bonsai.toml` schema + parser.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Which curated query source repo supplies highlights for this language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuerySource {
    Helix,
    NvimTreesitter,
}

impl QuerySource {
    /// Sub-path prefix inside the source repo that holds `<lang>/highlights.scm`.
    pub fn query_prefix(self) -> &'static str {
        match self {
            QuerySource::Helix => "runtime/queries",
            QuerySource::NvimTreesitter => "queries",
        }
    }
}

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
    /// Which curated query source repo supplies `highlights.scm`.
    pub query_source: QuerySource,
    /// Override the per-source default `<lang>` subdirectory (rare).
    #[serde(default)]
    pub query_subdir: Option<String>,
    /// Provenance tag — `"helix"`, `"nvim-treesitter"`, or
    /// `"helix+nvim-treesitter"`. Informational; not used by the loader.
    #[serde(default)]
    pub source: Option<String>,
}

/// Top-level `[meta]` block — pinned revisions for the two query-source repos.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ManifestMeta {
    pub helix_repo: String,
    pub helix_rev: String,
    pub nvim_treesitter_repo: String,
    pub nvim_treesitter_rev: String,
}

#[derive(Debug, Deserialize)]
struct ManifestRaw {
    meta: ManifestMeta,
    language: BTreeMap<String, LangSpec>,
}

/// Parsed `bonsai.toml`. Languages stored in a `BTreeMap` so iteration is
/// alphabetical by name — the registry relies on that ordering for
/// deterministic first-match-wins extension resolution.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub meta: ManifestMeta,
    languages: BTreeMap<String, LangSpec>,
}

impl Manifest {
    /// Parse a TOML manifest string.
    pub fn from_toml_str(s: &str) -> Result<Self> {
        let raw: ManifestRaw = toml::from_str(s).context("parse bonsai.toml")?;
        for (name, spec) in &raw.language {
            validate_relative_path(name, "subpath", spec.subpath.as_deref())?;
            validate_relative_path(name, "query_subdir", spec.query_subdir.as_deref())?;
            for c_file in &spec.c_files {
                validate_relative_path(name, "c_files", Some(c_file))?;
            }
        }
        Ok(Self {
            meta: raw.meta,
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

fn validate_relative_path(language: &str, field: &str, value: Option<&str>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.is_empty()
        || Path::new(value).components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        anyhow::bail!("unsafe {field} path {value:?} in language {language:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        [meta]
        helix_repo = "https://github.com/helix-editor/helix"
        helix_rev = "aaaa0000bbbb1111cccc2222dddd3333eeee4444"
        nvim_treesitter_repo = "https://github.com/nvim-treesitter/nvim-treesitter"
        nvim_treesitter_rev = "ffff5555aaaa0000bbbb1111cccc2222dddd3333"

        [language.rust]
        git_url = "https://example/rust"
        git_rev = "deadbeef"
        extensions = ["rs"]
        c_files = ["src/parser.c"]
        query_source = "helix"

        [language.typescript]
        git_url = "https://example/ts"
        git_rev = "cafef00d"
        subpath = "typescript"
        extensions = ["ts"]
        c_files = ["src/parser.c", "src/scanner.c"]
        query_source = "nvim_treesitter"
        source = "helix+nvim-treesitter"
    "#;

    #[test]
    fn parses_two_entries() {
        let m = Manifest::from_toml_str(SAMPLE).unwrap();
        assert_eq!(m.len(), 2);
        let rust = m.get("rust").unwrap();
        assert_eq!(rust.extensions, vec!["rs"]);
        assert_eq!(rust.subpath, None);
        assert_eq!(rust.query_source, QuerySource::Helix);
        let ts = m.get("typescript").unwrap();
        assert_eq!(ts.subpath.as_deref(), Some("typescript"));
        assert_eq!(ts.query_source, QuerySource::NvimTreesitter);
        assert_eq!(ts.source.as_deref(), Some("helix+nvim-treesitter"));
    }

    #[test]
    fn parses_meta_block() {
        let m = Manifest::from_toml_str(SAMPLE).unwrap();
        assert_eq!(m.meta.helix_repo, "https://github.com/helix-editor/helix");
        assert_eq!(m.meta.helix_rev, "aaaa0000bbbb1111cccc2222dddd3333eeee4444");
    }

    #[test]
    fn iter_is_alphabetical() {
        let m = Manifest::from_toml_str(SAMPLE).unwrap();
        let names: Vec<_> = m.iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["rust", "typescript"]);
    }

    #[test]
    fn rejects_unsafe_path_fields() {
        for (field, value) in [
            ("subpath", "../escape"),
            ("c_files", "/etc/passwd"),
            ("query_subdir", "../queries"),
        ] {
            let manifest = SAMPLE.replace(
                "query_source = \"helix\"",
                &format!("query_source = \"helix\"\n{field} = \"{value}\""),
            );
            assert!(
                Manifest::from_toml_str(&manifest).is_err(),
                "{field} accepted {value}"
            );
        }
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
