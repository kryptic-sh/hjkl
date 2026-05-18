# hjkl-splash

Rendering-agnostic splash-screen animation for kryptic-sh projects.

[![CI](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-splash.svg)](https://crates.io/crates/hjkl-splash)
[![docs.rs](https://img.shields.io/docsrs/hjkl-splash)](https://docs.rs/hjkl-splash)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE)

Part of the [hjkl monorepo](https://github.com/kryptic-sh/hjkl) — a vim-modal
editor in Rust.

A small state machine that traces a cursor along an arbitrary path through an
ASCII-art block, leaving a fading trail behind it. The core is renderer-agnostic
— it emits pure `SplashCell` items via an iterator, so consumers (TUI with
ratatui, GUI with wgpu/canvas, web with HTML canvas) translate cells to their
own surface without inheriting a renderer dependency.

## Status

Pre-1.0. Public API is small and stable; expect additive changes (custom
trail-color ramps, configurable hint-row layout, multi-color presets) on minor
bumps. Breaking changes may land on minor bumps per Cargo SemVer for `0.x`.

## Usage

```toml
hjkl-splash = "0.1"
```

```rust
use hjkl_splash::{Splash, Layout, CellKind, presets};

let mut splash = Splash::new(presets::hjkl::ART, presets::hjkl::PATH);
let layout = Layout::centered(80, 24, presets::hjkl::ROWS, presets::hjkl::COLS);

for cell in splash.cells(layout) {
    match cell.kind {
        CellKind::Art => paint_dim(cell.x, cell.y, cell.ch),
        CellKind::Trail { age } => {
            let rgb = hjkl_splash::default_trail_color(age);
            paint(cell.x, cell.y, cell.ch, rgb);
        }
        CellKind::Cursor => paint_highlighted(cell.x, cell.y, cell.ch),
    }
}

splash.advance(); // call once per animation tick
```

### ratatui adapter

```toml
hjkl-splash = { version = "0.1", features = ["ratatui"] }
```

```rust,no_run
use hjkl_splash::{default_trail_color, Rgb};
use ratatui::style::Color;

let color: Color = default_trail_color(2).into();
```

## What's here

- **`Splash<'a>`** — state machine: holds borrowed `art` + `path`, a tick
  counter, and a configurable trail length.
- **`Splash::cells(layout)`** — iterator over every cell to paint this tick: art
  glyphs first, then trail (oldest → newest), then cursor.
- **`CellKind { Art, Trail { age }, Cursor }`** — what role a cell plays;
  consumers map this to their own styling.
- **`SplashCell { x, y, ch, kind }`** — pure cell descriptor, no rendering types
  in the signature.
- **`Layout`** — origin + extent of the art block within a viewport.
  `Layout::centered(w, h, rows, cols)` matches the canonical hjkl placement
  (centered horizontally, slight headroom for hint text below).
- **`Rgb(u8, u8, u8)`** — pure RGB triple. With `features = ["ratatui"]`, `Rgb`
  implements `From<Rgb> for ratatui::style::Color`.
- **`default_trail_color(age)`** — canonical greyscale fade ramp; consumers with
  their own theme can ignore this and provide custom mappings.
- **`presets::hjkl`** — bundles the HJKL letterforms and the cursor-path that
  traces the H, J, K, L strokes. Other projects pass their own `art` + `path`.

## Documentation

[docs.rs/hjkl-splash](https://docs.rs/hjkl-splash)

## Contributing

See the
[monorepo CONTRIBUTING guide](https://github.com/kryptic-sh/hjkl/blob/main/CONTRIBUTING.md).

## License

MIT — see [LICENSE](https://github.com/kryptic-sh/hjkl/blob/main/LICENSE).
