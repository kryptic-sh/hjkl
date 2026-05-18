# hjkl

Vim-modal terminal editor: standalone TUI built on the hjkl engine.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl.svg)](https://crates.io/crates/hjkl)
[![docs.rs](https://img.shields.io/docsrs/hjkl)](https://docs.rs/hjkl)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

Native vim-modal editor. Single static binary, no plugins, no config files
required. Built on the [hjkl-engine](https://crates.io/crates/hjkl-engine) +
rope buffer.

## Install

```bash
cargo install hjkl
```

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

Or grab a pre-built tarball for any platform from the
[releases page](https://github.com/kryptic-sh/hjkl/releases).

## Status

`0.12.2` — full LSP client (5 phases: diagnostics, goto/hover, completion, code
actions/rename/format), window splits (`:sp`/`:vsp`, `Ctrl-w` nav, resize),
tabs, tmux-navigator handoff, mouse scroll, line numbers, multi-buffer editing,
fuzzy file/buffer/grep pickers with syntax-highlighted preview, tree-sitter
highlighting, smart indent, `.editorconfig`, and clipboard via
[`hjkl-clipboard`](https://crates.io/crates/hjkl-clipboard). See
[SCOPE.md](SCOPE.md) for the full feature roadmap.

## Usage

```bash
hjkl                  # empty buffer
hjkl file.txt         # open file
hjkl a.rs b.rs c.rs  # open multiple files
hjkl -R file.txt      # read-only
hjkl +42 file.txt     # jump to line 42
hjkl +/foo file.txt   # search for "foo" on open
hjkl +picker          # open fuzzy file picker immediately
hjkl +vsp file1 file2 # open two files in a vertical split
```

<!-- screenshot placeholder -->
<!-- ![hjkl screenshot](https://hjkl.kryptic.sh/screenshot.png) -->

## Headless mode

Run ex commands against files without a terminal. No ratatui, no crossterm —
suitable for CI scripts and automated code-mods.

```bash
# Substitute and write back
hjkl --headless +:%s/old/new/g +:wq src/foo.rs

# Same with -c flag (colon is optional)
hjkl --headless -c '%s/old/new/g' -c 'wq' src/foo.rs

# Loop over many files
for f in src/**/*.rs; do
  hjkl --headless +:%s/old/new/g +:wq "$f"
done
```

**Ordering**: all `-c CMD` flags run first (in flag order), then all `+cmd`
tokens (in argv order). The leading `:` is optional in both forms.

**No auto-write**: like vim, hjkl never writes implicitly. Include `:w`, `:wq`,
or `:x` in your command stream or the file is left unchanged.

**Exit codes**: `0` on success; `1` if any ex command returns an error or an I/O
failure occurs.

See [issue #26](https://github.com/kryptic-sh/hjkl/issues/26) for the
multi-phase roadmap (Phase 2: `--embed`, Phase 3: nvim-API msgpack-rpc).

### Embed mode (RPC)

`hjkl --embed` boots without a TUI and speaks JSON-RPC 2.0 over stdin/stdout
(newline-delimited, one request per line, one response per line). External code
can drive a live editor FSM — feed keystrokes, run ex commands, query buffer
state, cursor position, mode, and registers.

```bash
printf '{"jsonrpc":"2.0","method":"hjkl_input","params":["iHello"],"id":1}\n' \
  '{"jsonrpc":"2.0","method":"hjkl_get_buffer","params":[],"id":2}\n' \
  | hjkl --embed
```

See [`docs/embed-rpc.md`](../../docs/embed-rpc.md) for the full method
catalogue, error codes, and examples. Phase 2 of
[issue #26](https://github.com/kryptic-sh/hjkl/issues/26).

`hjkl --nvim-api` speaks the **msgpack-rpc** wire protocol with nvim-compatible
method names (`nvim_buf_set_lines`, `nvim_input`, `nvim_command`, etc.) so
existing `nvim-rs` clients can target hjkl unchanged. Phase 3 of
[issue #26](https://github.com/kryptic-sh/hjkl/issues/26).

## What works

- Normal / Insert / Visual / Command modes with full mode-indicator cursor shape
- All standard motions, operators, and text objects (free from the engine FSM)
- Status line: filename, mode, cursor position, dirty marker; `REC@r` badge
  while recording a macro; pending count + operator; search count `[n/m]`
- Cursor-line background (subtle blue-grey; suppressed during `:` / `/` prompts)
- `~` tilde markers on rows past end-of-buffer (vim `NonText` style)
- `:w` save, `:q` quit, `:wq` / `:x` write-quit, `:e` open file
- `:set` options, `:%s` search-and-replace with confirmation prompt
- `:!cmd` shell exec, `:r !cmd` / `:r file` read-into-buffer
- `:reg`, `:marks`, `:jumps`, `:changes` — output shown as a centered info popup
- `/` / `?` incremental search with match highlighting
- Undo / redo, marks, registers (shared across buffer slots)
- Terminal resize handled mid-frame
- Read-only guard (`-R` flag + engine-level mutation block)
- Jump to line (`+N`) and search-on-open (`+/pattern`)
- **`:set number` / `:set relativenumber`** line-number gutter. Aliases `nu` /
  `rnu` / `nonu` / `nornu`; combined `nu rnu` enables vim hybrid mode. Plus
  `:set numberwidth=N` for minimum gutter width.
- **Multi-buffer**: open many files; tab line at top when more than one buffer
  is open; switch with `:bn` / `:bp` / `:bd[!]` / `:bfirst` / `:blast` / `:b N`
  / `:b name` / `:ls` / `:buffers`; alt buffer (`Ctrl-^` / `:b#`); cycle with
  `Shift-H` / `Shift-L` and `gt` / `gT` / `]b` / `[b`; bulk save/quit with `:wa`
  / `:qa[!]` / `:wqa[!]`
- **Window splits** — `:sp` / `:vsp`, `Ctrl-w j/k/h/l/w/W` navigation, resize
  (`Ctrl-w +/-/>/<`/`=`/`_`/`|`), `:resize` / `:vertical resize`, `:only` /
  `Ctrl-w o`, `:new` / `:vnew`; per-window cursor + viewport state; 1-cell
  separator between panes
- **Tabs** — `:tabnew`, `gt` / `gT`, `:tabnext` / `:tabprev` / `:tabclose` /
  `:tabfirst` / `:tablast` / `:tabonly` / `:tabmove` / `:tabs`, `Ctrl-w T`
- **tmux-navigator handoff** — `Ctrl-h/j/k/l` in Normal mode move between hjkl
  windows; at an edge and `$TMUX` is set, falls through to `tmux select-pane`
- **Mouse wheel scroll** — wheel scrolls viewport with cursor clamped inside,
  respecting `scrolloff`. Toggle with `editor.mouse` config field or
  `:set mouse` / `:set nomouse` / `:set mouse!`
- **LSP** — per-language server lifecycle (bundled configs for rust-analyzer,
  pyright, typescript-language-server, clangd, gopls, lua-language-server):
  - Diagnostics: inline + signcolumn rendering, severity highlighting, `]d` /
    `[d` motions, `:LspInfo`
  - Goto: `gd` / `gD` / `gi` / `gy`, hover with `K`, references with `gr` /
    `:lreferences`
  - Completion: triggered + manual popup, kind icons, snippet expansion, async
    resolve
  - Code actions: `<leader>ca` / `:LspCodeAction`
  - Rename: `<leader>rn` / `:LspRename`
  - Format: `:LspFormat` / `:LspFormatRange` with workspace-edit application
  - Status-line spinner while LSP requests are in flight
- **Fuzzy file picker** (`<Space><Space>` / `<Space>f` / `:picker` /
  `hjkl +picker`) with syntax-highlighted preview
- **Buffer picker** (`<Space>b` / `:bpicker`)
- **Grep picker** (`<Space>/` / `:rg <pattern>`) — ripgrep-backed content search
  with grep / findstr fallback; preview jumps to and highlights the match line
- **Tree-sitter syntax highlighting** (Rust, Markdown, JSON, TOML, SQL bundled);
  grammar-load spinner in status line
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

- Plugins

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
- [`hjkl-editor-tui`](https://crates.io/crates/hjkl-editor-tui) — ratatui
  rendering adapters + shared spinner
- [`hjkl-lsp`](https://crates.io/crates/hjkl-lsp) — LSP client crate

See [docs.rs/hjkl-engine](https://docs.rs/hjkl-engine) for the engine trait
reference.

## Links

- Website: <https://hjkl.kryptic.sh>
- Repository: <https://github.com/kryptic-sh/hjkl>

## Documentation

[docs.rs/hjkl](https://docs.rs/hjkl)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
