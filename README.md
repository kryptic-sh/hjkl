# hjkl-bonsai

Tree-sitter syntax highlighting for the hjkl editor stack — runtime grammar
loading, no baked-in languages.

[![CI](https://github.com/kryptic-sh/hjkl-bonsai/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl-bonsai/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-bonsai.svg)](https://crates.io/crates/hjkl-bonsai)
[![docs.rs](https://img.shields.io/docsrs/hjkl-bonsai)](https://docs.rs/hjkl-bonsai)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

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

## License

MIT. See [LICENSE](LICENSE).
