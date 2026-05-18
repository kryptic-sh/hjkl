//! Grammar source acquisition and query-source cache.
//!
//! `SourceCache` clones upstream grammar repos for compilation.
//! `QuerySourceCache` clones (with sparse checkout) the two curated query
//! repos (helix, nvim-treesitter) and resolves `highlights.scm`, expanding
//! `; inherits: foo,bar` chains into a single concatenated file.
//!
//! Strategy mirrors helix's `helix-loader`: shell out to `git`. Avoids
//! dragging in libgit2 and matches the assumption that bonsai consumers have
//! a developer toolchain installed.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context, Result, bail};

use super::manifest::{LangSpec, ManifestMeta, QuerySource};
use super::xdg;

/// Lazily-allocated per-key mutex map. Used by [`SourceCache`] and
/// [`QuerySourceCache`] to serialise concurrent `acquire_*` calls for the
/// same `(name, rev)` / `(label, rev)` so two threads never race on the
/// same staging directory or git working tree. Different keys still run
/// in parallel.
type KeyLocks = Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>;

/// Look up (or insert) the per-key mutex for `key` and return an `Arc`.
/// The outer mutex is held only for the duration of the map lookup.
fn key_lock(locks: &KeyLocks, key: &str) -> Arc<Mutex<()>> {
    let mut map = locks.lock().unwrap_or_else(PoisonError::into_inner);
    Arc::clone(
        map.entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

// ---------------------------------------------------------------------------
// SourceCache — grammar compilation trees
// ---------------------------------------------------------------------------

/// Cache of cloned grammar source trees.
#[derive(Debug, Clone)]
pub struct SourceCache {
    base: PathBuf,
    /// Per-key locks keyed on `<name>-<short-rev>`. Threads acquiring the
    /// same grammar version serialise on this; distinct grammars run in
    /// parallel.
    locks: KeyLocks,
}

impl SourceCache {
    /// Wrap an arbitrary base directory. Sources land at
    /// `<base>/<name>-<short-rev>/`. Useful for tests.
    pub fn new(base: PathBuf) -> Self {
        Self {
            base,
            locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// User-default cache rooted at `$XDG_CACHE_HOME/bonsai/grammars/`,
    /// falling back to `~/.cache/bonsai/grammars/` on every platform.
    /// macOS / Windows do *not* use their platform-native cache dirs —
    /// bonsai stores grammar source caches uniformly across platforms.
    ///
    /// Each cloned grammar lives under `<base>/<name>-<short-rev>/`. The
    /// compiled `<name>.{so|dylib|dll}` is built **in-place** inside the
    /// same dir (see [`GrammarCompiler`]) and then installed to the
    /// durable user-data layer (see [`GrammarLoader`]).
    pub fn user_default() -> Result<Self> {
        let p = xdg::cache_home()?.join("bonsai/grammars");
        Ok(Self::new(p))
    }

    /// Root directory of this cache. Created on first acquire.
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Path where the source tree for `(name, spec)` would live (whether or
    /// not it has been cloned yet).
    pub fn source_dir(&self, name: &str, spec: &LangSpec) -> PathBuf {
        self.base
            .join(format!("{name}-{}", short_rev(&spec.git_rev)))
    }

    /// Resolve `injections.scm` from the grammar source's own `queries/`
    /// directory. Grammar repos (e.g. MDeiml/tree-sitter-markdown) typically
    /// ship `queries/injections.scm` using the standard tree-sitter injection
    /// protocol (`@injection.language` + `@injection.content`).
    ///
    /// This intentionally reads from the **grammar source**, NOT the curated
    /// query repos (helix / nvim-treesitter): those files often use
    /// non-standard predicates (`#set-lang-from-info-string!`) that are
    /// nvim-specific and won't compile with stock tree-sitter.
    ///
    /// Returns `None` when the grammar does not ship `queries/injections.scm`
    /// — normal and not an error. Returns `Err` only on unexpected I/O.
    pub fn injections_path(&self, grammar_source_root: &Path) -> Result<Option<PathBuf>> {
        let injections_path = grammar_source_root.join("queries").join("injections.scm");
        match std::fs::metadata(&injections_path) {
            Ok(_) => Ok(Some(injections_path)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e)
                .with_context(|| format!("stat injections.scm at {}", injections_path.display())),
        }
    }

    /// Clone the grammar source if not already present. Returns the path to
    /// the (possibly nested via `subpath`) grammar directory ready for
    /// compilation.
    ///
    /// Thread-safe: concurrent calls for the same `(name, rev)` serialise on
    /// a per-key mutex so only one clone runs; later callers re-check
    /// `dest.exists()` and return the winner's result with no duplicate
    /// work. Calls for different grammars still run in parallel.
    pub fn acquire(&self, name: &str, spec: &LangSpec) -> Result<PathBuf> {
        let dest = self.source_dir(name, spec);
        if dest.exists() {
            return Ok(grammar_root(&dest, spec));
        }

        let key = format!("{name}-{}", short_rev(&spec.git_rev));
        let lock = key_lock(&self.locks, &key);
        let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);

        // Recheck after acquiring the per-key lock — another thread may
        // have completed the clone while we were waiting.
        if dest.exists() {
            return Ok(grammar_root(&dest, spec));
        }

        std::fs::create_dir_all(&self.base)
            .with_context(|| format!("create cache base {}", self.base.display()))?;

        let staging = self
            .base
            .join(format!("{name}-{}.tmp", short_rev(&spec.git_rev)));
        let _ = std::fs::remove_dir_all(&staging);

        match clone_into(&staging, &spec.git_url, &spec.git_rev) {
            Ok(()) => {}
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(e);
            }
        }

        match std::fs::rename(&staging, &dest) {
            Ok(()) => Ok(grammar_root(&dest, spec)),
            Err(_) if dest.exists() => {
                let _ = std::fs::remove_dir_all(&staging);
                Ok(grammar_root(&dest, spec))
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                Err(e)
                    .with_context(|| format!("rename {} -> {}", staging.display(), dest.display()))
            }
        }
    }
}

pub(crate) fn short_rev(rev: &str) -> &str {
    let take = rev.len().min(12);
    &rev[..take]
}

fn grammar_root(clone_dir: &Path, spec: &LangSpec) -> PathBuf {
    match &spec.subpath {
        Some(s) if !s.is_empty() => clone_dir.join(s),
        _ => clone_dir.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// QuerySourceCache — sparse clones of curated query repos
// ---------------------------------------------------------------------------

/// Sparse clones of the helix + nvim-treesitter query repos, shared across
/// all grammar installs. Clone once keyed by `<short-rev>`, reuse for every
/// language.
#[derive(Debug, Clone)]
pub struct QuerySourceCache {
    base: PathBuf,
    /// Per-key locks keyed on `<label>-<short-rev>`. Two grammar builds
    /// resolving queries from the same Helix / nvim-treesitter rev
    /// serialise here; distinct revs run in parallel.
    locks: KeyLocks,
}

impl QuerySourceCache {
    pub fn new(base: PathBuf) -> Self {
        Self {
            base,
            locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn user_default() -> Result<Self> {
        let p = xdg::cache_home()?.join("bonsai/query-sources");
        Ok(Self::new(p))
    }

    /// Ensure the sparse clone for `source` at `rev` is present. Returns the
    /// root of the sparse checkout (the repo root — subdirectories inside are
    /// accessed by callers with the right prefix).
    ///
    /// Thread-safe: concurrent calls for the same `(label, rev)` serialise on
    /// a per-key mutex so two grammar builds racing on the shared Helix /
    /// nvim-treesitter clone never collide on the staging dir or git
    /// working tree.
    pub fn acquire_source(&self, source: QuerySource, meta: &ManifestMeta) -> Result<PathBuf> {
        let (url, rev) = match source {
            QuerySource::Helix => (meta.helix_repo.as_str(), meta.helix_rev.as_str()),
            QuerySource::NvimTreesitter => (
                meta.nvim_treesitter_repo.as_str(),
                meta.nvim_treesitter_rev.as_str(),
            ),
        };
        let label = match source {
            QuerySource::Helix => "helix",
            QuerySource::NvimTreesitter => "nvim-treesitter",
        };
        let dest = self.base.join(format!("{label}-{}", short_rev(rev)));
        if dest.exists() {
            return Ok(dest);
        }

        let key = format!("{label}-{}", short_rev(rev));
        let lock = key_lock(&self.locks, &key);
        let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);

        // Recheck after acquiring the per-key lock — another thread may
        // have completed the clone while we were waiting.
        if dest.exists() {
            return Ok(dest);
        }

        std::fs::create_dir_all(&self.base)
            .with_context(|| format!("create query-source base {}", self.base.display()))?;

        let staging = self.base.join(format!("{label}-{}.tmp", short_rev(rev)));
        let _ = std::fs::remove_dir_all(&staging);

        let sparse_prefix = source.query_prefix();
        match sparse_clone_into(&staging, url, rev, sparse_prefix) {
            Ok(()) => {}
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(e);
            }
        }

        match std::fs::rename(&staging, &dest) {
            Ok(()) => Ok(dest),
            Err(_) if dest.exists() => {
                let _ = std::fs::remove_dir_all(&staging);
                Ok(dest)
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                Err(e)
                    .with_context(|| format!("rename {} -> {}", staging.display(), dest.display()))
            }
        }
    }

    /// Resolve a fully-expanded `highlights.scm` for `lang_name` from
    /// `source`. `; inherits: foo,bar` chains are expanded by concatenating
    /// ancestor content before descendant content, transitively. The result
    /// is written to a stable path inside the cache and returned.
    pub fn resolve_highlights(
        &self,
        source: QuerySource,
        meta: &ManifestMeta,
        lang_name: &str,
        query_subdir: Option<&str>,
    ) -> Result<PathBuf> {
        let repo_root = self.acquire_source(source, meta)?;
        let prefix = source.query_prefix();
        let subdir = query_subdir.unwrap_or(lang_name);
        let resolved_path = self.base.join(format!(
            "{}-{}-{lang_name}.resolved.scm",
            match source {
                QuerySource::Helix => "helix",
                QuerySource::NvimTreesitter => "nvim-treesitter",
            },
            short_rev(match source {
                QuerySource::Helix => meta.helix_rev.as_str(),
                QuerySource::NvimTreesitter => meta.nvim_treesitter_rev.as_str(),
            }),
        ));
        // Already resolved — reuse (idempotent).
        if resolved_path.exists() {
            return Ok(resolved_path);
        }

        let content = resolve_inherits(&repo_root, prefix, subdir, &mut vec![])?;

        let mut f = std::fs::File::create(&resolved_path)
            .with_context(|| format!("create resolved scm {}", resolved_path.display()))?;
        f.write_all(content.as_bytes())
            .with_context(|| format!("write resolved scm {}", resolved_path.display()))?;
        Ok(resolved_path)
    }
}

/// Recursively expand `; inherits: a,b,c` directives. `visited` guards
/// against cycles. Returns the fully concatenated query text.
fn resolve_inherits(
    repo_root: &Path,
    prefix: &str,
    lang: &str,
    visited: &mut Vec<String>,
) -> Result<String> {
    if visited.iter().any(|v| v == lang) {
        return Ok(String::new());
    }
    visited.push(lang.to_string());

    let scm_path = repo_root.join(prefix).join(lang).join("highlights.scm");
    if !scm_path.is_file() {
        bail!(
            "highlights.scm not found at {} for lang `{lang}`",
            scm_path.display()
        );
    }
    let raw = std::fs::read_to_string(&scm_path)
        .with_context(|| format!("read {}", scm_path.display()))?;

    // Collect `; inherits: foo,bar` or `; inherits: foo, bar` from first non-
    // empty lines (helix always puts it near the top, but scan all lines to be safe).
    let mut parents: Vec<String> = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("; inherits:")
            .or_else(|| trimmed.strip_prefix(";; inherits:"))
        {
            for part in rest.split(',') {
                // helix uses `_typescript` (underscore prefix = "private") and
                // `ecma`. Look them up as-is including the underscore because
                // that IS the directory name.
                let p_raw = part.trim();
                if !p_raw.is_empty() {
                    parents.push(p_raw.to_string());
                }
            }
        }
    }

    let mut out = String::new();
    for parent in &parents {
        // Try exact name first, then without leading underscore (private langs).
        let resolved = resolve_inherits(repo_root, prefix, parent, visited)
            .or_else(|_| {
                let stripped = parent.trim_start_matches('_');
                if stripped != parent {
                    resolve_inherits(repo_root, prefix, stripped, visited)
                } else {
                    bail!("no fallback for parent `{parent}`")
                }
            })
            .unwrap_or_default();
        if !resolved.is_empty() {
            out.push_str(&resolved);
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    out.push_str(&raw);
    Ok(out)
}

// ---------------------------------------------------------------------------
// git helpers
// ---------------------------------------------------------------------------

/// Sparse clone: init + enable sparse checkout + fetch single rev + checkout.
/// Only the `sparse_prefix` subtree is materialized on disk.
fn sparse_clone_into(dir: &Path, url: &str, rev: &str, sparse_prefix: &str) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create staging {}", dir.display()))?;

    run_git(dir, &["init", "--quiet"])?;
    run_git(dir, &["remote", "add", "origin", url])?;
    run_git(dir, &["sparse-checkout", "init", "--no-cone"])?;
    run_git(dir, &["sparse-checkout", "set", sparse_prefix])?;

    if run_git(dir, &["fetch", "--depth=1", "--quiet", "origin", rev]).is_err() {
        run_git(dir, &["fetch", "--quiet", "origin", rev])
            .with_context(|| format!("fetch {rev} from {url}"))?;
    }

    run_git(dir, &["checkout", "--quiet", "FETCH_HEAD"])
        .with_context(|| format!("checkout {rev}"))?;
    Ok(())
}

/// `git init` + add origin + fetch a single rev + checkout. Tries shallow
/// (`--depth=1`) first, falls back to a full fetch if the server refuses
/// fetching by SHA.
fn clone_into(dir: &Path, url: &str, rev: &str) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create staging {}", dir.display()))?;

    run_git(dir, &["init", "--quiet"])?;
    run_git(dir, &["remote", "add", "origin", url])?;

    if run_git(dir, &["fetch", "--depth=1", "--quiet", "origin", rev]).is_err() {
        run_git(dir, &["fetch", "--quiet", "origin", rev])
            .with_context(|| format!("fetch {rev} from {url}"))?;
    }

    run_git(dir, &["checkout", "--quiet", "FETCH_HEAD"])
        .with_context(|| format!("checkout {rev}"))?;
    Ok(())
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("spawn git {}", args.join(" ")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            cwd.display(),
            stderr.trim()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::manifest::QuerySource;

    fn dummy_spec(rev: &str, subpath: Option<&str>) -> LangSpec {
        LangSpec {
            git_url: "https://example/repo".into(),
            git_rev: rev.into(),
            subpath: subpath.map(String::from),
            extensions: vec!["x".into()],
            c_files: vec!["src/parser.c".into()],
            query_source: QuerySource::Helix,
            query_subdir: None,
            source: None,
        }
    }

    fn dummy_meta() -> ManifestMeta {
        ManifestMeta {
            helix_repo: "https://github.com/helix-editor/helix".into(),
            helix_rev: "aaaa0000bbbb1111cccc2222dddd3333eeee4444".into(),
            nvim_treesitter_repo: "https://github.com/nvim-treesitter/nvim-treesitter".into(),
            nvim_treesitter_rev: "ffff5555aaaa0000bbbb1111cccc2222dddd3333".into(),
        }
    }

    #[test]
    fn short_rev_truncates_to_12() {
        assert_eq!(short_rev("0123456789abcdef"), "0123456789ab");
        assert_eq!(short_rev("abc"), "abc");
    }

    #[test]
    fn source_dir_format_includes_short_rev() {
        let cache = SourceCache::new(PathBuf::from("/tmp/cache"));
        let spec = dummy_spec("0123456789abcdef00112233", None);
        assert_eq!(
            cache.source_dir("rust", &spec),
            PathBuf::from("/tmp/cache/rust-0123456789ab")
        );
    }

    #[test]
    fn grammar_root_honors_subpath() {
        let clone = PathBuf::from("/tmp/cache/typescript-deadbeef0000");
        let spec = dummy_spec("deadbeef00000000", Some("typescript"));
        assert_eq!(grammar_root(&clone, &spec), clone.join("typescript"));
    }

    #[test]
    fn grammar_root_no_subpath_returns_clone_dir() {
        let clone = PathBuf::from("/tmp/cache/rust-deadbeef0000");
        let spec = dummy_spec("deadbeef00000000", None);
        assert_eq!(grammar_root(&clone, &spec), clone);
    }

    #[test]
    fn inherits_chain_resolved_into_single_file() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        // Fake helix layout: runtime/queries/<lang>/highlights.scm
        let prefix = "runtime/queries";
        let ecma_dir = repo.join(prefix).join("ecma");
        let ts_dir = repo.join(prefix).join("typescript");
        std::fs::create_dir_all(&ecma_dir).unwrap();
        std::fs::create_dir_all(&ts_dir).unwrap();
        std::fs::write(ecma_dir.join("highlights.scm"), "(injection.foo)\n").unwrap();
        std::fs::write(
            ts_dir.join("highlights.scm"),
            "; inherits: ecma\n(typescript.bar)\n",
        )
        .unwrap();

        let mut visited = vec![];
        let result = resolve_inherits(&repo, prefix, "typescript", &mut visited).unwrap();
        assert!(
            result.contains("(injection.foo)"),
            "parent ecma content missing: {result}"
        );
        assert!(
            result.contains("(typescript.bar)"),
            "child typescript content missing: {result}"
        );
        // Parent must appear before child.
        let parent_pos = result.find("(injection.foo)").unwrap();
        let child_pos = result.find("(typescript.bar)").unwrap();
        assert!(parent_pos < child_pos, "parent must precede child");
    }

    #[test]
    fn query_source_helix_picks_helix_layout() {
        let tmp = tempfile::tempdir().unwrap();
        // Build a minimal fake helix sparse-clone layout.
        let cache_base = tmp.path().join("query-sources");
        let meta = dummy_meta();
        let label = format!("helix-{}", short_rev(&meta.helix_rev));
        let repo = cache_base.join(&label);
        let qs_dir = repo.join("runtime/queries/rust");
        std::fs::create_dir_all(&qs_dir).unwrap();
        std::fs::write(qs_dir.join("highlights.scm"), "(rust.id) @variable\n").unwrap();

        let qsc = QuerySourceCache::new(cache_base);
        // Pre-seed so acquire_source is skipped (no network in tests).
        let resolved = qsc
            .resolve_highlights(QuerySource::Helix, &meta, "rust", None)
            .unwrap();
        let content = std::fs::read_to_string(&resolved).unwrap();
        assert!(content.contains("(rust.id)"), "got: {content}");
    }

    #[test]
    fn query_source_nvim_used_when_helix_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_base = tmp.path().join("query-sources");
        let meta = dummy_meta();
        let label = format!("nvim-treesitter-{}", short_rev(&meta.nvim_treesitter_rev));
        let repo = cache_base.join(&label);
        let qs_dir = repo.join("queries/go");
        std::fs::create_dir_all(&qs_dir).unwrap();
        std::fs::write(qs_dir.join("highlights.scm"), "(go.func) @function\n").unwrap();

        let qsc = QuerySourceCache::new(cache_base);
        let resolved = qsc
            .resolve_highlights(QuerySource::NvimTreesitter, &meta, "go", None)
            .unwrap();
        let content = std::fs::read_to_string(&resolved).unwrap();
        assert!(content.contains("(go.func)"), "got: {content}");
    }

    /// Real network test against a tiny well-known repo. Kept `#[ignore]`d
    /// so plain `cargo test` stays offline; run with
    /// `cargo test -p hjkl-bonsai -- --ignored` for manual smoke-testing.
    #[test]
    #[ignore = "network: clones from github"]
    fn acquire_clones_real_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = SourceCache::new(tmp.path().to_path_buf());
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
        let root = cache.acquire("c", &spec).unwrap();
        assert!(root.join("src/parser.c").is_file());
        let root2 = cache.acquire("c", &spec).unwrap();
        assert_eq!(root, root2);
    }
}
