# hjkl-bonsai

Tree-sitter grammar registry + highlighter for the hjkl editor stack.

[![CI](https://github.com/kryptic-sh/hjkl-bonsai/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl-bonsai/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-bonsai.svg)](https://crates.io/crates/hjkl-bonsai)
[![docs.rs](https://img.shields.io/docsrs/hjkl-bonsai)](https://docs.rs/hjkl-bonsai)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

> Renamed from `hjkl-tree-sitter`. The previous name's git history is
> preserved here via the GitHub repo rename redirect. The
> `hjkl-tree-sitter` crate on crates.io stays as a deprecated artifact at
> 0.5.0; new releases ship under `hjkl-bonsai`.

Currently bundles 27 grammars (Rust, Markdown, JSON, TOML, SQL, Python,
JavaScript, TypeScript, TSX, Go, YAML, Bash, C, C++, C#, HTML, CSS, Java, PHP,
Ruby, Swift, Lua, Dart, R, Make, XML, Diff) and exposes a Helix-flavored
capture-name theming system compatible with Neovim capture-name conventions.
Language detection by file extension. Highlights are returned as a flat list
of `(byte_range, capture_name)` spans for renderers to style.

## Roadmap

`hjkl-bonsai` is pivoting away from bundled grammars to a Helix-style
**compile-on-demand** loader: when a grammar isn't found in the system or
user paths, fetch its source (pinned via a manifest), compile with `cc` on
the user's machine, cache to `~/.cache/hjkl/grammars/`. Distros can ship
pre-compiled `.so` files in `/usr/share/hjkl/runtime/grammars/` so end users
never need a compiler. The bundled-grammar approach (current) caps at ~30
languages without ballooning the binary; the runtime loader will scale to
the full Helix/nvim-treesitter list (~300).

## Status

27 bundled grammars, `DotFallbackTheme` for dark/light theming, incremental
re-parse via `tree-sitter::InputEdit`.

## Usage

```toml
hjkl-bonsai = "0.1"
```

```rust
use hjkl_bonsai::{DotFallbackTheme, Highlighter, LanguageRegistry};

let registry = LanguageRegistry::new();
let config = registry.by_name("rust").unwrap();
let mut highlighter = Highlighter::new(config).unwrap();
let spans = highlighter.highlight(b"fn main() {}");

let theme = DotFallbackTheme::dark();
for span in &spans {
    if let Some(style) = theme.style(span.capture()) {
        // apply style to span.byte_range() in your renderer
        let _ = style;
    }
}
```

## License

MIT. See [LICENSE](LICENSE).
