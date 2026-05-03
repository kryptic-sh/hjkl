//! Grammar loader — walks the lookup chain to resolve a language name to a
//! ready-to-`dlopen` shared library path.
//!
//! Lookup order:
//!   1. **System**: `<system_dir>/<name><ext>` — distro-shipped, never built.
//!   2. **User**: `<user_dir>/<name><ext>` — user-shipped (e.g. hand-built).
//!   3. **Cache**: previously compiled artifact in
//!      `$XDG_CACHE_HOME/hjkl/grammars/`.
//!   4. **Compile**: clone the source via [`SourceCache`], then build via
//!      [`GrammarCompiler`].
//!
//! Steps 1–2 use a flat `<name><ext>` filename (matches what distro
//! maintainers ship via `cargo xtask build-grammars`). Step 3 uses the
//! content-addressed `<name>-<short-rev>-abi<N><ext>` format from
//! [`GrammarCompiler::artifact_path`] so multiple revs / ABIs coexist.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use super::compile::GrammarCompiler;
use super::manifest::LangSpec;
use super::source::SourceCache;

/// Configurable grammar resolver. Construct once, reuse across lookups.
#[derive(Debug, Clone)]
pub struct GrammarLoader {
    system_dirs: Vec<PathBuf>,
    user_dirs: Vec<PathBuf>,
    sources: SourceCache,
    compiler: GrammarCompiler,
}

impl GrammarLoader {
    /// Build a loader with the supplied lookup directories. Pre-built lookup
    /// directories are checked in order before falling back to compile.
    pub fn new(
        system_dirs: Vec<PathBuf>,
        user_dirs: Vec<PathBuf>,
        sources: SourceCache,
        compiler: GrammarCompiler,
    ) -> Self {
        Self {
            system_dirs,
            user_dirs,
            sources,
            compiler,
        }
    }

    /// Default loader for end-user installs:
    /// - system: `/usr/share/hjkl/runtime/grammars/`,
    ///   `/usr/local/share/hjkl/runtime/grammars/` (Unix only)
    /// - user:   platform user-data dir + `hjkl/runtime/grammars/`
    /// - sources / compile: [`SourceCache::user_default`] +
    ///   [`GrammarCompiler::user_default`]
    pub fn user_default() -> Result<Self> {
        let system_dirs = if cfg!(target_family = "unix") {
            vec![
                PathBuf::from("/usr/share/hjkl/runtime/grammars"),
                PathBuf::from("/usr/local/share/hjkl/runtime/grammars"),
            ]
        } else {
            Vec::new()
        };
        let mut user = dirs::data_dir().context("could not resolve user data directory")?;
        user.push("hjkl/runtime/grammars");
        Ok(Self::new(
            system_dirs,
            vec![user],
            SourceCache::user_default()?,
            GrammarCompiler::user_default()?,
        ))
    }

    /// Resolve `name` (with its [`LangSpec`]) to a shared library path,
    /// triggering a clone + compile if no cached / shipped artifact exists.
    pub fn load(&self, name: &str, spec: &LangSpec) -> Result<PathBuf> {
        let ext = shared_lib_ext();
        let flat_name = format!("{name}{ext}");

        for dir in self.system_dirs.iter().chain(self.user_dirs.iter()) {
            let candidate = dir.join(&flat_name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }

        let cached = self.compiler.artifact_path(name, spec);
        if cached.is_file() {
            return Ok(cached);
        }

        let source_root = self
            .sources
            .acquire(name, spec)
            .with_context(|| format!("acquire source for {name}"))?;
        let built = self
            .compiler
            .compile(name, spec, &source_root)
            .with_context(|| format!("compile grammar {name}"))?;
        if !built.is_file() {
            bail!(
                "compiler reported success but artifact missing: {}",
                built.display()
            );
        }
        Ok(built)
    }

    /// The same lookup as [`load`] but without falling back to clone + build.
    /// Returns `Ok(None)` if no pre-built artifact is available. Useful for
    /// callers that want to surface a "needs build" prompt before paying the
    /// cost.
    pub fn lookup_only(&self, name: &str, spec: &LangSpec) -> Option<PathBuf> {
        let flat = format!("{name}{}", shared_lib_ext());
        for dir in self.system_dirs.iter().chain(self.user_dirs.iter()) {
            let candidate = dir.join(&flat);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        let cached = self.compiler.artifact_path(name, spec);
        cached.is_file().then_some(cached)
    }
}

fn shared_lib_ext() -> &'static str {
    if cfg!(target_os = "macos") {
        ".dylib"
    } else if cfg!(target_os = "windows") {
        ".dll"
    } else {
        ".so"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_spec(rev: &str) -> LangSpec {
        LangSpec {
            git_url: "https://example/repo".into(),
            git_rev: rev.into(),
            subpath: None,
            extensions: vec!["x".into()],
            c_files: vec!["src/parser.c".into()],
            query_dir: "queries".into(),
            source: None,
        }
    }

    fn touch(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"\0\0\0\0").unwrap();
    }

    #[test]
    fn system_dir_wins_over_user() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = tmp.path().join("sys");
        let user = tmp.path().join("user");
        let cache_src = SourceCache::new(tmp.path().join("src"));
        let cache_out = GrammarCompiler::new(tmp.path().join("out"));
        let loader =
            GrammarLoader::new(vec![sys.clone()], vec![user.clone()], cache_src, cache_out);

        let name = "rust";
        let spec = dummy_spec("deadbeef00000000");
        let want = sys.join(format!("{name}{}", shared_lib_ext()));
        let other = user.join(format!("{name}{}", shared_lib_ext()));
        touch(&want);
        touch(&other);

        assert_eq!(loader.lookup_only(name, &spec), Some(want));
    }

    #[test]
    fn user_dir_used_when_no_system_match() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = tmp.path().join("sys");
        let user = tmp.path().join("user");
        let cache_src = SourceCache::new(tmp.path().join("src"));
        let cache_out = GrammarCompiler::new(tmp.path().join("out"));
        let loader = GrammarLoader::new(vec![sys], vec![user.clone()], cache_src, cache_out);

        let name = "python";
        let spec = dummy_spec("deadbeef00000000");
        let want = user.join(format!("{name}{}", shared_lib_ext()));
        touch(&want);

        assert_eq!(loader.lookup_only(name, &spec), Some(want));
    }

    #[test]
    fn cached_artifact_returned_when_no_shipped_match() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_src = SourceCache::new(tmp.path().join("src"));
        let cache_out = GrammarCompiler::new(tmp.path().join("out"));
        let loader = GrammarLoader::new(vec![], vec![], cache_src, cache_out.clone());

        let name = "go";
        let spec = dummy_spec("0123456789abcdef00112233");
        let cached = cache_out.artifact_path(name, &spec);
        touch(&cached);

        assert_eq!(loader.lookup_only(name, &spec), Some(cached));
    }

    #[test]
    fn lookup_only_returns_none_when_nothing_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_src = SourceCache::new(tmp.path().join("src"));
        let cache_out = GrammarCompiler::new(tmp.path().join("out"));
        let loader = GrammarLoader::new(vec![], vec![], cache_src, cache_out);

        let spec = dummy_spec("deadbeef00000000");
        assert!(loader.lookup_only("nope", &spec).is_none());
    }

    #[test]
    fn load_short_circuits_on_cached_artifact() {
        // Verify load() does not attempt a clone when a cached artifact is
        // present — git URL is bogus, so any clone attempt would fail.
        let tmp = tempfile::tempdir().unwrap();
        let cache_src = SourceCache::new(tmp.path().join("src"));
        let cache_out = GrammarCompiler::new(tmp.path().join("out"));
        let loader = GrammarLoader::new(vec![], vec![], cache_src, cache_out.clone());

        let name = "fake";
        let spec = LangSpec {
            git_url: "https://invalid.invalid/should-never-be-fetched".into(),
            git_rev: "0000000000000000".into(),
            subpath: None,
            extensions: vec!["x".into()],
            c_files: vec!["src/parser.c".into()],
            query_dir: "queries".into(),
            source: None,
        };
        let cached = cache_out.artifact_path(name, &spec);
        touch(&cached);

        let resolved = loader.load(name, &spec).unwrap();
        assert_eq!(resolved, cached);
    }
}
