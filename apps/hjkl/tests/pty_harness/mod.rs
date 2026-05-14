//! PTY harness sub-modules, included from the `e2e` test binary.

// macOS pty timing makes `at_colon_repeats_last_goto_line` see `:10\r` as
// literal Insert-mode text (other tests pass). The `@:` feature is fully
// covered by unit tests in `apps/hjkl/src/app/tests.rs`; restrict this e2e
// file to linux until the flake is root-caused.
#[cfg(all(unix, not(target_os = "macos")))]
pub mod at_colon;
pub mod harness;
pub mod register_count;
pub mod render_sync;
