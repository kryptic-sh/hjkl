# hjkl

Vim-modal terminal editor. Standalone TUI built on the hjkl engine.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl.svg)](https://crates.io/crates/hjkl)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

Native vim-modal editor. Single static binary, no plugins, no config files.
Built on the [hjkl-engine](https://crates.io/crates/hjkl-engine) + rope buffer.

## Status

`0.4.0` — multi-buffer editing, fuzzy file/buffer/grep pickers with
syntax-highlighted preview, tree-sitter highlighting + comment-marker overlay,
smart indent, `.editorconfig`, `softtabstop`, and clipboard via our in-house
[`hjkl-clipboard`](https://crates.io/crates/hjkl-clipboard) (sync + async, OSC
52 SSH fallback). See [SCOPE.md](SCOPE.md) for the full feature roadmap.

## Install

**macOS (Homebrew)**

```bash
brew install kryptic-sh/tap/hjkl
```

**Arch Linux (AUR)**

```bash
paru -S hjkl-bin
```

**Alpine Linux**

```bash
apk add --allow-untrusted hjkl-*.apk   # download .apk from releases page
```

**Debian / Ubuntu** — `.deb` for amd64 + arm64 on the
[releases page](https://github.com/kryptic-sh/hjkl/releases).

**Fedora / RHEL** — `.rpm` for x86_64 + aarch64 on the same page.

**From source**

```bash
cargo install hjkl
```

Or grab a pre-built tarball for any platform from the
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
- Status line: filename, mode, cursor position, dirty marker; `REC@r` badge
  while recording a macro; pending count + operator; search count `[n/m]`
- Cursor-line background (subtle blue-grey; suppressed during `:` / `/` prompts)
- `:w` save, `:q` quit, `:wq` / `:x` write-quit, `:e` open file
- `:set` options, `:%s` search-and-replace with confirmation prompt
- `:!cmd` shell exec, `:r !cmd` / `:r file` read-into-buffer
- `:reg`, `:marks`, `:jumps`, `:changes` — output shown as a centered info popup
- `/` / `?` incremental search with match highlighting
- Undo / redo, marks, registers (shared across buffer slots)
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
- **Grep picker** (`<Space>/` / `:rg <pattern>`) — ripgrep-backed content search
  with grep / findstr fallback; preview jumps to and highlights the match line
- **Tree-sitter syntax highlighting** (Rust, Markdown, JSON, TOML, SQL bundled)
- **Comment marker overlay** — `TODO` / `FIXME` / `FIX` / `NOTE` / `INFO` /
  `WARN` markers highlighted; consecutive single-line comments inherit the
  marker
- **Smart indent** — Enter / `o` / `O` auto-indent after `{` / `(` / `[`; close
  brace on a new line auto-dedents
- **`.editorconfig` support** — `indent_style`, `indent_size`, `tab_width`, and
  `max_line_length` applied on file open
- **Tab settings**: `tabstop`, `softtabstop`, `expandtab` (defaults: 4-space
  soft tabs); tabs render as visually aligned spaces; Backspace deletes a soft
  tab as a unit; `:set tabstop=N` updates rendering end-to-end
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
- [`hjkl-bonsai`](https://crates.io/crates/hjkl-bonsai) — bundled tree-sitter
  grammars + Neovim-flavoured highlight themes
- [`hjkl-clipboard`](https://crates.io/crates/hjkl-clipboard) — system clipboard
  adapter
- [`hjkl-form`](https://crates.io/crates/hjkl-form) — single-line form input
  built on the engine (used by pickers, `:` / `/` prompts)
- [`hjkl-picker`](https://crates.io/crates/hjkl-picker) — fuzzy picker subsystem
  (`Picker`, `PickerLogic`, `FileSource`, `RgSource`, scorer)
- [`hjkl-ratatui`](https://crates.io/crates/hjkl-ratatui) — ratatui rendering
  adapters + shared spinner

See [docs.rs/hjkl-engine](https://docs.rs/hjkl-engine) for the engine trait
reference.

## Links

- Website: <https://hjkl.kryptic.sh>
- Repository: <https://github.com/kryptic-sh/hjkl>

## License

MIT. See [LICENSE](../../LICENSE).
