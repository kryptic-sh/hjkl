//! `cargo xtask build-grammars` — clone + compile every grammar in
//! `bonsai.toml` and install the result into a runtime layout the
//! `GrammarLoader`'s system/user lookup picks up directly.
//!
//! Distro maintainers run this once per release per architecture and ship
//! the resulting directory at `/usr/share/hjkl/runtime/grammars/`.
//!
//! Layout produced under `<out>/`:
//!   `<name>.{so|dylib|dll}` — compiled parser
//!   `<name>.scm`            — highlights query
//!
//! Sources are cloned through the user's runtime cache
//! (`$XDG_CACHE_HOME/hjkl/grammars/`) and the .so is built in-place inside
//! each clone. Re-running this command is incremental — already-cloned
//! sources and already-built .so files are reused.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use hjkl_bonsai::runtime::{GrammarCompiler, GrammarLoader, LangSpec, Manifest, SourceCache};

const MANIFEST: &str = include_str!("../../bonsai.toml");

pub fn run(args: &[String]) -> Result<()> {
    let opts = Options::parse(args)?;
    let manifest = Manifest::from_toml_str(MANIFEST).context("parse embedded bonsai.toml")?;

    std::fs::create_dir_all(&opts.out_dir)
        .with_context(|| format!("create out dir {}", opts.out_dir.display()))?;

    // The loader is doing the heavy lifting (acquire → compile → install).
    // We point its `user_dir` at the requested out dir, leave system_dirs
    // empty (so we always go through the install path), and reuse the
    // user's standard cache for source clones so re-runs are warm.
    let sources = SourceCache::user_default()?;
    let loader = GrammarLoader::new(
        Vec::new(),
        opts.out_dir.clone(),
        sources,
        GrammarCompiler::new(),
    );

    let total = manifest.iter().count();
    let mut built = 0usize;
    let mut already = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<(String, anyhow::Error)> = Vec::new();

    for (name, spec) in manifest.iter() {
        if !opts.want(name) {
            skipped += 1;
            continue;
        }
        match build_one(name, spec, &loader, &opts.out_dir) {
            Ok(BuildKind::Built) => {
                built += 1;
                println!("  built  {name}");
            }
            Ok(BuildKind::Cached) => {
                already += 1;
                println!("  cached {name}");
            }
            Err(e) => {
                failures.push((name.to_string(), e));
                println!("  FAIL   {name}");
            }
        }
    }

    println!();
    println!(
        "summary: of {total} → {built} built, {already} already installed, {skipped} skipped, {} failed",
        failures.len()
    );
    if !failures.is_empty() {
        println!();
        println!("failures:");
        for (name, e) in &failures {
            println!("  {name}: {e:#}");
        }
        bail!("{} grammar(s) failed", failures.len());
    }
    Ok(())
}

enum BuildKind {
    Built,
    Cached,
}

fn build_one(
    name: &str,
    spec: &LangSpec,
    loader: &GrammarLoader,
    out_dir: &Path,
) -> Result<BuildKind> {
    let already_installed = out_dir
        .join(format!("{name}{}", shared_lib_ext()))
        .is_file();
    loader
        .load(name, spec)
        .with_context(|| format!("install {name}"))?;
    Ok(if already_installed {
        BuildKind::Cached
    } else {
        BuildKind::Built
    })
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

#[derive(Debug, Default)]
struct Options {
    out_dir: PathBuf,
    only: Option<HashSet<String>>,
    exclude: HashSet<String>,
}

impl Options {
    fn want(&self, name: &str) -> bool {
        if self.exclude.contains(name) {
            return false;
        }
        match &self.only {
            Some(set) => set.contains(name),
            None => true,
        }
    }

    fn parse(args: &[String]) -> Result<Self> {
        let mut opts = Options {
            out_dir: PathBuf::from("build/grammars"),
            ..Default::default()
        };
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--out" | "-o" => {
                    let v = args
                        .get(i + 1)
                        .with_context(|| format!("{} requires a value", args[i]))?;
                    opts.out_dir = PathBuf::from(v);
                    i += 2;
                }
                "--only" => {
                    let v = args
                        .get(i + 1)
                        .with_context(|| format!("{} requires a value", args[i]))?;
                    opts.only = Some(v.split(',').map(|s| s.trim().to_string()).collect());
                    i += 2;
                }
                "--exclude" => {
                    let v = args
                        .get(i + 1)
                        .with_context(|| format!("{} requires a value", args[i]))?;
                    opts.exclude = v.split(',').map(|s| s.trim().to_string()).collect();
                    i += 2;
                }
                "-h" | "--help" => {
                    println!("{HELP}");
                    std::process::exit(0);
                }
                other => bail!("unknown argument: {other} (try --help)"),
            }
        }
        Ok(opts)
    }
}

const HELP: &str = "\
cargo xtask build-grammars [OPTIONS]

Compile every grammar in bonsai.toml and install into <out>/ in the
layout the GrammarLoader expects:

  <out>/<name>.{so,dylib,dll}
  <out>/<name>.scm

Distro maintainers ship the resulting dir at
/usr/share/hjkl/grammars/ (or /usr/local/share/hjkl/grammars/).

Source clones live in the user's runtime cache
($XDG_CACHE_HOME/hjkl/grammars/) and are reused across runs, so
re-invocations are incremental.

Options:
  -o, --out <dir>      Output directory (default: build/grammars/).
      --only <list>    Comma-separated allowlist of language names.
      --exclude <list> Comma-separated denylist of language names.
  -h, --help           Show this help.

Examples:
  cargo xtask build-grammars
  cargo xtask build-grammars --out /tmp/grammars --only rust,python,go
  cargo xtask build-grammars --exclude angular
";
