# hjkl

Vim-modal terminal editor: a standalone TUI built on the
[`hjkl-engine`](https://crates.io/crates/hjkl-engine) modal-editing core.
The umbrella crate ships the `hjkl` binary and wires the engine,
ratatui adapters, and a terminal `Host` impl into a single executable
you can drop into any shell.

## Install

```sh
cargo install hjkl
```

## Launch

```sh
hjkl <file>
```

See <https://hjkl.kryptic.sh> for documentation and design notes.
