//! # hjkl-buffer
//!
//! **Pre-release placeholder.** This 0.0.0 release reserves the crate name on
//! crates.io. There is no public API yet.
//!
//! See the [migration plan][plan] for the full design and roadmap.
//!
//! [plan]: https://github.com/kryptic-sh/hjkl/blob/main/MIGRATION.md

// `unsafe_code` opt-out: rope perf may need unsafe later. Each unsafe block
// must carry a SAFETY comment and be covered by miri tests.
#![deny(unsafe_op_in_unsafe_fn)]
