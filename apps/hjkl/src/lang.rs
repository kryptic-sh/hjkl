//! Language directory — facade over hjkl-bonsai's runtime grammar API.
//!
//! Wraps a [`GrammarRegistry`] (manifest lookup), [`AsyncGrammarLoader`]
//! (non-blocking clone+compile), and a cache behind a single struct that
//! resolves a `Path` or language name to a cached `Arc<Grammar>`.
//!
//! ## Fast paths
//!
//! 1. **Cache hit** — returns `GrammarRequest::Cached` immediately.
//! 2. **Disk hit** — `lookup_fresh` found a fresh installed parser without
//!    any network I/O; builds the Grammar synchronously (cheap: just dlopen
//!    + read two `.scm` files) and returns `GrammarRequest::Cached`.
//! 3. **True miss** — clone+compile required; fires `load_async` on the
//!    background pool, returns `GrammarRequest::Loading { name, handle }`.
//!    The caller polls `handle.try_recv()` each tick and calls
//!    `complete_load` when the handle resolves.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use hjkl_bonsai::runtime::{
    AsyncGrammarLoader, Grammar, GrammarLoader, GrammarRegistry, LoadHandle,
};

/// Result of an async-friendly grammar resolution request.
pub enum GrammarRequest {
    /// Already cached — install immediately.
    Cached(Arc<Grammar>),
    /// First time we've seen this language — clone+compile is in flight on
    /// the background pool.  Caller must poll `handle.try_recv()` each tick
    /// and call `complete_load(name, path)` when `Some(Ok(path))` arrives.
    Loading { name: String, handle: LoadHandle },
    /// No spec / no recognized extension.  Plain-text only for this buffer.
    Unknown,
}

/// Shared language resolver. `Arc<LanguageDirectory>` is the right thing to
/// pass around so the in-memory `Grammar` cache is shared across the
/// `SyntaxLayer` and every picker source.
pub struct LanguageDirectory {
    registry: GrammarRegistry,
    async_loader: AsyncGrammarLoader,
    cache: Mutex<HashMap<String, Arc<Grammar>>>,
}

impl LanguageDirectory {
    /// Build a new directory rooted at the user's standard XDG dirs. Fails
    /// only if the embedded `bonsai.toml` doesn't parse or `$HOME` is unset.
    pub fn new() -> Result<Self> {
        let registry = GrammarRegistry::embedded()?;
        let loader = GrammarLoader::user_default(registry.meta())?;
        let async_loader = AsyncGrammarLoader::new(loader);
        Ok(Self {
            registry,
            async_loader,
            cache: Mutex::new(HashMap::new()),
        })
    }

    // ── Async-friendly API (UI-thread callers) ────────────────────────────────

    /// Async-friendly resolution by path.  Returns immediately; never blocks
    /// on clone+compile.  See module-level docs for the three fast paths.
    pub fn request_for_path(&self, path: &Path) -> GrammarRequest {
        let name = match self.registry.name_for_path(path) {
            Some(n) => n.to_string(),
            None => return GrammarRequest::Unknown,
        };
        self.request_by_name(&name)
    }

    /// Async-friendly resolution by language name.  Returns immediately.
    pub fn request_by_name(&self, name: &str) -> GrammarRequest {
        // Fast path 1: cache hit.
        if let Some(g) = self.cache_get(name) {
            return GrammarRequest::Cached(g);
        }

        let spec = match self.registry.by_name(name) {
            Some(s) => s,
            None => return GrammarRequest::Unknown,
        };
        let meta = self.registry.meta();

        // Fast path 2: already installed on disk (lookup_fresh short-circuits).
        if let Some(path) = self.async_loader.inner().lookup_fresh(name, spec, meta) {
            match Grammar::load_from_path(name, &path) {
                Ok(g) => {
                    let arc = self.cache_insert(name, g);
                    return GrammarRequest::Cached(arc);
                }
                Err(e) => {
                    tracing::debug!("load_from_path({name}) failed after lookup_fresh: {e:#}");
                    // Fall through to async load — the .so may be corrupted.
                }
            }
        }

        // Slow path: kick off (or subscribe to) a background clone+compile.
        let handle = self
            .async_loader
            .load_async(name.to_string(), spec.clone(), meta.clone());
        GrammarRequest::Loading {
            name: name.to_string(),
            handle,
        }
    }

    /// Snapshot of grammar names currently in flight on the async pool.
    /// Order unspecified. Surfaced to the renderer so a single
    /// status-bar spinner can reflect *any* queued lang (active buffer,
    /// preview pane, or otherwise), not just the focused one.
    pub fn in_flight_names(&self) -> Vec<String> {
        self.async_loader.in_flight_names()
    }

    /// Called by the consumer when a `Loading` handle resolves with
    /// `Some(Ok(lib_path))`.  Constructs the `Grammar`, caches it, returns
    /// the `Arc`.
    pub fn complete_load(&self, name: &str, lib_path: PathBuf) -> Result<Arc<Grammar>> {
        let grammar = Grammar::load_from_path(name, &lib_path)?;
        Ok(self.cache_insert(name, grammar))
    }

    // ── Sync API (picker-thread callers; blocking is OK there) ───────────────

    /// Resolve a language name (e.g. `"rust"`, `"python"`) to a loaded
    /// grammar.  May block on first use to clone + compile + install.
    /// Pickers run on their own background threads so blocking is fine.
    pub fn by_name(&self, name: &str) -> Option<Arc<Grammar>> {
        if let Some(g) = self.cache_get(name) {
            return Some(g);
        }
        let spec = self.registry.by_name(name)?;
        let meta = self.registry.meta();
        let grammar = Grammar::load(name, spec, self.async_loader.inner(), meta).ok()?;
        Some(self.cache_insert(name, grammar))
    }

    // ── Cache helpers ─────────────────────────────────────────────────────────

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

    #[test]
    fn request_for_path_returns_unknown_for_unrecognized_extension() {
        let dir = LanguageDirectory::new().unwrap();
        assert!(matches!(
            dir.request_for_path(&PathBuf::from("foo.zzznope")),
            GrammarRequest::Unknown
        ));
    }

    /// On a cold system (no grammar installed) request_for_path should
    /// return Loading (or Cached if the grammar happens to already be
    /// installed) — either way it must NOT block the caller for a
    /// clone+compile.  We can't easily assert "Loading" without controlling
    /// the disk, so this test only validates the Unknown fast-path; the
    /// Loading/Cached paths are covered by the ignore-gated integration
    /// tests in bonsai.
    #[test]
    fn request_by_name_returns_unknown_for_nonexistent_lang() {
        let dir = LanguageDirectory::new().unwrap();
        assert!(matches!(
            dir.request_by_name("definitely_not_a_real_language"),
            GrammarRequest::Unknown
        ));
    }
}
