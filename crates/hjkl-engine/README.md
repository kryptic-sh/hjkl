# hjkl-engine

Vim FSM, motion grammar, ex commands, and editor glue. Imported from
sqeel-vim with full git history; the engine/editor sub-split per the
[migration plan][plan] happens in-place during phase 5.

## Status

**Pre-1.0 churn.** API may change in patch bumps until 0.1.0. See
[SPEC.md](SPEC.md) for the planned 0.0.1 trait surface and stability
contract.

## Features

| Feature     | Default | Notes                                        |
| ----------- | ------- | -------------------------------------------- |
| `serde`     | yes     | Serde derives for `Editor` snapshot types.   |
| `ratatui`   | yes     | Re-export `hjkl-buffer/ratatui` for rendering. |
| `crossterm` | yes     | `From<crossterm::KeyEvent>` for the input enum. |

Disable defaults for embedded / wasm consumers:

```toml
hjkl-engine = { version = "=0.0.x", default-features = false }
```

## License

MIT

[plan]: https://github.com/kryptic-sh/hjkl/blob/main/MIGRATION.md
