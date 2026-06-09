//! PTY harness sub-modules, included from the `e2e` test binary.

// macOS pty timing makes `at_colon_repeats_last_goto_line` see `:10\r` as
// literal Insert-mode text (other tests pass). The `@:` feature is fully
// covered by unit tests in `apps/hjkl/src/app/tests.rs`; restrict this e2e
// file to linux until the flake is root-caused.
#[cfg(all(unix, not(target_os = "macos")))]
pub mod at_colon;
// Event-driven autoreload (#242): writes a file externally and waits for the
// reload with no keypress. macOS tmpdir lives under a `/private` symlink that
// notify and canonicalize disagree on; restrict to linux like the other suites.
#[cfg(all(unix, not(target_os = "macos")))]
pub mod autoreload;
// Explorer e2e drives `<leader>e` + `/search<CR>` + `dd`/`p`; restrict to linux
// for the same macOS pty `:cmd\r`/`/pat\r` timing reasons as the other suites.
#[cfg(all(unix, not(target_os = "macos")))]
pub mod explorer;
pub mod harness;
// Uses `:set`/`:w` ex commands; macOS pty timing mangles `:cmd\r` into literal
// insert text (see `at_colon` note above), so restrict to linux.
#[cfg(all(unix, not(target_os = "macos")))]
pub mod indent;
pub mod register_count;
pub mod render_sync;
