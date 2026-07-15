//! PTY harness sub-modules, included from the `e2e` test binary.
//!
//! The macOS "`:cmd\r` typed as literal Insert text" flake class that used
//! to gate most suites to linux is root-caused: a bare Esc and the byte
//! after it arriving in one pty `read()` decode as a single Alt+key (which
//! the app drops), and macOS ptys deliver a burst in one read far more
//! consistently than Linux. The harness now splits writes after every bare
//! Esc (see `TerminalSession::keys`), which fixed the class on sqeel's
//! identical harness and was validated by its macOS CI lane — so those
//! gates are lifted. If a suite still flakes on macOS, re-gate it with
//! `#[cfg(all(unix, not(target_os = "macos")))]` and note the failing test.

pub mod at_colon;
// Event-driven autoreload (#242): writes a file externally and waits for the
// reload with no keypress. macOS tmpdir lives under a `/private` symlink that
// notify and canonicalize disagree on — a DIFFERENT root cause from the pty
// Esc-coalescing flake, so this suite stays linux-only.
#[cfg(all(unix, not(target_os = "macos")))]
pub mod autoreload;
pub mod explorer;
pub mod global_marks;
pub mod harness;
pub mod indent;
pub mod paste;
pub mod register_count;
pub mod render_sync;
pub mod tabline_icons;
