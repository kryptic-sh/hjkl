# hjkl-statusline

Renderer-agnostic statusline data model for hjkl editors. Pure data — no
ratatui, no floem. Companion crates wire it to a backend:

- [`hjkl-statusline-tui`](https://crates.io/crates/hjkl-statusline-tui) — ratatui
- `hjkl-statusline-gui` (future) — floem

The naming follows the vim convention (`:help statusline`, lualine,
vim-airline, lightline).

Scaffolding crate — model lands in a follow-up patch.

## License

MIT
