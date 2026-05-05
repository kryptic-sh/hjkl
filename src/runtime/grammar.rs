//! `dlopen`-loaded grammar bundle: parser library + queries.
//!
//! Combines a [`tree_sitter::Language`] (resolved from a runtime-loaded shared
//! library) with the `.scm` query strings the highlighter needs. Field order
//! matters: `tree_sitter::Language` references data inside `_lib`, so `_lib`
//! must outlive it. Rust drops fields top-down, so `_lib` stays last.

use anyhow::{Context, Result};
use libloading::Library;
use tree_sitter::Language;
use tree_sitter_language::LanguageFn;

use super::loader::GrammarLoader;
use super::manifest::{LangSpec, ManifestMeta};

/// A loaded tree-sitter grammar — parser shared library + highlights query.
pub struct Grammar {
    name: String,
    language: Language,
    highlights_scm: String,
    /// Language-injection query sourced from `<name>.injections.scm` next to
    /// `<name>.scm`. `None` when the grammar does not ship one (normal — most
    /// grammars don't define injections).
    injections_scm: Option<String>,
    /// Kept alive so `language`'s underlying pointer stays valid. Must be
    /// the LAST field so its `Drop` runs after `language`'s.
    _lib: Library,
}

impl Grammar {
    /// Canonical language name (matches the manifest key).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Tree-sitter language handle ready to install on a `Parser`.
    pub fn language(&self) -> &Language {
        &self.language
    }

    /// `highlights.scm` source.
    pub fn highlights_scm(&self) -> &str {
        &self.highlights_scm
    }

    /// `injections.scm` source, if this grammar ships one. Grammars that do
    /// not define language injections return `None`.
    pub fn injections_scm(&self) -> Option<&str> {
        self.injections_scm.as_deref()
    }

    /// Load a grammar by name. The [`GrammarLoader`] handles parser
    /// resolution (system → user → on-demand clone+compile+install).
    /// The highlights query is read from `<so_parent>/<name>.scm`.
    /// The injections query is read from `<so_parent>/<name>.injections.scm`
    /// when present (absent = no injections, not an error).
    pub fn load(
        name: &str,
        spec: &LangSpec,
        loader: &GrammarLoader,
        meta: &ManifestMeta,
    ) -> Result<Self> {
        let so = loader
            .load(name, spec, meta)
            .with_context(|| format!("resolve grammar {name}"))?;
        let lib =
            unsafe { Library::new(&so) }.with_context(|| format!("dlopen {}", so.display()))?;

        let symbol = format!("tree_sitter_{}", symbol_name(name));
        // Safety: tree-sitter grammars expose `tree_sitter_<lang>()` as
        // `unsafe extern "C" fn() -> *const TSLanguage`. We just resolve the
        // symbol; LanguageFn::from_raw documents the call-site invariants.
        let raw: unsafe extern "C" fn() -> *const () = unsafe {
            *lib.get(symbol.as_bytes())
                .with_context(|| format!("missing symbol `{symbol}` in {}", so.display()))?
        };
        let lang_fn = unsafe { LanguageFn::from_raw(raw) };
        let language = Language::from(lang_fn);

        let parent = so
            .parent()
            .with_context(|| format!("grammar {} has no parent dir", so.display()))?;
        let highlights_path = parent.join(format!("{name}.scm"));
        let highlights_scm = std::fs::read_to_string(&highlights_path).with_context(|| {
            format!(
                "read highlights query for {name} at {}",
                highlights_path.display()
            )
        })?;

        // Injections are optional — NotFound maps to None, other IO errors propagate.
        let injections_path = parent.join(format!("{name}.injections.scm"));
        let injections_scm = match std::fs::read_to_string(&injections_path) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "read injections query for {name} at {}",
                        injections_path.display()
                    )
                });
            }
        };

        Ok(Self {
            name: name.to_string(),
            language,
            highlights_scm,
            injections_scm,
            _lib: lib,
        })
    }

    /// Load a grammar from an already-resolved shared library path.
    ///
    /// This is the fast path used by `complete_load` after the async loader
    /// finishes a clone+compile: by the time this is called the `.so`,
    /// `<name>.scm`, and optional `<name>.injections.scm` are already on disk
    /// next to each other, so we skip the `GrammarLoader` chain entirely and
    /// go straight to `dlopen` + query reads.
    pub fn load_from_path(name: &str, so: &std::path::Path) -> Result<Self> {
        let lib =
            unsafe { Library::new(so) }.with_context(|| format!("dlopen {}", so.display()))?;

        let symbol = format!("tree_sitter_{}", symbol_name(name));
        let raw: unsafe extern "C" fn() -> *const () = unsafe {
            *lib.get(symbol.as_bytes())
                .with_context(|| format!("missing symbol `{symbol}` in {}", so.display()))?
        };
        let lang_fn = unsafe { LanguageFn::from_raw(raw) };
        let language = Language::from(lang_fn);

        let parent = so
            .parent()
            .with_context(|| format!("grammar {} has no parent dir", so.display()))?;
        let highlights_path = parent.join(format!("{name}.scm"));
        let highlights_scm = std::fs::read_to_string(&highlights_path).with_context(|| {
            format!(
                "read highlights query for {name} at {}",
                highlights_path.display()
            )
        })?;

        let injections_path = parent.join(format!("{name}.injections.scm"));
        let injections_scm = match std::fs::read_to_string(&injections_path) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "read injections query for {name} at {}",
                        injections_path.display()
                    )
                });
            }
        };

        Ok(Self {
            name: name.to_string(),
            language,
            highlights_scm,
            injections_scm,
            _lib: lib,
        })
    }

    /// Construct a [`Grammar`] from already-resolved pieces. Useful for
    /// tests or callers that have a custom parser source.
    ///
    /// # Safety
    ///
    /// `language` must be the value returned by `lib`'s
    /// `tree_sitter_<name>()` symbol (or otherwise reference data owned by
    /// `lib`). `lib` must remain alive until this `Grammar` is dropped —
    /// the type enforces that internally, but the *caller* must guarantee
    /// the `language`/`lib` pairing is correct.
    pub unsafe fn from_parts(
        name: impl Into<String>,
        lib: Library,
        language: Language,
        highlights_scm: impl Into<String>,
        injections_scm: Option<impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            language,
            highlights_scm: highlights_scm.into(),
            injections_scm: injections_scm.map(|s| s.into()),
            _lib: lib,
        }
    }
}

/// tree-sitter's C symbols use underscores, never hyphens — hyphens are
/// invalid C identifiers. The convention is `c_sharp` (manifest name)
/// already matches, but defensive normalization handles future entries.
fn symbol_name(name: &str) -> String {
    name.replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_name_normalizes_hyphens() {
        assert_eq!(symbol_name("rust"), "rust");
        assert_eq!(symbol_name("c-sharp"), "c_sharp");
        assert_eq!(symbol_name("html-erb"), "html_erb");
    }

    #[test]
    #[ignore = "network + compiler: clones tree-sitter-c + helix queries, builds, installs, dlopens"]
    fn load_real_grammar_end_to_end() {
        use super::super::compile::GrammarCompiler;
        use super::super::manifest::{ManifestMeta, QuerySource};
        use super::super::source::{QuerySourceCache, SourceCache};

        let tmp = tempfile::tempdir().unwrap();
        let sources = SourceCache::new(tmp.path().join("cache"));
        let query_sources = QuerySourceCache::new(tmp.path().join("qcache"));
        let user_dir = tmp.path().join("user");
        let loader = GrammarLoader::new(
            vec![],
            user_dir,
            sources,
            query_sources,
            GrammarCompiler::new(),
        );

        let meta = ManifestMeta {
            helix_repo: "https://github.com/helix-editor/helix".into(),
            helix_rev: "87d5c05c4432a079d3b7aaa10cda1cfe1803c18c".into(),
            nvim_treesitter_repo: "https://github.com/nvim-treesitter/nvim-treesitter".into(),
            nvim_treesitter_rev: "cf12346a3414fa1b06af75c79faebe7f76df080a".into(),
        };
        let spec = LangSpec {
            git_url: "https://github.com/tree-sitter/tree-sitter-c".into(),
            git_rev: "2a265d69a4caf57108a73ad2ed1e6922dd2f998c".into(),
            subpath: None,
            extensions: vec!["c".into()],
            c_files: vec!["src/parser.c".into()],
            query_source: QuerySource::Helix,
            query_subdir: None,
            source: None,
        };

        let grammar = Grammar::load("c", &spec, &loader, &meta).unwrap();
        assert_eq!(grammar.name(), "c");
        let q = tree_sitter::Query::new(grammar.language(), grammar.highlights_scm());
        assert!(q.is_ok(), "highlights.scm failed to compile: {:?}", q.err());
    }
}
