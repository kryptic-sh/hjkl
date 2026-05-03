//! Language directory — facade over hjkl-bonsai's runtime grammar API.
//!
//! Wraps a [`GrammarRegistry`] (manifest lookup), [`GrammarLoader`] (system →
//! user → cache → compile chain), and [`SourceCache`] (clone-on-demand for
//! `.scm` queries) behind a single struct that resolves a `Path` or language
//! name to a cached `Arc<Grammar>`.
//!
//! First use of a previously-unseen language synchronously triggers the
//! loader chain (worst case: clone the upstream repo + cc compile). Distro
//! packages that pre-populate `/usr/share/hjkl/runtime/grammars/` skip the
//! compile step. Once a `Grammar` is cached, subsequent lookups are a cheap
//! `HashMap::get` + `Arc::clone`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use hjkl_bonsai::runtime::{Grammar, GrammarLoader, GrammarRegistry, SourceCache};

/// Shared language resolver. `Arc<LanguageDirectory>` is the right thing to
/// pass around so the in-memory `Grammar` cache is shared across the
/// `SyntaxLayer` and every picker source.
pub struct LanguageDirectory {
    registry: GrammarRegistry,
    loader: GrammarLoader,
    sources: SourceCache,
    cache: Mutex<HashMap<String, Arc<Grammar>>>,
}

impl LanguageDirectory {
    /// Build a new directory rooted at the user's standard XDG dirs. Fails
    /// only if the embedded `bonsai.toml` doesn't parse or `$HOME` is unset.
    pub fn new() -> Result<Self> {
        Ok(Self {
            registry: GrammarRegistry::embedded()?,
            loader: GrammarLoader::user_default()?,
            sources: SourceCache::user_default()?,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// Resolve a language name (e.g. `"rust"`, `"python"`) to a loaded
    /// grammar. May block on first use to clone + compile.
    pub fn by_name(&self, name: &str) -> Option<Arc<Grammar>> {
        if let Some(g) = self.cache_get(name) {
            return Some(g);
        }
        let spec = self.registry.by_name(name)?;
        let grammar = Grammar::load(name, spec, &self.loader, &self.sources).ok()?;
        Some(self.cache_insert(name, grammar))
    }

    /// Resolve a path to a loaded grammar via its file extension.
    pub fn for_path(&self, path: &Path) -> Option<Arc<Grammar>> {
        let name = self.registry.name_for_path(path)?.to_string();
        self.by_name(&name)
    }

    fn cache_get(&self, name: &str) -> Option<Arc<Grammar>> {
        let g = self.cache.lock().ok()?;
        g.get(name).cloned()
    }

    fn cache_insert(&self, name: &str, grammar: Grammar) -> Arc<Grammar> {
        let arc = Arc::new(grammar);
        if let Ok(mut g) = self.cache.lock() {
            // Lost-race guard: if another caller raced us to insert, prefer
            // theirs so all consumers share the same Arc.
            if let Some(existing) = g.get(name) {
                return existing.clone();
            }
            g.insert(name.to_string(), arc.clone());
        }
        arc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// `for_path` triggers a clone+compile on cache miss, so it stays
    /// `#[ignore]`-gated. Verify the no-load fast paths separately.
    #[test]
    #[ignore = "network + compiler: clones + builds tree-sitter-rust"]
    fn for_path_returns_grammar_for_known_extension() {
        let dir = LanguageDirectory::new().unwrap();
        let g = dir.for_path(&PathBuf::from("foo.rs")).unwrap();
        assert_eq!(g.name(), "rust");
    }

    #[test]
    fn for_path_returns_none_for_unknown_extension() {
        let dir = LanguageDirectory::new().unwrap();
        assert!(dir.for_path(&PathBuf::from("foo.zzznope")).is_none());
    }
}
