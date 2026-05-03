//! Grammar loader — walks the lookup chain to resolve a language name to a
//! ready-to-`dlopen` parser, installing the highlights query alongside.
//!
//! Lookup order:
//!   1. **System**: `<system_dir>/<name><ext>` — distro-shipped, never built.
//!   2. **User**: `<user_dir>/<name><ext>` — durable user-data layer; this
//!      is also where on-demand builds get installed to so they survive
//!      across runs.
//!   3. **Build on demand**: clone source via [`SourceCache`], compile via
//!      [`GrammarCompiler`] (which writes `<source_root>/<name><ext>`),
//!      then install the parser + `<query_dir>/highlights.scm` into
//!      `<user_dir>/<name><ext>` + `<user_dir>/<name>.scm`.
//!
//! Layout written by the install step (and expected by [`Grammar::load`]):
//! - `<user_dir>/<name><ext>` — parser
//! - `<user_dir>/<name>.scm`  — highlights query
//!
//! Distro maintainers shipping pre-built grammars should reproduce that
//! layout under one of `system_dirs`. `cargo xtask build-grammars` does
//! exactly this.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::compile::{GrammarCompiler, shared_lib_ext};
use super::grammar::HIGHLIGHTS_FILE;
use super::manifest::LangSpec;
use super::source::SourceCache;

/// Configurable grammar resolver. Construct once, reuse across lookups.
#[derive(Debug, Clone)]
pub struct GrammarLoader {
    system_dirs: Vec<PathBuf>,
    user_dir: PathBuf,
    sources: SourceCache,
    compiler: GrammarCompiler,
}

impl GrammarLoader {
    /// Build a loader with explicit lookup paths. `user_dir` is both the
    /// place we look first (after `system_dirs`) and the install target
    /// for on-demand builds.
    pub fn new(
        system_dirs: Vec<PathBuf>,
        user_dir: PathBuf,
        sources: SourceCache,
        compiler: GrammarCompiler,
    ) -> Self {
        Self {
            system_dirs,
            user_dir,
            sources,
            compiler,
        }
    }

    /// Default loader for end-user installs:
    /// - system: `/usr/share/hjkl/grammars/`,
    ///   `/usr/local/share/hjkl/grammars/` (Unix only)
    /// - user:   platform user-data dir + `hjkl/grammars/`
    /// - sources / compile: [`SourceCache::user_default`] +
    ///   [`GrammarCompiler::new`]
    pub fn user_default() -> Result<Self> {
        let system_dirs = if cfg!(target_family = "unix") {
            vec![
                PathBuf::from("/usr/share/hjkl/grammars"),
                PathBuf::from("/usr/local/share/hjkl/grammars"),
            ]
        } else {
            Vec::new()
        };
        let mut user = dirs::data_dir().context("could not resolve user data directory")?;
        user.push("hjkl/grammars");
        Ok(Self::new(
            system_dirs,
            user,
            SourceCache::user_default()?,
            GrammarCompiler::new(),
        ))
    }

    /// User install dir. Parser + queries install here on demand.
    pub fn user_dir(&self) -> &Path {
        &self.user_dir
    }

    /// Resolve `name` (with its [`LangSpec`]) to a shared library path,
    /// triggering a clone + compile + install if no shipped artifact
    /// exists yet.
    pub fn load(&self, name: &str, spec: &LangSpec) -> Result<PathBuf> {
        if let Some(p) = self.lookup_only(name) {
            return Ok(p);
        }

        let source_root = self
            .sources
            .acquire(name, spec)
            .with_context(|| format!("acquire source for {name}"))?;
        let built = self
            .compiler
            .compile(name, spec, &source_root)
            .with_context(|| format!("compile grammar {name}"))?;
        install_into_user_dir(name, spec, &built, &source_root, &self.user_dir)
            .with_context(|| format!("install grammar {name}"))
    }

    /// Same lookup as [`Self::load`] but without falling back to clone +
    /// build. Returns `None` if no pre-shipped artifact is available —
    /// useful for callers that want to surface a "needs build" prompt
    /// before paying the cost.
    pub fn lookup_only(&self, name: &str) -> Option<PathBuf> {
        let flat = format!("{name}{}", shared_lib_ext());
        for dir in self
            .system_dirs
            .iter()
            .chain(std::iter::once(&self.user_dir))
        {
            let candidate = dir.join(&flat);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }
}

/// Copy `built_so` to `<user_dir>/<name><ext>` and the upstream
/// `highlights.scm` to `<user_dir>/<name>.scm`. Returns the installed
/// parser path (i.e. the path subsequent `lookup_only` calls will hit).
fn install_into_user_dir(
    name: &str,
    spec: &LangSpec,
    built_so: &Path,
    source_root: &Path,
    user_dir: &Path,
) -> Result<PathBuf> {
    std::fs::create_dir_all(user_dir)
        .with_context(|| format!("create user dir {}", user_dir.display()))?;

    let dest = user_dir.join(format!("{name}{}", shared_lib_ext()));
    copy_atomic(built_so, &dest)?;

    let highlights_src = source_root.join(&spec.query_dir).join(HIGHLIGHTS_FILE);
    if !highlights_src.is_file() {
        bail!(
            "highlights.scm missing in source clone: {}",
            highlights_src.display()
        );
    }
    let highlights_dest = user_dir.join(format!("{name}.scm"));
    copy_atomic(&highlights_src, &highlights_dest)?;

    Ok(dest)
}

/// Copy `from` to `to` via a sibling staging file + rename. Tolerates a
/// concurrent install winning the race (treats existing `to` as success).
fn copy_atomic(from: &Path, to: &Path) -> Result<()> {
    let staging = to.with_file_name(format!(
        "{}.tmp-{}",
        to.file_name().and_then(|s| s.to_str()).unwrap_or("install"),
        std::process::id(),
    ));
    let _ = std::fs::remove_file(&staging);
    std::fs::copy(from, &staging)
        .with_context(|| format!("copy {} -> {}", from.display(), staging.display()))?;
    match std::fs::rename(&staging, to) {
        Ok(()) => Ok(()),
        Err(_) if to.exists() => {
            let _ = std::fs::remove_file(&staging);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&staging);
            Err(e).with_context(|| format!("rename {} -> {}", staging.display(), to.display()))
        }
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

    fn loader_with(
        system_dirs: Vec<PathBuf>,
        user_dir: PathBuf,
        cache_root: PathBuf,
    ) -> GrammarLoader {
        GrammarLoader::new(
            system_dirs,
            user_dir,
            SourceCache::new(cache_root),
            GrammarCompiler::new(),
        )
    }

    #[test]
    fn system_dir_wins_over_user() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = tmp.path().join("sys");
        let user = tmp.path().join("user");
        let loader = loader_with(vec![sys.clone()], user.clone(), tmp.path().join("cache"));

        let name = "rust";
        let want = sys.join(format!("{name}{}", shared_lib_ext()));
        let other = user.join(format!("{name}{}", shared_lib_ext()));
        touch(&want);
        touch(&other);

        assert_eq!(loader.lookup_only(name), Some(want));
    }

    #[test]
    fn user_dir_used_when_no_system_match() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let loader = loader_with(vec![], user.clone(), tmp.path().join("cache"));

        let name = "python";
        let want = user.join(format!("{name}{}", shared_lib_ext()));
        touch(&want);

        assert_eq!(loader.lookup_only(name), Some(want));
    }

    #[test]
    fn lookup_only_returns_none_when_nothing_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let loader = loader_with(vec![], tmp.path().join("user"), tmp.path().join("cache"));
        assert!(loader.lookup_only("nope").is_none());
    }

    #[test]
    fn load_short_circuits_on_user_dir_hit() {
        // load() must not attempt a clone when the user dir already has
        // the parser — bogus git URL would fail any acquire.
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let loader = loader_with(vec![], user.clone(), tmp.path().join("cache"));

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
        let pre = user.join(format!("{name}{}", shared_lib_ext()));
        touch(&pre);

        let resolved = loader.load(name, &spec).unwrap();
        assert_eq!(resolved, pre);
    }

    #[test]
    fn install_copies_parser_and_highlights_query() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let source_root = tmp.path().join("source");
        let queries = source_root.join("queries");
        std::fs::create_dir_all(&queries).unwrap();
        std::fs::write(queries.join("highlights.scm"), "; highlights").unwrap();
        // locals.scm / injections.scm intentionally present — must be ignored.
        std::fs::write(queries.join("locals.scm"), "; locals").unwrap();

        let built = source_root.join(format!("rust{}", shared_lib_ext()));
        std::fs::write(&built, b"fake parser bytes").unwrap();

        let spec = dummy_spec("deadbeef00000000");
        let installed = install_into_user_dir("rust", &spec, &built, &source_root, &user).unwrap();

        assert_eq!(installed, user.join(format!("rust{}", shared_lib_ext())));
        assert!(installed.is_file());
        assert!(user.join("rust.scm").is_file());
        assert!(!user.join("rust").exists(), "no per-lang subdir expected");
    }

    #[test]
    fn install_errors_when_no_queries_present() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let source_root = tmp.path().join("source");
        std::fs::create_dir_all(source_root.join("queries")).unwrap();
        let built = source_root.join(format!("rust{}", shared_lib_ext()));
        std::fs::write(&built, b"x").unwrap();

        let spec = dummy_spec("deadbeef00000000");
        let err = install_into_user_dir("rust", &spec, &built, &source_root, &user).unwrap_err();
        assert!(
            err.to_string().contains("highlights.scm missing"),
            "got: {err:#}"
        );
    }
}
