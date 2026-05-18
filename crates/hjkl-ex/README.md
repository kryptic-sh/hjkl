# hjkl-ex

Ex-command registry and dispatch layer for the hjkl editor stack.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-ex.svg)](https://crates.io/crates/hjkl-ex)
[![docs.rs](https://img.shields.io/docsrs/hjkl-ex)](https://docs.rs/hjkl-ex)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

Provides a typed `Registry` for ex commands, range parsing, Tab-completion, and
the full `:set` option table. Host applications register commands and call
`try_dispatch` to let hjkl-ex handle them; unrecognised commands fall back to
the host's own handling.

## What lives here

- `Registry<H>` / `HostRegistry<Ctx>` — extensible command registries with
  canonical names, aliases, and `ArgKind` metadata used by the completer.
- `try_dispatch` — resolves a command string against the editor registry and
  returns an `ExEffect` (save, quit, info, error, …), or `None` when no match.
- `complete` / `complete_arg` — Phase 6 Tab-completion engine. Resolves the
  command token, then dispatches to path, setting, buffer, register, or mark
  completion based on the command's declared `ArgKind`.
- `all_setting_names` — flat list of all `:set` option names and aliases,
  consumed by the host's completion `ArgSources`.
- `parse_range` — Vim-compatible line-range parser (`%`, `.`, `$`, `'a`, …).

## Usage

```toml
hjkl-ex = "0.1"
```

```rust,no_run
use hjkl_ex::{Registry, try_dispatch, all_setting_names, ArgSources, complete};

// Register commands (or use the built-in default_registry).
let reg = hjkl_ex::default_registry::<MyHost>();
let host_reg = hjkl_ex::HostRegistry::<()>::new();

// Dispatch a command string (without the leading ':').
if let Some(effect) = try_dispatch(&reg, &mut editor, "w") {
    // handle ExEffect::Save / ExEffect::Quit / …
}

// Tab-completion for the prompt.
let settings: Vec<String> = all_setting_names();
let sources = ArgSources {
    settings: &settings,
    ..Default::default()
};
let completions = complete("set nu", 6, &reg, &host_reg, &sources);
// completions.candidates == ["nu", "number", "numberwidth", …]
```

## Documentation

[docs.rs/hjkl-ex](https://docs.rs/hjkl-ex)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
