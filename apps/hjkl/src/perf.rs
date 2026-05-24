//! Runtime perf-log toggle.
//!
//! Every hot path emits `tracing::debug!(target: "hjkl::profile", …)`.
//! By default the env filter (`info`) drops those events on the floor.
//! `:perf` flips the filter at runtime via a [`reload::Handle`] held in
//! [`FILTER_HANDLE`] so logging starts/stops without restarting hjkl.
//!
//! - **On**: `info,hjkl::profile=debug` — every recorded path lands in
//!   `~/.local/share/hjkl/logs/hjkl.log`.
//! - **Off**: whatever filter the user originally booted with (env or `info`).

use std::sync::OnceLock;
use tracing_subscriber::{EnvFilter, Registry, reload::Handle};

/// Reload handle stashed by `init_tracing` in `main.rs`. `None` when
/// tracing was never initialised (test paths, headless mode).
pub static FILTER_HANDLE: OnceLock<Handle<EnvFilter, Registry>> = OnceLock::new();

/// Install the reload handle. Called once from `main.rs::init_tracing`
/// after the subscriber is built.
pub fn install_filter_handle(handle: Handle<EnvFilter, Registry>) {
    let _ = FILTER_HANDLE.set(handle);
}

/// Try to set the active filter directive (e.g. `"info,hjkl::profile=debug"`).
/// No-op when the handle was never installed (tests, headless).
pub fn try_set_filter(directive: &str) -> Result<(), String> {
    let Some(handle) = FILTER_HANDLE.get() else {
        return Ok(());
    };
    let filter = EnvFilter::try_new(directive).map_err(|e| e.to_string())?;
    handle.reload(filter).map_err(|e| e.to_string())
}
