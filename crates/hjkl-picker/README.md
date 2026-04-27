# hjkl-picker

Fuzzy picker subsystem for hjkl-based apps — file walk, grep search, and custom
sources.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-picker.svg)](https://crates.io/crates/hjkl-picker)
[![docs.rs](https://img.shields.io/docsrs/hjkl-picker)](https://docs.rs/hjkl-picker)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

The non-generic `Picker` harness accepts any `Box<dyn PickerLogic>`, so new
sources (file walk, ripgrep/grep/findstr, buffer list, …) plug in without
touching picker internals. Built-in sources cover gitignore-aware file walking
(`FileSource`) and multi-backend content search (`RgSource`). A subsequence
fuzzy scorer with word-boundary bonuses ranks candidates; `PreviewSpans` carries
per-row syntax-highlight data from the calling app's tree-sitter layer into the
renderer without creating a dependency on `hjkl-tree-sitter`.

The crate has no direct dependency on `hjkl-engine`, making it reusable across
`hjkl`, `sqeel`, `buffr`, and any other hjkl-family app.

## Status

`0.2.0` — stable public API. `PickerLogic` is the extension point for custom
sources; the trait is unlikely to gain mandatory methods without a major bump.

## Usage

```toml
hjkl-picker = "0.2"
```

```rust,no_run
use hjkl_picker::{Picker, FileSource};
use std::path::PathBuf;

let cwd = PathBuf::from(".");
let source = Box::new(FileSource::new(cwd));
let mut picker = Picker::new(source);
// Drive with `picker.handle_key(event)` from your event loop.
```

## What's here

- **`Picker`** — non-generic picker state; holds a `Box<dyn PickerLogic>` and a
  vim-modal query field powered by `hjkl-form::TextFieldEditor`.
- **`PickerLogic`** trait — implement to add a custom source; key methods:
  `title`, `item_count`, `label`, `match_text`, `preview`, `select`,
  `preview_top_row`, `preview_match_row`, `preview_line_offset`,
  `label_match_positions`, `enumerate`.
- **`PickerAction`** — enum of possible selection outcomes (`OpenPath`,
  `OpenPathAtLine`, `SwitchSlot`, `None`).
- **`PickerEvent`** — outcome of routing one key event (`None`, `Cancel`,
  `Select(PickerAction)`).
- **`RequeryMode`** — `FilterInMemory` (score existing vec) or `Spawn`
  (re-enumerate on every debounced query change).
- **`FileSource`** — gitignore-aware recursive file walker; `FilterInMemory`
  mode.
- **`RgSource`** — content search backed by ripgrep, with automatic fallback to
  `grep` (Unix) or `findstr` (Windows); `Spawn` mode.
- **`GrepBackend`** — enum (`Ripgrep`, `Grep`, `Findstr`) returned by
  `detect_grep_backend`.
- **`RgMatch`** — parsed grep result (path, line number, text).
- **`PreviewSpans`** — per-row syntax-highlight spans + style table passed from
  the app's tree-sitter layer to the renderer.
- **`score(needle, haystack) -> Option<(i64, Vec<usize>)>`** — subsequence fuzzy
  scorer with word-boundary bonuses; returns score + match positions.
- **`load_preview`** / **`build_preview_spans`** / **`PREVIEW_MAX_BYTES`** —
  utilities for loading and highlighting preview content.

## License

MIT. See [LICENSE](../../LICENSE).
