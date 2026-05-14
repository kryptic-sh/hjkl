//! End-to-end render-sync regression suite.
//!
//! Spawns the `hjkl` binary under a real pty and scrapes the rendered screen
//! via `vt100` to catch the bug class where engine state moves but the user's
//! terminal display doesn't follow.
//!
//! Run with:
//!   cargo test -p hjkl --test e2e -- --test-threads=1
//!
//! The `--test-threads=1` flag is important: pty tests should run serially to
//! avoid contention over terminal resources and keep timing more predictable.
//!
//! Unix-only: ConPTY/portable-pty on Windows behaves differently enough that
//! the harness assertions don't hold (cursor reads return 0,0; rendered rows
//! don't carry the expected gutter format). Tracked separately; gating the
//! whole suite keeps Windows CI green.

#[cfg(unix)]
mod pty_harness;
