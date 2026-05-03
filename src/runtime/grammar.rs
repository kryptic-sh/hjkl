//! `dlopen`-loaded grammar bundle: parser library + queries.
//!
//! Combines a [`tree_sitter::Language`] (resolved from a runtime-loaded shared
//! library) with the `.scm` query strings the highlighter needs. Field order
//! matters: `tree_sitter::Language` references data inside `_lib`, so `_lib`
//! must outlive it. Rust drops fields top-down, so `_lib` stays last.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use libloading::Library;
use tree_sitter::Language;
use tree_sitter_language::LanguageFn;

use super::loader::GrammarLoader;
use super::manifest::LangSpec;
use super::source::SourceCache;

/// Standard query files tree-sitter recognizes. Read in this order; missing
/// files are tolerated (return `None`).
pub const HIGHLIGHTS_FILE: &str = "highlights.scm";
pub const LOCALS_FILE: &str = "locals.scm";
pub const INJECTIONS_FILE: &str = "injections.scm";

/// A loaded tree-sitter grammar — parser shared library + query sources.
pub struct Grammar {
    name: String,
    language: Language,
    highlights_scm: String,
    locals_scm: Option<String>,
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

    /// `highlights.scm` source — required.
    pub fn highlights_scm(&self) -> &str {
        &self.highlights_scm
    }

    /// `locals.scm` source if present.
    pub fn locals_scm(&self) -> Option<&str> {
        self.locals_scm.as_deref()
    }

    /// `injections.scm` source if present.
    pub fn injections_scm(&self) -> Option<&str> {
        self.injections_scm.as_deref()
    }

    /// Load a grammar by name, walking the standard chain:
    /// 1. Resolve the parser `.so` via [`GrammarLoader::load`] (system →
    ///    user → cache → compile).
    /// 2. Read the `.scm` query files. Source-cloning happens via the
    ///    [`SourceCache`] reference so the queries can come from the same
    ///    upstream tree the parser was built from.
    pub fn load(
        name: &str,
        spec: &LangSpec,
        loader: &GrammarLoader,
        sources: &SourceCache,
    ) -> Result<Self> {
        let so = loader
            .load(name, spec)
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

        let source_root = sources
            .acquire(name, spec)
            .with_context(|| format!("acquire source for {name} (queries)"))?;
        let query_dir = source_root.join(&spec.query_dir);
        let highlights_scm = read_required_query(&query_dir, HIGHLIGHTS_FILE, name)?;
        let locals_scm = read_optional_query(&query_dir, LOCALS_FILE);
        let injections_scm = read_optional_query(&query_dir, INJECTIONS_FILE);

        Ok(Self {
            name: name.to_string(),
            language,
            highlights_scm,
            locals_scm,
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
        locals_scm: Option<String>,
        injections_scm: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            language,
            highlights_scm: highlights_scm.into(),
            locals_scm,
            injections_scm,
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

fn read_required_query(query_dir: &Path, filename: &str, lang: &str) -> Result<String> {
    let p = query_dir.join(filename);
    std::fs::read_to_string(&p).with_context(|| {
        format!(
            "read required query {filename} for {lang} at {}",
            p.display()
        )
    })
}

fn read_optional_query(query_dir: &Path, filename: &str) -> Option<String> {
    let p: PathBuf = query_dir.join(filename);
    std::fs::read_to_string(&p).ok()
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
    #[ignore = "network + compiler: clones tree-sitter-c, builds, dlopens"]
    fn load_real_grammar_end_to_end() {
        use super::super::compile::GrammarCompiler;

        let tmp = tempfile::tempdir().unwrap();
        let sources = SourceCache::new(tmp.path().join("sources"));
        let compiler = GrammarCompiler::new(tmp.path().join("cache"));
        let loader = GrammarLoader::new(vec![], vec![], sources.clone(), compiler);

        let spec = LangSpec {
            git_url: "https://github.com/tree-sitter/tree-sitter-c".into(),
            git_rev: "2a265d69a4caf57108a73ad2ed1e6922dd2f998c".into(),
            subpath: None,
            extensions: vec!["c".into()],
            c_files: vec!["src/parser.c".into()],
            query_dir: "queries".into(),
            source: None,
        };

        let grammar = Grammar::load("c", &spec, &loader, &sources).unwrap();
        assert_eq!(grammar.name(), "c");
        // Sanity: the language resolved enough to compile a trivial query.
        let q = tree_sitter::Query::new(grammar.language(), grammar.highlights_scm());
        assert!(q.is_ok(), "highlights.scm failed to compile: {:?}", q.err());
    }
}
