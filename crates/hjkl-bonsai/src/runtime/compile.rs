//! Compile a tree-sitter grammar's C/C++ sources into a shared library.
//!
//! ⚠️ **Security:** this module runs the system C/C++ compiler over source
//! that the loader just cloned from a remote repository. Compiling untrusted
//! source is itself arbitrary-code-execution (compilers run `#pragma`s,
//! `#include`s, and — via any build tooling — can touch the filesystem and
//! network), and the artifact it produces is later `dlopen`ed and run
//! in-process. Only compile grammars whose source you trust; see the crate
//! root docs for the trust model.
//!
//! Honors `$CC` / `$CXX` if set, otherwise falls back to `cc` / `c++` on
//! `PATH`. MSVC targets instead find `cl.exe` through the `cc` crate's Visual
//! Studio discovery — it is not on `PATH` outside a developer prompt — which
//! also honors `$CC` / `$CXX`. The compiled `<name>.{so|dylib|dll}` is
//! written **in-place inside the source clone** (e.g.
//! `~/.cache/hjkl/grammars/<name>-<rev>/<name>.so`) — sources and their
//! built parser stay together so the cache dir is one self-contained
//! tree per grammar revision. The durable user-data install (the parser
//! that the loader actually picks up across runs) is the
//! [`GrammarLoader`]'s responsibility.
//!
//! Unix compilers are driven by hand. `cc-rs` is used only on MSVC targets,
//! for compiler discovery and its environment (`INCLUDE`, `LIB`, `PATH`); the
//! build-script inputs it normally reads (`TARGET`, `HOST`, `OPT_LEVEL`) are
//! set explicitly.

use std::path::{Path, PathBuf};
#[cfg(not(target_env = "msvc"))]
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::manifest::LangSpec;

/// Compiles a grammar's C/C++ sources to a shared library inside the
/// grammar's own source-clone directory. Stateless aside from the choice
/// of compiler binary (resolved via `$CC` / `$CXX`).
#[derive(Debug, Clone, Default)]
pub struct GrammarCompiler;

impl GrammarCompiler {
    pub fn new() -> Self {
        Self
    }

    /// Path where the compiled artifact for `(name, source_root)` would
    /// live (whether or not it has been built yet). Matches what
    /// [`Self::compile`] produces.
    pub fn artifact_path(&self, name: &str, source_root: &Path) -> PathBuf {
        source_root.join(format!("{name}{}", shared_lib_ext()))
    }

    /// Compile the grammar at `source_root` into a shared library at
    /// `<source_root>/<name>.<ext>`. Idempotent — returns the existing
    /// artifact path on a hit.
    ///
    /// ⚠️ **Security:** invokes the system C/C++ compiler on `source_root`,
    /// which for on-demand loads holds source freshly cloned from a remote
    /// repo. Compiling and (later) `dlopen`ing that output executes untrusted
    /// native code in-process. Only compile trusted grammar sources.
    pub fn compile(&self, name: &str, spec: &LangSpec, source_root: &Path) -> Result<PathBuf> {
        let dest = self.artifact_path(name, source_root);
        if dest.exists() {
            return Ok(dest);
        }

        // Unique per call (pid + counter): two threads compiling the same
        // grammar concurrently must not share a staging file, or one
        // thread's cleanup deletes the other's in-flight artifact.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let staging = source_root.join(format!(
            "{name}.tmp-{}-{n}{}",
            std::process::id(),
            shared_lib_ext(),
        ));
        let _ = std::fs::remove_file(&staging);

        match compile_into(spec, source_root, &staging) {
            Ok(()) => {}
            Err(e) => {
                let _ = std::fs::remove_file(&staging);
                return Err(e);
            }
        }

        super::publish::publish_path(&staging, &dest)
            .with_context(|| format!("rename {} -> {}", staging.display(), dest.display()))?;
        Ok(dest)
    }
}

fn compile_into(spec: &LangSpec, source_root: &Path, out_file: &Path) -> Result<()> {
    if spec.c_files.is_empty() {
        bail!("LangSpec has no c_files to compile");
    }

    // Resolve sources + classify C vs C++.
    let mut any_cpp = false;
    let mut sources: Vec<PathBuf> = Vec::with_capacity(spec.c_files.len());
    for f in &spec.c_files {
        let path = Path::new(f);
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            bail!("unsafe grammar source path: {f:?}");
        }
        let p = source_root.join(path);
        if !p.is_file() {
            bail!("missing source file: {}", p.display());
        }
        if matches!(
            p.extension().and_then(|s| s.to_str()),
            Some("cc" | "cpp" | "cxx" | "C")
        ) {
            any_cpp = true;
        }
        sources.push(p);
    }

    let include = source_root.join("src");
    run_compiler(any_cpp, &include, &sources, out_file)
}

/// gcc / clang: one driver invocation compiles and links the shared library.
#[cfg(not(target_env = "msvc"))]
fn run_compiler(cpp: bool, include: &Path, sources: &[PathBuf], out_file: &Path) -> Result<()> {
    let compiler = pick_compiler(cpp);
    let mut cmd = Command::new(&compiler);
    // Speed > size for parser code; -fPIC required for shared libs on ELF.
    cmd.arg("-O2").arg("-fPIC").arg("-I").arg(include);
    if cpp {
        cmd.arg("-std=c++14");
    } else {
        cmd.arg("-std=c11");
    }
    cmd.args(sources);
    cmd.arg("-shared").arg("-o").arg(out_file);

    let out = cmd
        .output()
        .with_context(|| format!("spawn compiler {compiler}"))?;
    check_compile_output(&out, out_file)
}

/// MSVC (`cl.exe`, or `clang-cl`): `-LD` compiles and links a DLL. Object
/// files go to a scratch directory beside the output — `cl.exe` otherwise
/// drops them in its working directory — and the linker skips the import
/// library and export file, which nothing loads.
#[cfg(target_env = "msvc")]
fn run_compiler(cpp: bool, include: &Path, sources: &[PathBuf], out_file: &Path) -> Result<()> {
    let tool = cc::Build::new()
        .cargo_metadata(false)
        .cargo_warnings(false)
        .emit_rerun_if_env_changed(false)
        .target(MSVC_TARGET)
        .host(MSVC_TARGET)
        .opt_level(2)
        .debug(false)
        .cpp(cpp)
        .try_get_compiler()
        .map_err(|e| {
            anyhow::anyhow!(
                "no MSVC C compiler found (install Visual Studio Build Tools with \
                 the C++ workload): {e}"
            )
        })?;
    if !tool.is_like_msvc() {
        bail!(
            "compiler {} is not MSVC-compatible; grammars on this target need \
             cl.exe or clang-cl",
            tool.path().display()
        );
    }

    let obj_dir = out_file.with_extension("obj.d");
    std::fs::create_dir_all(&obj_dir).with_context(|| format!("create {}", obj_dir.display()))?;

    let mut cmd = tool.to_command();
    cmd.arg("-utf-8").arg("-I").arg(include);
    if cpp {
        cmd.arg("-std:c++14");
    }
    cmd.args(sources);
    let mut fo = obj_dir.clone().into_os_string();
    fo.push("\\");
    cmd.arg({
        let mut arg = std::ffi::OsString::from("-Fo");
        arg.push(fo);
        arg
    });
    cmd.arg({
        let mut arg = std::ffi::OsString::from("-Fe");
        arg.push(out_file);
        arg
    });
    cmd.arg("-LD").arg("-link").arg("-NOIMPLIB").arg("-NOEXP");

    let out = cmd.output();
    let _ = std::fs::remove_dir_all(&obj_dir);
    let out = out.with_context(|| format!("spawn compiler {}", tool.path().display()))?;
    check_compile_output(&out, out_file)
}

/// The Rust target `cc` resolves the MSVC toolchain for — the one this binary
/// was built for. Nothing else supplies it outside a build script.
#[cfg(all(target_env = "msvc", target_arch = "x86_64"))]
const MSVC_TARGET: &str = "x86_64-pc-windows-msvc";
#[cfg(all(target_env = "msvc", target_arch = "aarch64"))]
const MSVC_TARGET: &str = "aarch64-pc-windows-msvc";
#[cfg(all(target_env = "msvc", target_arch = "x86"))]
const MSVC_TARGET: &str = "i686-pc-windows-msvc";

fn check_compile_output(out: &std::process::Output, out_file: &Path) -> Result<()> {
    if !out.status.success() {
        // cl.exe reports diagnostics on stdout, gcc/clang on stderr.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        bail!(
            "compile failed for {}: {}",
            out_file.display(),
            [stderr.trim(), stdout.trim()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    Ok(())
}

#[cfg(not(target_env = "msvc"))]
fn pick_compiler(cpp: bool) -> String {
    let env_key = if cpp { "CXX" } else { "CC" };
    if let Some(v) = std::env::var_os(env_key)
        && !v.is_empty()
    {
        return v.to_string_lossy().into_owned();
    }
    if cpp { "c++".into() } else { "cc".into() }
}

pub(super) fn shared_lib_ext() -> &'static str {
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
            query_source: super::super::manifest::QuerySource::Helix,
            query_subdir: None,
            source: None,
        }
    }

    #[test]
    fn artifact_path_lives_inside_source_root() {
        let c = GrammarCompiler::new();
        let root = PathBuf::from("/tmp/cache/rust-deadbeef");
        let p = c.artifact_path("rust", &root);
        assert_eq!(p, root.join(format!("rust{}", shared_lib_ext())));
    }

    #[test]
    fn shared_lib_ext_matches_platform() {
        let ext = shared_lib_ext();
        if cfg!(target_os = "linux") {
            assert_eq!(ext, ".so");
        } else if cfg!(target_os = "macos") {
            assert_eq!(ext, ".dylib");
        } else if cfg!(target_os = "windows") {
            assert_eq!(ext, ".dll");
        }
    }

    #[test]
    fn compile_errors_on_missing_source() {
        let tmp = tempfile::tempdir().unwrap();
        let c = GrammarCompiler::new();
        let spec = dummy_spec("deadbeef00000000");
        let bad_root = tmp.path().join("nonexistent");
        let err = c.compile("ghost", &spec, &bad_root).unwrap_err();
        assert!(err.to_string().contains("missing source"), "got: {err:#}");
    }

    /// Real compile against a tiny well-known grammar. `#[ignore]`d so plain
    /// `cargo test` stays offline. Run via:
    /// `cargo test -p hjkl-bonsai -- --ignored`
    #[test]
    #[ignore = "network + compiler: clones tree-sitter-c then builds it"]
    fn compile_real_grammar_end_to_end() {
        use super::super::source::SourceCache;

        let tmp = tempfile::tempdir().unwrap();
        let cache = SourceCache::new(tmp.path().to_path_buf());
        let compiler = GrammarCompiler::new();
        let spec = LangSpec {
            git_url: "https://github.com/tree-sitter/tree-sitter-c".into(),
            git_rev: "2a265d69a4caf57108a73ad2ed1e6922dd2f998c".into(),
            subpath: None,
            extensions: vec!["c".into()],
            c_files: vec!["src/parser.c".into()],
            query_source: super::super::manifest::QuerySource::Helix,
            query_subdir: None,
            source: None,
        };
        let root = cache.acquire("c", &spec).unwrap();
        let so = compiler.compile("c", &spec, &root).unwrap();
        assert!(so.is_file(), "expected artifact at {}", so.display());
        assert_eq!(so.parent().unwrap(), root);
        let meta = std::fs::metadata(&so).unwrap();
        assert!(meta.len() > 1024, "artifact suspiciously small");
        // The compiler's by-products (MSVC objects, import library, export
        // file) and the staging file must not be left beside the artifact.
        let so_name = so.file_name().unwrap().to_owned();
        let strays: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| {
                let n = n.to_string_lossy();
                *n != *so_name.to_string_lossy()
                    && (n.starts_with("c.") || n.ends_with(".obj") || n.ends_with(".exp"))
            })
            .collect();
        assert!(strays.is_empty(), "stray build products: {strays:?}");
        // The artifact must load and export the grammar's entry symbol — on
        // Windows that takes `dllexport`, which linking alone does not prove.
        let lib = unsafe { libloading::Library::new(&so) }.expect("compiled grammar must load");
        let entry: Result<libloading::Symbol<'_, unsafe extern "C" fn() -> *const ()>, _> =
            unsafe { lib.get(b"tree_sitter_c") };
        assert!(entry.is_ok(), "missing tree_sitter_c: {:?}", entry.err());

        // Second compile is idempotent.
        let so2 = compiler.compile("c", &spec, &root).unwrap();
        assert_eq!(so, so2);
    }
}
