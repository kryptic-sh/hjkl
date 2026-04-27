# hjkl-tree-sitter

Generic tree-sitter syntax highlighting for the hjkl editor stack.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-tree-sitter.svg)](https://crates.io/crates/hjkl-tree-sitter)
[![docs.rs](https://img.shields.io/docsrs/hjkl-tree-sitter)](https://docs.rs/hjkl-tree-sitter)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Bundles 5 grammars (Rust, Markdown, JSON, TOML, SQL) and exposes a
Helix-flavored capture-name theming system compatible with Neovim capture-name
conventions. Language detection by file extension. Highlights are returned as a
flat list of `(byte_range, capture_name)` spans for renderers to style.

## Status

`0.2.0` — 5 bundled grammars, `DotFallbackTheme` for dark/light theming,
incremental re-parse via `tree-sitter::InputEdit`.

## Usage

```toml
hjkl-tree-sitter = "0.2"
```

```rust
use hjkl_tree_sitter::{DotFallbackTheme, Highlighter, LanguageRegistry};

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

MIT. See [LICENSE](../../LICENSE).
