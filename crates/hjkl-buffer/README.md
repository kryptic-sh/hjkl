# hjkl-buffer

Rope-backed text buffer with cursor, edits, motions, folds, viewport, and
search.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-buffer.svg)](https://crates.io/crates/hjkl-buffer)
[![docs.rs](https://img.shields.io/docsrs/hjkl-buffer)](https://docs.rs/hjkl-buffer)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Core text storage layer for the hjkl workspace. Provides vim-shaped buffer
semantics — charwise/linewise/blockwise selection, motions matching vim edge
cases (`h` no-wrap, `$` clamp, sticky col on `j`/`k`), folds, viewport, and
search. Extracted from
[sqeel-buffer](https://github.com/kryptic-sh/sqeel/tree/main/sqeel-buffer) with
full git history.

## Status

`0.2.0` — frozen public API; see [IMPLEMENTERS.md](IMPLEMENTERS.md) for the
14-method sealed surface.

## Features

- `ratatui` (optional, default off): re-exports `Style` and styled-span helpers
  wired through ratatui. Pull this in only when consuming from a ratatui
  frontend; otherwise the buffer is UI-agnostic.

## Usage

```toml
hjkl-buffer = "0.2"
```

```rust
use hjkl_buffer::{Buffer, Position};

let mut buf = Buffer::from_str("hello\nworld");
assert_eq!(buf.row_count(), 2);
assert_eq!(buf.line(0), Some("hello"));
assert_eq!(buf.cursor(), Position { row: 0, col: 0 });

// Move cursor to second row
buf.set_cursor(Position { row: 1, col: 0 });
assert_eq!(buf.cursor().row, 1);

// Replace all content
buf.replace_all("new content");
assert_eq!(buf.as_string(), "new content");
```

## License

MIT. See [LICENSE](../../LICENSE).
