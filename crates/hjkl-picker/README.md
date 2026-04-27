# hjkl-picker

Fuzzy picker subsystem for hjkl-based apps — file walk, grep search, and custom sources.

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

## License

MIT. See [LICENSE](../../LICENSE).
