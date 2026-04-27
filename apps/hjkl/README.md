# hjkl

Vim-modal terminal editor. Standalone TUI built on the hjkl engine.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl.svg)](https://crates.io/crates/hjkl)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Native vim-modal editor. Single static binary, no plugins, no config files.
Built on the [hjkl-engine](https://crates.io/crates/hjkl-engine) + rope buffer.

## Install

```bash
cargo install hjkl
```

Or grab a pre-built binary from the
[releases page](https://github.com/kryptic-sh/hjkl/releases).

## Usage

```bash
hjkl                  # empty buffer
hjkl file.txt         # open file
hjkl a.rs b.rs c.rs  # open multiple files
hjkl -R file.txt      # read-only
hjkl +42 file.txt     # jump to line 42
hjkl +/foo file.txt   # search for "foo" on open
hjkl +picker          # open fuzzy file picker immediately
```

<!-- screenshot placeholder -->
<!-- ![hjkl screenshot](https://hjkl.kryptic.sh/screenshot.png) -->

## What works (v0)

- Normal / Insert / Visual / Command modes with full mode-indicator cursor shape
- All standard motions, operators, and text objects (free from the engine FSM)
- Status line: filename, mode, cursor position, dirty marker
- `:w` save, `:q` quit, `:wq` / `:x` write-quit, `:e` open file
- `:set` options, `:%s` search-and-replace with confirmation prompt
- `:!cmd` shell exec, `:r !cmd` / `:r file` read-into-buffer
- `/` / `?` incremental search with match highlighting
- Undo / redo, marks, registers
- Terminal resize handled mid-frame
- Read-only guard (`-R` flag + engine-level mutation block)
- Jump to line (`+N`) and search-on-open (`+/pattern`)
- **Multi-buffer**: open many files (`hjkl a.rs b.rs c.rs`); tab line at top
  when more than one buffer is open; switch with `:bn` / `:bp` / `:bd[!]` /
  `:bfirst` / `:blast` / `:b N` / `:b name` / `:ls` / `:buffers`; alt buffer
  (`Ctrl-^` / `:b#`); cycle with `Shift-H` / `Shift-L` and `gt` / `gT` / `]b` /
  `[b`; bulk save/quit with `:wa` / `:qa[!]` / `:wqa[!]`; helix-style `:q`
  closes the active slot when more than one buffer is open
- **Fuzzy file picker** (`<Space><Space>` / `<Space>f` / `:picker` /
  `hjkl +picker`) with syntax-highlighted preview
- **Buffer picker** (`<Space>b` / `:bpicker`)
- **Tree-sitter syntax highlighting** (Rust, Markdown, JSON, TOML, SQL bundled)
- **Per-buffer git diff signs** (`+` / `~` / `_` in the gutter) and tree-sitter
  diagnostic signs

## What's deferred

- Splits / multiple windows
- Plugins / config files
- LSP

## Related crates

- [`hjkl-buffer`](https://crates.io/crates/hjkl-buffer) — rope-based buffer
- [`hjkl-engine`](https://crates.io/crates/hjkl-engine) — modal-editing FSM
- [`hjkl-editor`](https://crates.io/crates/hjkl-editor) — ex commands, search,
  shell exec
- [`hjkl-ratatui`](https://crates.io/crates/hjkl-ratatui) — ratatui rendering
  adapters

See [SPEC.md](../../crates/hjkl-engine/SPEC.md) for the frozen 0.1.0 trait
surface.

## Links

- Website: <https://hjkl.kryptic.sh>
- Repository: <https://github.com/kryptic-sh/hjkl>

## License

MIT.
