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
//! - `<user_dir>/<name>.rev`  — sidecar `<git_rev>:abi<N>` for staleness
//!   detection. When the manifest pins a new rev or tree-sitter bumps
//!   its ABI, an existing user-dir install reads as stale and gets
//!   recompiled (overwriting in place — no stale files left behind).
//!   System dirs are *not* rev-checked: distro packagers own that
//!   lifecycle.
//!
//! Distro maintainers shipping pre-built grammars should reproduce the
//! parser + .scm pair under one of `system_dirs`. `cargo xtask
//! build-grammars` does exactly this (and writes the sidecar too).

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
    /// triggering a clone + compile + install when no fresh shipped
    /// artifact exists. A user-dir hit whose `<name>.rev` sidecar
    /// disagrees with `spec.git_rev` / current ABI is treated as stale
    /// and recompiled (overwriting in place).
    pub fn load(&self, name: &str, spec: &LangSpec) -> Result<PathBuf> {
        if let Some(p) = self.lookup_fresh(name, spec) {
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

    /// Look up an installed parser **with** the freshness check applied
    /// to the user-dir tier. System dirs are returned on first hit
    /// regardless of rev (distro's responsibility). User-dir hits are
    /// filtered against the sidecar — stale installs read as `None` so
    /// the caller falls through to a recompile.
    pub fn lookup_fresh(&self, name: &str, spec: &LangSpec) -> Option<PathBuf> {
        let flat = format!("{name}{}", shared_lib_ext());
        for dir in &self.system_dirs {
            let candidate = dir.join(&flat);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        let user_candidate = self.user_dir.join(&flat);
        if user_candidate.is_file() && is_user_install_fresh(&self.user_dir, name, spec) {
            return Some(user_candidate);
        }
        None
    }

    /// Lower-level lookup that ignores the sidecar — returns the first
    /// `<name><ext>` found under any lookup dir even if it's stale. Use
    /// this when you want to know "is anything installed at all" rather
    /// than "is there something we can use right now."
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

/// Read `<user_dir>/<name>.rev` and check it matches the spec's pinned
/// rev + the tree-sitter ABI we're built against. Missing or
/// unparseable sidecars count as stale.
fn is_user_install_fresh(user_dir: &Path, name: &str, spec: &LangSpec) -> bool {
    let rev_path = user_dir.join(format!("{name}.rev"));
    let Ok(content) = std::fs::read_to_string(&rev_path) else {
        return false;
    };
    let Some((rev, abi)) = parse_rev_sidecar(content.trim()) else {
        return false;
    };
    rev == spec.git_rev && abi == tree_sitter::LANGUAGE_VERSION
}

/// Parse the one-line `<git_rev>:abi<N>` payload. `None` when malformed.
fn parse_rev_sidecar(s: &str) -> Option<(&str, usize)> {
    let (rev, abi_part) = s.split_once(':')?;
    let abi_str = abi_part.strip_prefix("abi")?;
    let abi = abi_str.parse().ok()?;
    Some((rev, abi))
}

/// Copy `built_so` to `<user_dir>/<name><ext>` and the upstream
/// `highlights.scm` to `<user_dir>/<name>.scm`, then write the
/// `<user_dir>/<name>.rev` sidecar **last** so an interrupted install
/// leaves the previous (or no) rev recorded — the next `load` rebuilds.
/// Returns the installed parser path.
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

    let rev_dest = user_dir.join(format!("{name}.rev"));
    let rev_payload = format!("{}:abi{}", spec.git_rev, tree_sitter::LANGUAGE_VERSION);
    write_atomic(&rev_dest, rev_payload.as_bytes())?;

    Ok(dest)
}

/// Write `bytes` to `to` via a sibling staging file + rename.
fn write_atomic(to: &Path, bytes: &[u8]) -> Result<()> {
    let staging = staging_path(to);
    let _ = std::fs::remove_file(&staging);
    std::fs::write(&staging, bytes).with_context(|| format!("write {}", staging.display()))?;
    if let Err(e) = std::fs::rename(&staging, to) {
        let _ = std::fs::remove_file(&staging);
        return Err(e).with_context(|| format!("rename {} -> {}", staging.display(), to.display()));
    }
    Ok(())
}

fn staging_path(to: &Path) -> PathBuf {
    to.with_file_name(format!(
        "{}.tmp-{}",
        to.file_name().and_then(|s| s.to_str()).unwrap_or("install"),
        std::process::id(),
    ))
}

/// Copy `from` to `to` via a sibling staging file + rename. Tolerates a
/// concurrent install winning the race (treats existing `to` as success).
fn copy_atomic(from: &Path, to: &Path) -> Result<()> {
    let staging = staging_path(to);
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

    fn write_rev_sidecar(user_dir: &Path, name: &str, rev: &str, abi: usize) {
        std::fs::create_dir_all(user_dir).unwrap();
        std::fs::write(
            user_dir.join(format!("{name}.rev")),
            format!("{rev}:abi{abi}"),
        )
        .unwrap();
    }

    #[test]
    fn load_short_circuits_on_fresh_user_install() {
        // load() must not attempt a clone when the user dir already has
        // a parser whose .rev sidecar matches — bogus git URL would fail
        // any acquire.
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let loader = loader_with(vec![], user.clone(), tmp.path().join("cache"));

        let name = "fake";
        let rev = "0000000000000000";
        let spec = LangSpec {
            git_url: "https://invalid.invalid/should-never-be-fetched".into(),
            git_rev: rev.into(),
            subpath: None,
            extensions: vec!["x".into()],
            c_files: vec!["src/parser.c".into()],
            query_dir: "queries".into(),
            source: None,
        };
        let pre = user.join(format!("{name}{}", shared_lib_ext()));
        touch(&pre);
        write_rev_sidecar(&user, name, rev, tree_sitter::LANGUAGE_VERSION);

        let resolved = loader.load(name, &spec).unwrap();
        assert_eq!(resolved, pre);
    }

    #[test]
    fn lookup_fresh_misses_when_sidecar_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let loader = loader_with(vec![], user.clone(), tmp.path().join("cache"));

        let spec = dummy_spec("aaaaaaaaaaaa");
        let pre = user.join(format!("rust{}", shared_lib_ext()));
        touch(&pre);
        // No .rev sidecar — counts as stale.

        assert!(loader.lookup_fresh("rust", &spec).is_none());
        // lookup_only ignores the sidecar and still finds the .so.
        assert_eq!(loader.lookup_only("rust"), Some(pre));
    }

    #[test]
    fn lookup_fresh_misses_when_rev_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let loader = loader_with(vec![], user.clone(), tmp.path().join("cache"));

        let spec = dummy_spec("new-rev-aaaa");
        let pre = user.join(format!("rust{}", shared_lib_ext()));
        touch(&pre);
        write_rev_sidecar(&user, "rust", "old-rev-bbbb", tree_sitter::LANGUAGE_VERSION);

        assert!(loader.lookup_fresh("rust", &spec).is_none());
    }

    #[test]
    fn lookup_fresh_misses_when_abi_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let loader = loader_with(vec![], user.clone(), tmp.path().join("cache"));

        let rev = "deadbeef0000";
        let spec = dummy_spec(rev);
        let pre = user.join(format!("rust{}", shared_lib_ext()));
        touch(&pre);
        let stale_abi = tree_sitter::LANGUAGE_VERSION + 1;
        write_rev_sidecar(&user, "rust", rev, stale_abi);

        assert!(loader.lookup_fresh("rust", &spec).is_none());
    }

    #[test]
    fn lookup_fresh_skips_sidecar_check_for_system_dirs() {
        // Distros own the rev for system-dir installs — even with no
        // sidecar at all, a system-dir hit must be returned.
        let tmp = tempfile::tempdir().unwrap();
        let sys = tmp.path().join("sys");
        let user = tmp.path().join("user");
        let loader = loader_with(vec![sys.clone()], user, tmp.path().join("cache"));

        let spec = dummy_spec("aaaaaaaaaaaa");
        let want = sys.join(format!("rust{}", shared_lib_ext()));
        touch(&want);

        assert_eq!(loader.lookup_fresh("rust", &spec), Some(want));
    }

    #[test]
    fn parse_rev_sidecar_parses_normal_payload() {
        assert_eq!(parse_rev_sidecar("abc123:abi15"), Some(("abc123", 15)));
    }

    #[test]
    fn parse_rev_sidecar_rejects_malformed() {
        assert!(parse_rev_sidecar("no-colon").is_none());
        assert!(parse_rev_sidecar("rev:abi").is_none()); // missing number
        assert!(parse_rev_sidecar("rev:15").is_none()); // missing abi prefix
    }

    #[test]
    fn install_copies_parser_highlights_and_writes_rev_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let source_root = tmp.path().join("source");
        let queries = source_root.join("queries");
        std::fs::create_dir_all(&queries).unwrap();
        std::fs::write(queries.join("highlights.scm"), "; highlights").unwrap();
        // locals.scm intentionally present — must be ignored.
        std::fs::write(queries.join("locals.scm"), "; locals").unwrap();

        let built = source_root.join(format!("rust{}", shared_lib_ext()));
        std::fs::write(&built, b"fake parser bytes").unwrap();

        let rev = "deadbeef00000000";
        let spec = dummy_spec(rev);
        let installed = install_into_user_dir("rust", &spec, &built, &source_root, &user).unwrap();

        assert_eq!(installed, user.join(format!("rust{}", shared_lib_ext())));
        assert!(installed.is_file());
        assert!(user.join("rust.scm").is_file());
        assert!(!user.join("rust").exists(), "no per-lang subdir expected");

        let rev_payload = std::fs::read_to_string(user.join("rust.rev")).unwrap();
        assert_eq!(
            rev_payload,
            format!("{rev}:abi{}", tree_sitter::LANGUAGE_VERSION)
        );
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
