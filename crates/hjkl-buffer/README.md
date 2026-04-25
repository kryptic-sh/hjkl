# hjkl-buffer

Rope-backed text buffer with cursor, edits, motions, folds, viewport, and
search. Extracted from
[sqeel-buffer](https://github.com/kryptic-sh/sqeel/tree/main/sqeel-buffer) with
full history.

## Status

**Pre-1.0 churn.** API may change in patch bumps until 0.1.0. See
[MIGRATION.md](https://github.com/kryptic-sh/hjkl/blob/main/MIGRATION.md) for
the extraction roadmap and stability contract.

## Features

- `ratatui` (optional, default off): re-export `Style` and styled-span helpers
  wired through ratatui. Pull this in only when consuming from a ratatui
  frontend; otherwise the buffer is UI-agnostic.

## License

MIT
