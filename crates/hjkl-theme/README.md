# hjkl-theme

Unified theme schema for the hjkl editor stack.

## Status

Phase 1: schema only. Parses TOML themes, interns palette refs, resolves
capture-name fallback chains. No rendering backend included yet.

See [kryptic-sh/hjkl#10](https://github.com/kryptic-sh/hjkl/issues/10) for the
full design and roadmap.

## Format

```toml
[palette]
blue = "#89b4fa"

"@function" = "$blue"
"@function.builtin" = { fg = "$blue", modifiers = ["bold"] }

[ui]
background = "#1e1e2e"
foreground = "#cdd6f4"
"statusline.inactive" = { fg = "#6c7086", bg = "#181825" }
```

## MSRV

Rust 1.95
