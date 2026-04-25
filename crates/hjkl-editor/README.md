# hjkl-editor

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/hjkl-editor.svg)](https://crates.io/crates/hjkl-editor)
[![docs.rs](https://img.shields.io/docsrs/hjkl-editor)](https://docs.rs/hjkl-editor)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Front door for the hjkl modal editor stack. Re-exports the working parts of
[`hjkl-engine`](../hjkl-engine) and [`hjkl-buffer`](../hjkl-buffer) under a
curated namespace so consumers add one dependency instead of three.

Website: <https://hjkl.kryptic.sh>. Source:
<https://github.com/kryptic-sh/hjkl>.

## Modules

| Module    | Surface                                                                                                                                  |
| --------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `buffer`  | Re-export of `hjkl-buffer`: `Buffer`, `Edit`, `Position`, motion helpers, render path.                                                   |
| `runtime` | Legacy runtime surface — the working sqeel-vim port: `Editor`, `Input`, `Key`, `Registers`, `LspIntent`, `KeybindingMode`.               |
| `spec`    | Planned 0.1.0 trait surface (additive, per `hjkl-engine/SPEC.md`): `Pos`, `Selection`, `SelectionSet`, `Host`, `Options`, `EngineError`. |

## Usage

```rust,no_run
use hjkl_editor::runtime::{Editor, KeybindingMode};

let mut editor = Editor::new(KeybindingMode::Vim);
editor.set_content("hello world");
```

For host integration (clipboard, intent fan-out, etc.) write a
`BuffrHost`/`SqeelHost` shape that mirrors `hjkl_editor::spec::Host`. The trait
extraction proper will rewire `Editor` to take it as a generic; in the meantime
the host-shape stays compatible by name.

## Status

Pre-1.0 churn. API may change in patch bumps until 0.1.0.

## License

MIT
