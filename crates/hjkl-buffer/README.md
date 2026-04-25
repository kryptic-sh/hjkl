# hjkl-buffer

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/hjkl-buffer.svg)](https://crates.io/crates/hjkl-buffer)
[![docs.rs](https://img.shields.io/docsrs/hjkl-buffer)](https://docs.rs/hjkl-buffer)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Rope-backed text buffer with cursor, edits, motions, folds, viewport, and
search. Extracted from
[sqeel-buffer](https://github.com/kryptic-sh/sqeel/tree/main/sqeel-buffer) with
full history.

Website: <https://hjkl.kryptic.sh>. Source:
<https://github.com/kryptic-sh/hjkl>.

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
