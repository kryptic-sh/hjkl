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
//!      then install the parser + resolved `highlights.scm` into
//!      `<user_dir>/<name><ext>` + `<user_dir>/<name>.scm`.
//!
//! Layout written by the install step (and expected by [`Grammar::load`]):
//! - `<user_dir>/<name><ext>` — parser
//! - `<user_dir>/<name>.scm`  — highlights query
//! - `<user_dir>/<name>.rev`  — sidecar `<git_rev>:<query_short_rev>:abi<N>` for
//!   staleness detection. Updating either the grammar rev or the query-source rev
//!   triggers a re-install (overwriting in place). Old two-field sidecars parse
//!   as stale → rebuild (correct behavior across the schema bump).
//!   System dirs are *not* rev-checked: distro packagers own that lifecycle.
//!
//! Distro maintainers shipping pre-built grammars should reproduce the
//! parser + .scm pair under one of `system_dirs`. `cargo xtask
//! build-grammars` does exactly this (and writes the sidecar too).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::compile::{GrammarCompiler, shared_lib_ext};
use super::manifest::{LangSpec, ManifestMeta};
use super::source::{QuerySourceCache, SourceCache, short_rev};
use super::xdg;

/// Configurable grammar resolver. Construct once, reuse across lookups.
#[derive(Debug, Clone)]
pub struct GrammarLoader {
    system_dirs: Vec<PathBuf>,
    user_dir: PathBuf,
    sources: SourceCache,
    query_sources: QuerySourceCache,
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
        query_sources: QuerySourceCache,
        compiler: GrammarCompiler,
    ) -> Self {
        Self {
            system_dirs,
            user_dir,
            sources,
            query_sources,
            compiler,
        }
    }

    /// Default loader for end-user installs:
    /// - system: `/usr/share/bonsai/grammars/`,
    ///   `/usr/local/share/bonsai/grammars/` (Unix only)
    /// - user:   `$XDG_DATA_HOME/bonsai/grammars/` (XDG-everywhere; falls back
    ///   to `~/.local/share/bonsai/grammars/` on every platform)
    /// - sources / compile: [`SourceCache::user_default`] +
    ///   [`GrammarCompiler::new`]
    pub fn user_default(_meta: &ManifestMeta) -> Result<Self> {
        let system_dirs = if cfg!(target_family = "unix") {
            vec![
                PathBuf::from("/usr/share/bonsai/grammars"),
                PathBuf::from("/usr/local/share/bonsai/grammars"),
            ]
        } else {
            Vec::new()
        };
        let user = xdg::data_home()?.join("bonsai/grammars");
        Ok(Self::new(
            system_dirs,
            user,
            SourceCache::user_default()?,
            QuerySourceCache::user_default()?,
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
    pub fn load(&self, name: &str, spec: &LangSpec, meta: &ManifestMeta) -> Result<PathBuf> {
        if let Some(p) = self.lookup_fresh(name, spec, meta) {
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

        let highlights_src = self
            .query_sources
            .resolve_highlights(spec.query_source, meta, name, spec.query_subdir.as_deref())
            .with_context(|| format!("resolve highlights for {name}"))?;

        // Injections come from the grammar's own source tree (not the curated
        // query repos — those use non-standard predicates). Optional: None when
        // the grammar does not ship injections.scm.
        let injections_src = self
            .sources
            .injections_path(&source_root)
            .with_context(|| format!("check injections for {name}"))?;

        let query_rev = match spec.query_source {
            crate::runtime::manifest::QuerySource::Helix => meta.helix_rev.as_str(),
            crate::runtime::manifest::QuerySource::NvimTreesitter => {
                meta.nvim_treesitter_rev.as_str()
            }
        };
        install_into_user_dir(
            name,
            spec,
            &built,
            &highlights_src,
            injections_src.as_deref(),
            query_rev,
            &self.user_dir,
        )
        .with_context(|| format!("install grammar {name}"))
    }

    /// Look up an installed parser **with** the freshness check applied
    /// to the user-dir tier. System dirs are returned on first hit
    /// regardless of rev (distro's responsibility). User-dir hits are
    /// filtered against the sidecar — stale installs read as `None` so
    /// the caller falls through to a recompile.
    pub fn lookup_fresh(
        &self,
        name: &str,
        spec: &LangSpec,
        meta: &ManifestMeta,
    ) -> Option<PathBuf> {
        let flat = format!("{name}{}", shared_lib_ext());
        for dir in &self.system_dirs {
            let candidate = dir.join(&flat);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        let user_candidate = self.user_dir.join(&flat);
        if user_candidate.is_file() && is_user_install_fresh(&self.user_dir, name, spec, meta) {
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
/// rev + query-source rev + the tree-sitter ABI we're built against. Missing
/// or unparseable sidecars count as stale.
fn is_user_install_fresh(
    user_dir: &Path,
    name: &str,
    spec: &LangSpec,
    meta: &ManifestMeta,
) -> bool {
    let rev_path = user_dir.join(format!("{name}.rev"));
    let Ok(content) = std::fs::read_to_string(&rev_path) else {
        return false;
    };
    let Some((grammar_rev, query_short, abi)) = parse_rev_sidecar(content.trim()) else {
        return false;
    };
    let expected_query_rev = match spec.query_source {
        crate::runtime::manifest::QuerySource::Helix => meta.helix_rev.as_str(),
        crate::runtime::manifest::QuerySource::NvimTreesitter => meta.nvim_treesitter_rev.as_str(),
    };
    grammar_rev == spec.git_rev
        && query_short == short_rev(expected_query_rev)
        && abi == tree_sitter::LANGUAGE_VERSION
}

/// Parse the three-field `<git_rev>:<query_short_rev>:abi<N>` payload.
/// `None` when malformed (including old two-field format → stale → rebuild).
fn parse_rev_sidecar(s: &str) -> Option<(&str, &str, usize)> {
    let mut parts = s.splitn(3, ':');
    let grammar_rev = parts.next()?;
    let query_short = parts.next()?;
    let abi_part = parts.next()?;
    let abi_str = abi_part.strip_prefix("abi")?;
    let abi = abi_str.parse().ok()?;
    Some((grammar_rev, query_short, abi))
}

/// Copy `built_so` to `<user_dir>/<name><ext>`, `highlights_src` to
/// `<user_dir>/<name>.scm`, optionally copy `injections_src` to
/// `<user_dir>/<name>.injections.scm`, then write the `<user_dir>/<name>.rev`
/// sidecar **last** so an interrupted install leaves no partial set of files.
/// Returns the installed parser path.
fn install_into_user_dir(
    name: &str,
    spec: &LangSpec,
    built_so: &Path,
    highlights_src: &Path,
    injections_src: Option<&Path>,
    query_rev: &str,
    user_dir: &Path,
) -> Result<PathBuf> {
    if !highlights_src.is_file() {
        bail!(
            "highlights.scm missing in source clone: {}",
            highlights_src.display()
        );
    }

    std::fs::create_dir_all(user_dir)
        .with_context(|| format!("create user dir {}", user_dir.display()))?;

    let dest = user_dir.join(format!("{name}{}", shared_lib_ext()));
    copy_atomic(built_so, &dest)?;

    let highlights_dest = user_dir.join(format!("{name}.scm"));
    copy_atomic(highlights_src, &highlights_dest)?;

    if let Some(inj_src) = injections_src {
        let inj_dest = user_dir.join(format!("{name}.injections.scm"));
        copy_atomic(inj_src, &inj_dest)?;
    }

    let rev_dest = user_dir.join(format!("{name}.rev"));
    let rev_payload = format!(
        "{}:{}:abi{}",
        spec.git_rev,
        short_rev(query_rev),
        tree_sitter::LANGUAGE_VERSION
    );
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
    use crate::runtime::manifest::QuerySource;
    use crate::runtime::source::QuerySourceCache;

    fn dummy_meta() -> ManifestMeta {
        ManifestMeta {
            helix_repo: "https://github.com/helix-editor/helix".into(),
            helix_rev: "aaaa0000bbbb1111cccc2222dddd3333eeee4444".into(),
            nvim_treesitter_repo: "https://github.com/nvim-treesitter/nvim-treesitter".into(),
            nvim_treesitter_rev: "ffff5555aaaa0000bbbb1111cccc2222dddd3333".into(),
        }
    }

    fn dummy_spec(rev: &str) -> LangSpec {
        LangSpec {
            git_url: "https://example/repo".into(),
            git_rev: rev.into(),
            subpath: None,
            extensions: vec!["x".into()],
            c_files: vec!["src/parser.c".into()],
            query_source: QuerySource::Helix,
            query_subdir: None,
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
        query_cache_root: PathBuf,
    ) -> GrammarLoader {
        GrammarLoader::new(
            system_dirs,
            user_dir,
            SourceCache::new(cache_root),
            QuerySourceCache::new(query_cache_root),
            GrammarCompiler::new(),
        )
    }

    fn write_rev_sidecar(
        user_dir: &Path,
        name: &str,
        grammar_rev: &str,
        query_short: &str,
        abi: usize,
    ) {
        std::fs::create_dir_all(user_dir).unwrap();
        std::fs::write(
            user_dir.join(format!("{name}.rev")),
            format!("{grammar_rev}:{query_short}:abi{abi}"),
        )
        .unwrap();
    }

    #[test]
    fn system_dir_wins_over_user() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = tmp.path().join("sys");
        let user = tmp.path().join("user");
        let loader = loader_with(
            vec![sys.clone()],
            user.clone(),
            tmp.path().join("cache"),
            tmp.path().join("qcache"),
        );

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
        let loader = loader_with(
            vec![],
            user.clone(),
            tmp.path().join("cache"),
            tmp.path().join("qcache"),
        );

        let name = "python";
        let want = user.join(format!("{name}{}", shared_lib_ext()));
        touch(&want);

        assert_eq!(loader.lookup_only(name), Some(want));
    }

    #[test]
    fn lookup_only_returns_none_when_nothing_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let loader = loader_with(
            vec![],
            tmp.path().join("user"),
            tmp.path().join("cache"),
            tmp.path().join("qcache"),
        );
        assert!(loader.lookup_only("nope").is_none());
    }

    #[test]
    fn load_short_circuits_on_fresh_user_install() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let loader = loader_with(
            vec![],
            user.clone(),
            tmp.path().join("cache"),
            tmp.path().join("qcache"),
        );
        let meta = dummy_meta();

        let name = "fake";
        let rev = "0000000000000000";
        let spec = LangSpec {
            git_url: "https://invalid.invalid/should-never-be-fetched".into(),
            git_rev: rev.into(),
            subpath: None,
            extensions: vec!["x".into()],
            c_files: vec!["src/parser.c".into()],
            query_source: QuerySource::Helix,
            query_subdir: None,
            source: None,
        };
        let pre = user.join(format!("{name}{}", shared_lib_ext()));
        touch(&pre);
        write_rev_sidecar(
            &user,
            name,
            rev,
            short_rev(&meta.helix_rev),
            tree_sitter::LANGUAGE_VERSION,
        );

        let resolved = loader.load(name, &spec, &meta).unwrap();
        assert_eq!(resolved, pre);
    }

    #[test]
    fn lookup_fresh_misses_when_sidecar_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let loader = loader_with(
            vec![],
            user.clone(),
            tmp.path().join("cache"),
            tmp.path().join("qcache"),
        );
        let meta = dummy_meta();

        let spec = dummy_spec("aaaaaaaaaaaa");
        let pre = user.join(format!("rust{}", shared_lib_ext()));
        touch(&pre);

        assert!(loader.lookup_fresh("rust", &spec, &meta).is_none());
        assert_eq!(loader.lookup_only("rust"), Some(pre));
    }

    #[test]
    fn lookup_fresh_misses_when_rev_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let loader = loader_with(
            vec![],
            user.clone(),
            tmp.path().join("cache"),
            tmp.path().join("qcache"),
        );
        let meta = dummy_meta();

        let spec = dummy_spec("new-rev-aaaa");
        let pre = user.join(format!("rust{}", shared_lib_ext()));
        touch(&pre);
        write_rev_sidecar(
            &user,
            "rust",
            "old-rev-bbbb",
            short_rev(&meta.helix_rev),
            tree_sitter::LANGUAGE_VERSION,
        );

        assert!(loader.lookup_fresh("rust", &spec, &meta).is_none());
    }

    #[test]
    fn lookup_fresh_misses_when_abi_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let loader = loader_with(
            vec![],
            user.clone(),
            tmp.path().join("cache"),
            tmp.path().join("qcache"),
        );
        let meta = dummy_meta();

        let rev = "deadbeef0000";
        let spec = dummy_spec(rev);
        let pre = user.join(format!("rust{}", shared_lib_ext()));
        touch(&pre);
        let stale_abi = tree_sitter::LANGUAGE_VERSION + 1;
        write_rev_sidecar(&user, "rust", rev, short_rev(&meta.helix_rev), stale_abi);

        assert!(loader.lookup_fresh("rust", &spec, &meta).is_none());
    }

    #[test]
    fn lookup_fresh_skips_sidecar_check_for_system_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = tmp.path().join("sys");
        let user = tmp.path().join("user");
        let loader = loader_with(
            vec![sys.clone()],
            user,
            tmp.path().join("cache"),
            tmp.path().join("qcache"),
        );
        let meta = dummy_meta();

        let spec = dummy_spec("aaaaaaaaaaaa");
        let want = sys.join(format!("rust{}", shared_lib_ext()));
        touch(&want);

        assert_eq!(loader.lookup_fresh("rust", &spec, &meta), Some(want));
    }

    #[test]
    fn parse_rev_sidecar_parses_normal_payload() {
        assert_eq!(
            parse_rev_sidecar("abc123:deadbeef0000:abi15"),
            Some(("abc123", "deadbeef0000", 15))
        );
    }

    #[test]
    fn parse_rev_sidecar_rejects_old_two_field_format() {
        // Old format "rev:abiN" must parse as stale (None) so stale installs rebuild.
        assert!(parse_rev_sidecar("abc123:abi15").is_none());
    }

    #[test]
    fn parse_rev_sidecar_rejects_malformed() {
        assert!(parse_rev_sidecar("no-colon").is_none());
        assert!(parse_rev_sidecar("rev:qrev:abi").is_none()); // missing number
        assert!(parse_rev_sidecar("rev:qrev:15").is_none()); // missing abi prefix
    }

    #[test]
    fn install_copies_parser_highlights_and_writes_rev_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");

        // Pre-resolved highlights.scm (the loader hands in an already-resolved path).
        let highlights_src = tmp.path().join("rust.resolved.scm");
        std::fs::write(&highlights_src, "; highlights").unwrap();

        let built = tmp.path().join(format!("rust{}", shared_lib_ext()));
        std::fs::write(&built, b"fake parser bytes").unwrap();

        let rev = "deadbeef00000000";
        let query_rev = "aaaa0000bbbb1111cccc2222dddd3333eeee4444";
        let spec = dummy_spec(rev);
        let installed = install_into_user_dir(
            "rust",
            &spec,
            &built,
            &highlights_src,
            None,
            query_rev,
            &user,
        )
        .unwrap();

        assert_eq!(installed, user.join(format!("rust{}", shared_lib_ext())));
        assert!(installed.is_file());
        assert!(user.join("rust.scm").is_file());
        assert!(!user.join("rust").exists());

        let rev_payload = std::fs::read_to_string(user.join("rust.rev")).unwrap();
        assert_eq!(
            rev_payload,
            format!(
                "{rev}:{}:abi{}",
                short_rev(query_rev),
                tree_sitter::LANGUAGE_VERSION
            )
        );
    }

    #[test]
    fn install_errors_when_no_highlights_present() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let highlights_src = tmp.path().join("ghost.scm"); // does not exist
        let built = tmp.path().join(format!("rust{}", shared_lib_ext()));
        std::fs::write(&built, b"x").unwrap();

        let spec = dummy_spec("deadbeef00000000");
        let err =
            install_into_user_dir("rust", &spec, &built, &highlights_src, None, "qrev", &user)
                .unwrap_err();
        assert!(
            err.to_string().contains("highlights.scm missing"),
            "got: {err:#}"
        );
    }

    #[test]
    fn install_with_preresolved_path_produces_all_three_files() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");

        let highlights_src = tmp.path().join("xml.resolved.scm");
        std::fs::write(&highlights_src, "; xml highlights").unwrap();

        let built = tmp.path().join(format!("xml{}", shared_lib_ext()));
        std::fs::write(&built, b"fake xml parser bytes").unwrap();

        let rev = "0d9a8099c963ed53";
        let query_rev = "aaaa0000bbbb1111cccc2222dddd3333eeee4444";
        let spec = LangSpec {
            git_url: "https://github.com/tree-sitter-grammars/tree-sitter-xml".into(),
            git_rev: rev.into(),
            subpath: Some("xml".into()),
            extensions: vec!["xml".into()],
            c_files: vec!["src/parser.c".into(), "src/scanner.c".into()],
            query_source: QuerySource::Helix,
            query_subdir: None,
            source: Some("helix+nvim-treesitter".into()),
        };

        let installed = install_into_user_dir(
            "xml",
            &spec,
            &built,
            &highlights_src,
            None,
            query_rev,
            &user,
        )
        .unwrap();

        assert_eq!(
            installed,
            user.join(format!("xml{}", shared_lib_ext())),
            ".so path mismatch"
        );
        assert!(installed.is_file(), ".so missing from user_dir");
        assert!(user.join("xml.scm").is_file(), ".scm missing from user_dir");
        assert!(user.join("xml.rev").is_file(), ".rev missing from user_dir");

        let rev_payload = std::fs::read_to_string(user.join("xml.rev")).unwrap();
        assert_eq!(
            rev_payload,
            format!(
                "{rev}:{}:abi{}",
                short_rev(query_rev),
                tree_sitter::LANGUAGE_VERSION
            )
        );
    }

    #[test]
    fn install_leaves_no_so_in_user_dir_when_highlights_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let highlights_src = tmp.path().join("ghost.scm"); // absent
        let built = tmp.path().join(format!("xml{}", shared_lib_ext()));
        std::fs::write(&built, b"fake parser bytes").unwrap();

        let spec = dummy_spec("deadbeef00000000");
        let err = install_into_user_dir("xml", &spec, &built, &highlights_src, None, "qrev", &user)
            .unwrap_err();
        assert!(
            err.to_string().contains("highlights.scm missing"),
            "got: {err:#}"
        );

        let so_path = user.join(format!("xml{}", shared_lib_ext()));
        assert!(
            !so_path.exists(),
            ".so must not be written when highlights.scm is missing; found {so_path:?}"
        );
    }

    #[test]
    fn old_sidecar_format_counts_as_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let loader = loader_with(
            vec![],
            user.clone(),
            tmp.path().join("cache"),
            tmp.path().join("qcache"),
        );
        let meta = dummy_meta();

        let rev = "deadbeef0000";
        let spec = dummy_spec(rev);
        let pre = user.join(format!("rust{}", shared_lib_ext()));
        touch(&pre);
        // Write old two-field format — must be treated as stale.
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(
            user.join("rust.rev"),
            format!("{rev}:abi{}", tree_sitter::LANGUAGE_VERSION),
        )
        .unwrap();

        assert!(
            loader.lookup_fresh("rust", &spec, &meta).is_none(),
            "old sidecar must count as stale"
        );
    }
}
