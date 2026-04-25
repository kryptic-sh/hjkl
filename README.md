# hjkl

Vim engine, rope buffer, and modal editor primitives extracted from
[sqeel](https://github.com/kryptic-sh/sqeel) for reuse across sqeel,
[buffr](https://github.com/kryptic-sh/buffr), and a future standalone hjkl
binary.

## Status

**Pre-release.** 0.0.0 placeholders are published to crates.io to reserve names.
No public API yet. See [MIGRATION.md](MIGRATION.md) for the full extraction plan
and design rationale.

## Crates

| Crate          | Role                                                          |
| -------------- | ------------------------------------------------------------- |
| `hjkl-engine`  | Vim FSM + grammar, traits, no I/O deps. `no_std + alloc`.     |
| `hjkl-buffer`  | Rope-backed text buffer with cursor + edits + undo.           |
| `hjkl-editor`  | Glue: engine + buffer + registers + ex commands.              |
| `hjkl-ratatui` | Optional ratatui `Style` adapters and `KeyEvent` conversions. |

## License

MIT. See [LICENSE](LICENSE).
