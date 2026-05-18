# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Removed

- Dropped the `ratatui` feature and `start_screen::render`. The ratatui adapter
  now lives in the new `hjkl-splash-tui` crate. Part of #162.

## [0.2.0] - 2026-05-06

### Changed (breaking)

- **`Splash` now owns its time source.** [`Splash::new`] anchors a wall-clock
  origin (`Instant::now()`) and runs at ~8 Hz (120 ms period) by default to
  match the canonical hjkl feel; [`Splash::cells`] takes `&self` and reads the
  current tick from the clock internally. This removes a class of bugs in
  consumer event loops where animation ticks were driven by `event::poll`
  timeouts and could stall when high-frequency events (mouse motion) starved the
  timeout branch.
- **Removed `Splash::advance`.** Consumers no longer track frame timing — call
  [`Splash::cells`] every redraw and the animation marches at the configured
  period regardless of redraw rate.

### Added

- [`Splash::with_period`] — configurable tick period (default 33 ms = 30 Hz).
- [`Splash::fixed_tick`] / [`Splash::set_fixed_tick`] — deterministic
  tick-pinned mode for snapshot tests and recorded playback.
- [`Splash::reset`] — re-anchor the wall clock to "now".
- Public constants `DEFAULT_PERIOD` and `DEFAULT_TRAIL_LEN`.

### Migration from 0.1

```rust
// Before
let mut screen = Splash::new(art, path);
loop {
    render(&screen);
    screen.advance();      // forget this and animation freezes
}

// After
let screen = Splash::new(art, path);   // immutable
loop {
    render(&screen);                    // cells() reads the clock itself
}
```

For tests asserting on specific tick values, use
`Splash::fixed_tick(art, path, 7)`.

## [0.1.0] - 2026-05-06

### Added

- Initial extraction from `apps/hjkl/src/start_screen.rs`. Provides a
  rendering-agnostic `Splash` state machine that emits pure `SplashCell` items
  via an iterator. Includes `Layout::centered`, `default_trail_color`, and the
  `presets::hjkl` letterforms + path. Optional `ratatui` feature ships a
  `From<Rgb> for ratatui::style::Color` adapter.

[Unreleased]: https://github.com/kryptic-sh/hjkl-splash/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/kryptic-sh/hjkl-splash/releases/tag/v0.2.0
[0.1.0]: https://github.com/kryptic-sh/hjkl-splash/releases/tag/v0.1.0
