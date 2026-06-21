//! `hjkl-splash` — rendering-agnostic splash-screen animation.
//!
//! Emits pure [`SplashCell`] items via an iterator; consumers (TUI/GUI)
//! translate to their own rendering surface. The crate owns its time source —
//! [`Splash::cells`] takes `&self` and reads the wall clock internally, so
//! consumers cannot accidentally desynchronise the animation by skipping a
//! per-iteration `advance()` call (the v0.1 footgun).

use std::time::{Duration, Instant};

pub mod presets;
pub mod start_screen;

/// 24-bit RGB colour value.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Describes what role a cell plays in the current animation frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CellKind {
    /// Static art glyph — renderer should paint dim.
    Art,
    /// Trail cell — `age` is 0 (just-passed) up to `trail_len - 1` (oldest).
    Trail { age: u8 },
    /// Current cursor position — renderer should highlight.
    Cursor,
}

/// A single cell to be painted this tick.
#[derive(Copy, Clone, Debug)]
pub struct SplashCell {
    pub x: u16,
    pub y: u16,
    pub ch: char,
    pub kind: CellKind,
}

/// Bounding box of the art block within the terminal/canvas.
#[derive(Copy, Clone, Debug)]
pub struct Layout {
    pub origin_x: u16,
    pub origin_y: u16,
    pub rows: u16,
    pub cols: u16,
}

impl Layout {
    /// Center an `art_rows × art_cols` block within a viewport, leaving a
    /// little headroom for hint text below (matching the canonical hjkl
    /// placement: `(height - art_rows - 4) / 2`).
    pub fn centered(viewport_w: u16, viewport_h: u16, art_rows: u16, art_cols: u16) -> Self {
        let origin_y = viewport_h.saturating_sub(art_rows + 4) / 2;
        let origin_x = viewport_w.saturating_sub(art_cols) / 2;
        Self {
            origin_x,
            origin_y,
            rows: art_rows,
            cols: art_cols,
        }
    }
}

/// Default tick period — ~8 Hz (120 ms). Matches the canonical hjkl feel
/// (in v0.1 and earlier, manual `advance()` was driven by a 120 ms poll
/// timeout in the consumer's event loop, so this preserves that cadence by
/// default). Consumers wanting smoother motion can opt in via
/// [`Splash::with_period`].
pub const DEFAULT_PERIOD: Duration = Duration::from_millis(120);

/// Default trail length.
pub const DEFAULT_TRAIL_LEN: u8 = 6;

/// How the splash derives the current `tick`.
#[derive(Copy, Clone, Debug)]
enum TimeSource {
    /// Anchor + period; tick = (now - anchor) / period.
    Wall { anchor: Instant, period: Duration },
    /// Pinned tick value — used for deterministic tests.
    Fixed(u64),
}

/// The animation state machine.
///
/// Default time source is the wall clock at 30 Hz; consumers call
/// [`Splash::cells`] every redraw and the animation marches at the configured
/// period regardless of redraw rate. For deterministic frame stepping (snapshot
/// tests, recorded playback) construct via [`Splash::fixed_tick`] or pin an
/// existing splash with [`Splash::set_fixed_tick`].
pub struct Splash<'a> {
    art: &'a str,
    path: &'a [(u8, u8, char)],
    trail_len: u8,
    time: TimeSource,
}

impl<'a> Splash<'a> {
    /// Wall-clock-driven splash with the default ~8 Hz tick rate and 6-cell
    /// trail. The clock anchor is `Instant::now()`.
    pub fn new(art: &'a str, path: &'a [(u8, u8, char)]) -> Self {
        Self {
            art,
            path,
            trail_len: DEFAULT_TRAIL_LEN,
            time: TimeSource::Wall {
                anchor: Instant::now(),
                period: DEFAULT_PERIOD,
            },
        }
    }

    /// Deterministic splash pinned to a fixed `tick`. Useful for snapshot tests
    /// and recorded playback. The tick value is what [`Splash::cells`] sees.
    pub fn fixed_tick(art: &'a str, path: &'a [(u8, u8, char)], tick: u64) -> Self {
        Self {
            art,
            path,
            trail_len: DEFAULT_TRAIL_LEN,
            time: TimeSource::Fixed(tick),
        }
    }

    /// Override the trail length (default [`DEFAULT_TRAIL_LEN`]).
    pub fn with_trail_len(mut self, n: u8) -> Self {
        self.trail_len = n;
        self
    }

    /// Override the wall-clock tick period (default [`DEFAULT_PERIOD`]).
    /// No-op when the splash is in fixed-tick mode.
    pub fn with_period(mut self, period: Duration) -> Self {
        if let TimeSource::Wall { anchor, .. } = self.time {
            self.time = TimeSource::Wall { anchor, period };
        }
        self
    }

    /// Override the wall-clock anchor (default `Instant::now()` from
    /// [`Splash::new`]). No-op when the splash is in fixed-tick mode.
    ///
    /// Consumers that rebuild a transient `Splash` on every redraw MUST pass a
    /// persistent anchor here — otherwise re-anchoring to "now" each frame pins
    /// the tick at 0 and the animation never advances.
    pub fn with_anchor(mut self, anchor: Instant) -> Self {
        if let TimeSource::Wall { period, .. } = self.time {
            self.time = TimeSource::Wall { anchor, period };
        }
        self
    }

    /// Reset the wall-clock anchor to "now". No-op when fixed-tick.
    pub fn reset(&mut self) {
        if let TimeSource::Wall { period, .. } = self.time {
            self.time = TimeSource::Wall {
                anchor: Instant::now(),
                period,
            };
        }
    }

    /// Pin the splash to `tick`, switching it into fixed-tick mode.
    /// Subsequent calls to [`Splash::cells`] return frames for that tick.
    pub fn set_fixed_tick(&mut self, tick: u64) {
        self.time = TimeSource::Fixed(tick);
    }

    /// Current tick — derived from the wall clock or the pinned value.
    pub fn tick(&self) -> u64 {
        match self.time {
            TimeSource::Wall { anchor, period } => {
                let elapsed = Instant::now().saturating_duration_since(anchor);
                let period_nanos = period.as_nanos().max(1);
                (elapsed.as_nanos() / period_nanos) as u64
            }
            TimeSource::Fixed(t) => t,
        }
    }

    /// Current trail length.
    pub fn trail_len(&self) -> u8 {
        self.trail_len
    }

    /// Yield every cell to paint for the current frame. Idempotent within a
    /// tick window — calling it 1× or 100× per period produces the same cells.
    ///
    /// Order:
    /// 1. All art-glyph cells from `self.art` lines (`CellKind::Art`).
    /// 2. The trail (oldest → newest), then the cursor cell. Later iterations
    ///    overwrite earlier, so naive renderers can paint in iteration order.
    pub fn cells(&self, layout: Layout) -> impl Iterator<Item = SplashCell> + '_ {
        let tick = self.tick();
        let art_cells = self.art_cells(layout);
        let trail_cells = self.trail_cells(layout, tick);
        art_cells.chain(trail_cells)
    }

    fn art_cells(&self, layout: Layout) -> impl Iterator<Item = SplashCell> + '_ {
        self.art
            .lines()
            .take(layout.rows as usize)
            .enumerate()
            .flat_map(move |(row_idx, line)| {
                line.chars()
                    .enumerate()
                    .map(move |(col_idx, ch)| SplashCell {
                        x: layout.origin_x + col_idx as u16,
                        y: layout.origin_y + row_idx as u16,
                        ch,
                        kind: CellKind::Art,
                    })
            })
    }

    fn trail_cells(&self, layout: Layout, tick: u64) -> impl Iterator<Item = SplashCell> + '_ {
        let path_len = self.path.len();
        let trail_len = self.trail_len as usize;
        let cursor_idx = tick as usize % path_len;

        // oldest first (age = trail_len) → cursor last (age = 0)
        (0..=trail_len).rev().map(move |age| {
            let idx = if cursor_idx + path_len >= age {
                (cursor_idx + path_len - age) % path_len
            } else {
                0
            };
            let (row, col, ch) = self.path[idx];
            let kind = if age == 0 {
                CellKind::Cursor
            } else {
                CellKind::Trail {
                    age: (age - 1) as u8,
                }
            };
            SplashCell {
                x: layout.origin_x + col as u16,
                y: layout.origin_y + row as u16,
                ch,
                kind,
            }
        })
    }
}

/// Default ramp for trail age → [`Rgb`].
///
/// Age 0 is the brightest (just-passed); age ≥ 5 clamps to the dimmest.
pub fn default_trail_color(age: u8) -> Rgb {
    match age {
        0 => Rgb(0xe5, 0xe9, 0xf0), // near-white
        1 => Rgb(0xa0, 0xa8, 0xb8), // mid-bright
        2 => Rgb(0x60, 0x68, 0x78), // mid
        3 => Rgb(0x38, 0x40, 0x50), // dim
        4 => Rgb(0x20, 0x26, 0x32), // very dim
        _ => Rgb(0x10, 0x14, 0x1c), // barely visible
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_tick_pins_value() {
        let path: &[(u8, u8, char)] = &[(0, 0, 'a'), (0, 1, 'b'), (0, 2, 'c')];
        let splash = Splash::fixed_tick("abc", path, 7);
        assert_eq!(splash.tick(), 7);
    }

    #[test]
    fn with_anchor_drives_tick_from_given_instant() {
        // A persistent anchor in the past yields a non-zero tick immediately —
        // this is what consumers that rebuild `Splash` per frame must use.
        // Regression for the frozen-splash bug (#251): re-anchoring to `now`
        // every frame (the default `new`) keeps the tick pinned at ~0.
        let path: &[(u8, u8, char)] = &[(0, 0, 'a')];
        let anchor = Instant::now() - Duration::from_millis(600); // 5 periods @120ms
        let advanced = Splash::new("a", path).with_anchor(anchor).tick();
        assert!(
            advanced >= 4,
            "anchored-in-past tick should reflect elapsed periods, got {advanced}"
        );
        // The buggy pattern (fresh anchor each construction) stays at 0.
        let frozen = Splash::new("a", path).tick();
        assert_eq!(
            frozen, 0,
            "fresh-anchor tick must be 0 (the bug being fixed)"
        );
    }

    #[test]
    fn with_anchor_noop_on_fixed_tick() {
        let path: &[(u8, u8, char)] = &[(0, 0, 'a')];
        let anchor = Instant::now() - Duration::from_secs(10);
        let splash = Splash::fixed_tick("a", path, 3).with_anchor(anchor);
        assert_eq!(splash.tick(), 3, "fixed-tick must ignore with_anchor");
    }

    #[test]
    fn wall_clock_advances_with_period() {
        let path: &[(u8, u8, char)] = &[(0, 0, 'a')];
        let splash = Splash::new("a", path).with_period(Duration::from_millis(1));
        let t0 = splash.tick();
        std::thread::sleep(Duration::from_millis(5));
        let t1 = splash.tick();
        assert!(
            t1 >= t0 + 4,
            "expected at least 4 ticks elapsed, got {t0} -> {t1}"
        );
    }

    #[test]
    fn cells_idempotent_across_calls_without_advance() {
        // Wall-clock splash; same tick window should yield identical cells.
        let art = "abc";
        let path: &[(u8, u8, char)] = &[(0, 0, 'a'), (0, 1, 'b'), (0, 2, 'c')];
        let splash = Splash::new(art, path).with_period(Duration::from_secs(60));
        let layout = Layout {
            origin_x: 0,
            origin_y: 0,
            rows: 1,
            cols: 3,
        };
        let frame_a: Vec<_> = splash.cells(layout).collect();
        let frame_b: Vec<_> = splash.cells(layout).collect();
        assert_eq!(frame_a.len(), frame_b.len());
        for (a, b) in frame_a.iter().zip(frame_b.iter()) {
            assert_eq!(a.x, b.x);
            assert_eq!(a.y, b.y);
            assert_eq!(a.ch, b.ch);
            assert_eq!(a.kind, b.kind);
        }
    }

    #[test]
    fn splash_emits_art_then_trail_then_cursor() {
        let art = "abc";
        let path: &[(u8, u8, char)] = &[(0, 0, 'a'), (0, 1, 'b'), (0, 2, 'c')];
        // pin to tick 2 → cursor_idx = 2, trail covers path[1]
        let splash = Splash::fixed_tick(art, path, 2).with_trail_len(1);

        let layout = Layout {
            origin_x: 0,
            origin_y: 0,
            rows: 1,
            cols: 3,
        };
        let cells: Vec<_> = splash.cells(layout).collect();

        let art_cells: Vec<_> = cells.iter().filter(|c| c.kind == CellKind::Art).collect();
        assert_eq!(art_cells.len(), 3);

        let trail_cells: Vec<_> = cells
            .iter()
            .filter(|c| matches!(c.kind, CellKind::Trail { .. }))
            .collect();
        assert_eq!(trail_cells.len(), 1);
        assert_eq!(trail_cells[0].x, 1);
        assert_eq!(trail_cells[0].kind, CellKind::Trail { age: 0 });

        let cursor: Vec<_> = cells
            .iter()
            .filter(|c| c.kind == CellKind::Cursor)
            .collect();
        assert_eq!(cursor.len(), 1);
        assert_eq!(cursor[0].x, 2);
        assert_eq!(cursor[0].ch, 'c');
    }

    #[test]
    fn default_trail_color_clamps_at_high_age() {
        let age0 = default_trail_color(0);
        let age5 = default_trail_color(5);
        let age10 = default_trail_color(10);
        assert!(age0.0 > age5.0, "age0 red should be brighter than age5");
        assert_eq!(age5, age10);
    }

    #[test]
    fn layout_centers_art() {
        let layout = Layout::centered(40, 20, 5, 32);
        // origin_y = (20 - 5 - 4) / 2 = 5
        assert_eq!(layout.origin_y, 5);
        // origin_x = (40 - 32) / 2 = 4
        assert_eq!(layout.origin_x, 4);
    }
}
