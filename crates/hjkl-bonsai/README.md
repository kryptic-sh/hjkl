# hjkl-bonsai

Tree-sitter grammar registry + highlighter for the hjkl editor stack

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-bonsai.svg)](https://crates.io/crates/hjkl-bonsai)
[![docs.rs](https://img.shields.io/docsrs/hjkl-bonsai)](https://docs.rs/hjkl-bonsai)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

> Renamed from `hjkl-tree-sitter` (the old crate stays as a deprecated 0.5.0
> artifact on crates.io). New releases ship under `hjkl-bonsai`.

`bonsai` ships a manifest of **421 languages** (sourced from helix +
nvim-treesitter) and resolves grammars at runtime through a chain of:

1. **System** — distro-shipped `.so` under `/usr/share/bonsai/grammars/` (or
   `/usr/local/share/bonsai/grammars/`).
2. **User** — `.so` previously installed under `<user_data>/bonsai/grammars/`.
3. **On-demand** — clone the upstream repo, compile with `cc`/`c++`, and install
   the result into the user dir for next time.

A `<name>.rev` sidecar tracks `<git_rev>:abi<N>`, so a manifest bump or a
`tree-sitter` ABI change recompiles in place automatically.

The release rlib is **~720 KB** because no grammars are baked in. The first edit
of an unknown language pays a one-time clone+compile cost; everything after that
is a `dlopen`.

## ⚠️ Security: on-demand loading downloads and executes remote code

When a grammar is not already installed under a system or user directory,
resolution step 3 above happens **automatically, with the current user's
privileges**:

1. **Download** — bonsai shells out to `git` to clone the upstream grammar
   repository (and the curated helix / nvim-treesitter query repos) named in the
   manifest.
2. **Compile** — it runs the system C/C++ compiler (`$CC` / `$CXX`, else `cc` /
   `c++`) over that freshly-downloaded source.
3. **Load & run** — it `dlopen`s the resulting shared library and calls into it
   to parse your buffers.

Steps 2 and 3 both execute **arbitrary native code** from the downloaded source,
**in-process and unsandboxed**. A malicious or compromised grammar repo can run
anything the compiler or the loaded `.so` chooses to. This is inherent to the
tree-sitter grammar model — helix, neovim, and every other tree-sitter host
behave the same way.

The trust boundary is:

- the embedded **manifest** and the git remotes / revisions it pins,
- the transport security of `git` (use HTTPS or SSH remotes),
- the integrity of the local source and artifact caches.

**To avoid on-demand fetching + compilation entirely,** ship pre-built, vetted
`.so` + `.scm` pairs under a system directory (see
[Distro packagers](#distro-packagers)). System-dir grammars are loaded as-is and
never trigger a clone or compile. Callers that must never fetch untrusted code
should resolve only via `GrammarLoader::lookup_only` and treat a miss as "no
highlighting" rather than falling back to the build path.

## API

`GrammarLoader::load` is synchronous and suitable for blocking contexts (xtask,
CLI one-shots, tests). For TUI/GUI event loops that cannot block 1–3 s on a
first-ever clone+compile, wrap the loader in `AsyncGrammarLoader` from
`runtime::async_loader` — it dispatches to a 2-worker pool and automatically
deduplicates concurrent requests for the same grammar name.

## Usage

```toml
hjkl-bonsai = "0.3"
```

```rust
use std::sync::Arc;
use hjkl_bonsai::{DotFallbackTheme, Highlighter, Theme};
use hjkl_bonsai::runtime::{Grammar, GrammarLoader, GrammarRegistry};

// 1. Load the embedded manifest + standard XDG-everywhere loader.
let registry = GrammarRegistry::embedded()?;
let loader = GrammarLoader::user_default()?;

// 2. Resolve a language by name (or path → registry.name_for_path).
let spec = registry.by_name("rust").expect("manifest entry");
let grammar = Arc::new(Grammar::load("rust", spec, &loader)?);

// 3. Highlight.
let mut highlighter = Highlighter::new(grammar)?;
let spans = highlighter.highlight(b"fn main() {}");

let theme = DotFallbackTheme::dark();
for span in &spans {
    if let Some(_style) = theme.style(span.capture()) {
        // apply style to span.byte_range() in your renderer
    }
}
# Ok::<(), anyhow::Error>(())
```

## Distro packagers

The runtime layout `bonsai` looks for is:

```text
<prefix>/bonsai/grammars/<name>.so
<prefix>/bonsai/grammars/<name>.scm
```

To pre-build the full set against the current manifest:

```sh
cargo xtask build-grammars --out /tmp/grammars
install -Dm644 /tmp/grammars/*.so -t \
    "$pkgdir/usr/share/bonsai/grammars/"
install -Dm644 /tmp/grammars/*.scm -t \
    "$pkgdir/usr/share/bonsai/grammars/"
```

System-shipped grammars are not rev-checked at runtime — the packager owns that
lifecycle.

## On-disk layout

```text
/usr/share/bonsai/grammars/         # 1st lookup (system)
/usr/local/share/bonsai/grammars/   # 2nd lookup (system)
<user_data>/bonsai/grammars/        # 3rd lookup + install target
  rust.so   rust.scm   rust.rev
  python.so python.scm python.rev
<user_cache>/bonsai/grammars/       # source clones, transient
  rust-e86119bdb496/
  python-710796b8b877/
```

`<user_data>` and `<user_cache>` follow XDG-everywhere — same paths on every
platform:

| Variable          | Default          |
| ----------------- | ---------------- |
| `$XDG_DATA_HOME`  | `~/.local/share` |
| `$XDG_CACHE_HOME` | `~/.cache`       |

macOS and Windows do **not** get their platform-native dirs
(`~/Library/Application Support`, `%APPDATA%`). bonsai stores its grammar cache
uniformly across platforms so a `~/.local/share/bonsai/` checkout looks
identical everywhere. The resolver is self-contained — no `hjkl-config` or other
umbrella deps.

## Documentation

[docs.rs/hjkl-bonsai](https://docs.rs/hjkl-bonsai)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
