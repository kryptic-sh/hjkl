# hjkl-editor

Front door for the hjkl modal editor stack. Re-exports the working parts of
[`hjkl-engine`](../hjkl-engine) and [`hjkl-buffer`](../hjkl-buffer) under a
curated namespace so consumers add one dependency instead of three.

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
