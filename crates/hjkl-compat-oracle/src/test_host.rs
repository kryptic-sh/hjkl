//! Headless [`Host`](hjkl_engine::Host) for oracle testing.
//!
//! Re-exports [`hjkl_engine::DefaultHost`] under the name `TestHost` so the
//! oracle drivers can refer to a stable, descriptive name without owning the
//! implementation. `DefaultHost` provides:
//!
//! - In-memory clipboard (no OS interaction).
//! - Wall-clock `now()` for timeout math.
//! - `prompt_search` → `None` (searches abort immediately).
//! - `emit_cursor_shape` recorded but not acted on.
//! - Viewport fixed at 80×24, no soft-wrap.
//! - `emit_intent` discards the unit intent.

/// No-op host for headless engine sessions. Thin alias over
/// [`hjkl_engine::DefaultHost`]; all behaviour is documented there.
pub type TestHost = hjkl_engine::DefaultHost;
