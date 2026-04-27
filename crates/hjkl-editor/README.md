# hjkl-editor

Front-door facade for the hjkl modal editor stack — one dependency instead of
three.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-editor.svg)](https://crates.io/crates/hjkl-editor)
[![docs.rs](https://img.shields.io/docsrs/hjkl-editor)](https://docs.rs/hjkl-editor)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Re-exports the working parts of [`hjkl-engine`](../hjkl-engine) and
[`hjkl-buffer`](../hjkl-buffer) under a curated namespace so consumers (sqeel,
buffr, hjkl binary) add one dependency instead of three and don't need to know
the crate-split.

## Status

`0.2.0` — stable facade over engine + buffer. Two surfaces coexist during the
0.x churn: the legacy [`runtime`] module and the planned 0.1.0 SPEC [`spec`]
module.

## Modules

| Module    | Surface                                                                                                                                  |
| --------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `buffer`  | Re-export of `hjkl-buffer`: `Buffer`, `Edit`, `Position`, motion helpers, render path.                                                   |
| `runtime` | Legacy runtime surface — the working sqeel-vim port: `Editor`, `Input`, `Key`, `Registers`, `LspIntent`, `KeybindingMode`.               |
| `spec`    | Planned 0.1.0 trait surface (additive, per `hjkl-engine/SPEC.md`): `Pos`, `Selection`, `SelectionSet`, `Host`, `Options`, `EngineError`. |

## Usage

```toml
hjkl-editor = "0.2"
```

```rust,no_run
use hjkl_editor::buffer::Buffer;
use hjkl_editor::runtime::{DefaultHost, Editor, Options};

let mut editor = Editor::new(
    Buffer::new(),
    DefaultHost::new(),
    Options::default(),
);
editor.set_content("hello world");
```

For host integration (clipboard, intent fan-out, etc.) implement a type that
satisfies `hjkl_editor::spec::Host`. The trait extraction will rewire `Editor`
to take it as a generic; in the meantime the host-shape stays compatible by
name.

## License

MIT. See [LICENSE](../../LICENSE).
