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

mod pty_harness;
