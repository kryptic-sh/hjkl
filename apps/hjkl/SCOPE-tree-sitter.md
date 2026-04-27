# hjkl — tree-sitter syntax highlighting design

Plan to extract sqeel's tree-sitter implementation into a new `hjkl-tree-sitter`
crate, refactor sqeel to consume it, and wire syntax highlighting into the
standalone `hjkl` binary.

Drafted 2026-04-27. Updated as phases land.

## Why

`sqeel-core/src/highlight.rs` has 1500+ LOC of working tree-sitter integration
(highlighter, parse-error harvesting, fold-block detection) plus a
`HighlightThread` worker in `sqeel-tui`. Most of that is generic — only the
`Dialect` post-processing is SQL-specific. Lifting the generic core to
`hjkl-tree-sitter` lets:

- The standalone `hjkl` binary highlight any vendored language.
- buffr / inbx / future hjkl consumers reuse the same path without re-rolling
  tree-sitter wiring.
- Sqeel keep its dialect-specific keyword promotion as a thin layer on top.

## Architecture

Steals selectively from Helix and Neovim:

**From Helix:**

- Grammars bundled as Rust deps (`tree-sitter-rust`, `tree-sitter-markdown`,
  etc.) — no runtime install, single static binary stays portable.
- `.scm` highlights queries shipped with each grammar's crate, embedded into
  `hjkl-tree-sitter` at compile time.
- Capture-name based theming (no fixed `TokenKind` enum chained through the
  engine).

**From Neovim:**

- Capture-name dot-fallback for theme resolution. `@keyword.return` → `@keyword`
  if the specific scope isn't themed.
- Decoration-provider mental model — syntax highlights, search matches, and
  diagnostics all express through the same per-frame "spans over a range"
  surface. Stops syntax highlights from being a special case.

**Skip from both (for v0):**

- ❌ Dynamic grammar install (Neovim's `:TSInstall`, Helix's
  `runtime/grammars/`). Vendor 5-10 grammars as Rust deps at compile time.
- ❌ Language injections (markdown code blocks, html `<style>` tags). Defer.
- ❌ User-extensible query files. Bundled queries only for v0.
- ❌ Indents / textobjects / locals queries. Engine FSM already covers vim-style
  text objects; revisit folds-via-TS later if needed.

## Crate layout

```
hjkl/
├── crates/
│   ├── hjkl-buffer/
│   ├── hjkl-engine/
│   ├── hjkl-editor/
│   ├── hjkl-ratatui/
│   └── hjkl-tree-sitter/   ← NEW
└── apps/
    └── hjkl/               ← consumes hjkl-tree-sitter
```

`hjkl-tree-sitter` provides:

- `Highlighter<L>` generic over `tree_sitter::Language` (or wrapper).
- `Syntax` per-buffer parse-tree state with incremental edit support.
- `HighlightSpan { range, capture_name }` — capture name as `&'static str`, not
  an enum.
- `Theme` trait — `fn style(&self, capture: &str) -> Option<Style>` with
  built-in dot-fallback resolver.
- `LanguageRegistry` — file extension / shebang → language config.
- (Optional) `HighlightThread` — moved from sqeel-tui, generic over language.

Sqeel keeps:

- `Dialect` enum + post-processing logic (`promote_keywords` over
  hjkl-tree-sitter spans).
- `statement_ranges` / `statement_at_byte` / `first_syntax_error` /
  `strip_sql_comments` / `is_show_create` — SQL-specific helpers consuming
  `hjkl_tree_sitter::Syntax::tree()`.

## Phasing

| Phase | Scope                                                                                                                                                                                                                         |
| ----- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **B** | Create `hjkl-tree-sitter` crate. Generic `Highlighter`, `Syntax`, `Theme`, `Registry`. Bundle 5 grammars: `rust`, `markdown`, `json`, `toml`, `sequel` (SQL). Default light + dark themes.                                    |
| **C** | Refactor sqeel to consume `hjkl-tree-sitter`. Drop `sqeel-core::highlight`'s generic bits, keep `Dialect` + SQL helpers. Update `sqeel-tui::highlight_thread` to use the generic worker (or move worker to hjkl-tree-sitter). |
| **D** | Wire syntax highlighting in the `hjkl` binary. File-extension dispatch via `Registry`. Render via ratatui spans with theme styles. Default light + dark themes selectable via `:set background={light,dark}`.                 |
| **E** | BCTP — hjkl 0.2.0 (new crate added to publish chain → bumps minor since binary now has a new feature; sqeel 0.2.0 (consumer migration is BREAKING for sqeel-core API).                                                        |

## Languages bundled in v0

| Language | Crate                 | Highlights |
| -------- | --------------------- | ---------- |
| Rust     | `tree-sitter-rust`    | bundled    |
| Markdown | `tree-sitter-md`      | bundled    |
| JSON     | `tree-sitter-json`    | bundled    |
| TOML     | `tree-sitter-toml-ng` | bundled    |
| SQL      | `tree-sitter-sequel`  | bundled    |

5 languages keeps binary size manageable (~5-10 MB extra). Add more in 0.2.x
patches (Python, JavaScript, C, Go, YAML) based on demand.

## Theme format

```toml
# hjkl/themes/default-dark.toml
"keyword"           = { fg = "#cc99cc", bold = true }
"keyword.control"   = { fg = "#ffaaaa", bold = true }
"string"            = { fg = "#a3be8c" }
"comment"           = { fg = "#858585", italic = true }
"function"          = { fg = "#88c0d0" }
"function.builtin"  = { fg = "#5e81ac" }
"type"              = { fg = "#ebcb8b" }
"variable"          = { fg = "#d8dee9" }
"number"            = { fg = "#b48ead" }
"punctuation"       = { fg = "#7c8696" }
```

Capture-name dot fallback: `function.builtin` looks up `function.builtin` →
`function` → `default` if nothing matches.

Themes shipped:

- `default-dark` (above)
- `default-light` (light mode)
- `none` (no highlighting; still parses for fold/structure if engine wants it).

User-supplied themes deferred to post-0.2.0 (config file support).

## Decisions

- **2026-04-27**: Vendored grammars (no runtime install). 5 languages in v0.
- **2026-04-27**: Capture-name based theming, dot-fallback for resolution.
- **2026-04-27**: New crate `hjkl-tree-sitter`. Sqeel consumes; doesn't re-roll.
- **2026-04-27**: Sync highlight in v0 (no worker thread). Worker thread is
  Phase 2 polish if rendering hitches show up. Sqeel currently uses worker
  thread; we may move that into `hjkl-tree-sitter::HighlightThread` during Phase
  C if it's cheap.
- **2026-04-27**: Themes shipped in TOML, embedded at compile time. User- loaded
  themes post-0.2.0.

## Risks / open

- **Binary size growth**. tree-sitter-rust ~700KB, tree-sitter-md ~600KB, others
  smaller. 5 grammars add ~3-4 MB compiled. Acceptable.
- **Incremental parsing complexity**. The buffer's edit operations need to
  surface enough info (byte offsets, old vs new ranges) to call `tree.edit()`.
  Verify `hjkl-buffer` exposes enough on `Edit` enum or via callbacks.
- **Capture name churn upstream**. Tree-sitter grammars sometimes rename
  captures between versions (`@string` → `@string.literal`). Pin grammar
  versions in Cargo.toml with `=`. Theme breakage on bump is the maintenance
  signal.
- **sqeel migration is BREAKING for sqeel-core**. The `Highlighter` /
  `TokenKind` / `HighlightSpan` types disappear from `sqeel_core::highlight`
  (they move to `hjkl_tree_sitter::*`). Sqeel-tui needs to follow. Consumers
  downstream of sqeel-core (none yet) would break. We're the only consumer so
  far — fine to break, but mark it BREAKING in commit + bump sqeel minor.
- **hjkl 0.2.0 vs 0.1.x**. Adding `hjkl-tree-sitter` to the workspace adds a new
  public crate to crates.io. Doesn't break the existing 4 lib crates' surfaces.
  Could land as 0.1.2 if conservative. Recommend 0.2.0 because the umbrella
  binary's behavior changes substantially (syntax highlighting appears).

## Future work (beyond v0 syntax highlighting)

- Worker thread (if sync render hitches)
- More languages (Python, JS, C, Go, YAML, etc.)
- Tree-sitter folds (replace heuristic folds with parse-tree-driven)
- Tree-sitter text objects (ip, ap-style on real syntax)
- Language injections
- User-supplied themes
- LSP integration tier (separate `hjkl-lsp` crate)

## Decisions log

- 2026-04-27: Architecture lifted from Helix (bundled) + Neovim (capture
  fallback).
- 2026-04-27: Phase B is the next dispatch.
