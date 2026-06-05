//! PTY harness sub-modules, included from the `e2e` test binary.

// macOS pty timing makes `at_colon_repeats_last_goto_line` see `:10\r` as
// literal Insert-mode text (other tests pass). The `@:` feature is fully
// covered by unit tests in `apps/hjkl/src/app/tests.rs`; restrict this e2e
// file to linux until the flake is root-caused.
#[cfg(all(unix, not(target_os = "macos")))]
pub mod at_colon;
pub mod explorer_search;
pub mod harness;
// Uses `:set`/`:w` ex commands; macOS pty timing mangles `:cmd\r` into literal
// insert text (see `at_colon` note above), so restrict to linux.
#[cfg(all(unix, not(target_os = "macos")))]
pub mod indent;
pub mod register_count;
pub mod render_sync;
