//! The single seam for hjkl's disk I/O.
//!
//! Every read and write hjkl performs — its own state (swap, undo history,
//! cursor index, trash, config) and the user's documents (`:w`, buffer loads) —
//! goes through this crate. One seam means atomicity, locking, permissions and
//! size caps are decided once instead of being re-derived, and occasionally
//! forgotten, at each call site.
//!
//! # What it provides
//!
//! - [`dirs`] — XDG-rooted paths, threaded through `hjkl-xdg` (the one resolver),
//!   plus owner-only directory creation.
//! - [`atomic`] — temp → fsync → rename → fsync-parent writes, with the
//!   permission and fallback policy expressed as [`atomic::WriteOptions`].
//! - [`lock`] — cross-process (`std::fs::File::lock`) plus in-process locking, so
//!   two hjkl instances and two threads are equally safe.
//! - [`read`] — reads that are bounded by an explicit cap.
//!
//! # Choosing the write
//!
//! ```no_run
//! use hjkl_fs::atomic::{WriteOptions, write_atomic};
//! use std::path::Path;
//!
//! // hjkl's own state: owner-only (0600), fully durable, never non-atomic.
//! write_atomic(Path::new("/tmp/state.bin"), b"...", &WriteOptions::state())?;
//!
//! // A user's document: keep its mode, and prefer a non-atomic write over
//! // refusing to save.
//! write_atomic(Path::new("/tmp/doc.txt"), b"...", &WriteOptions::document())?;
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! # Read-modify-write needs the lock, not just the atomic write
//!
//! An atomic rename makes each individual write self-consistent. It does *not*
//! make load → mutate → store atomic: two instances can both load, both mutate
//! their own copy, and the second rename silently discards the first's changes.
//! Anything with a shared on-disk record must hold the lock across the whole
//! sequence:
//!
//! ```no_run
//! use hjkl_fs::lock::with_lock_exclusive;
//! use std::path::Path;
//!
//! let store = Path::new("/tmp/index.bin");
//! with_lock_exclusive(store, || {
//!     // load, mutate and store all inside the lock
//!     Ok(())
//! })?;
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! # Ordering: lock outside, write inside
//!
//! Take the lock first and let the atomic write happen under it. The reverse
//! cannot work — the lock guards a *sequence*, and a write that has already
//! renamed itself into place is not something a later lock can protect.

pub mod atomic;
pub mod dirs;
pub mod lock;
pub mod read;

// The flat re-exports are the common vocabulary; the modules stay public for
// callers that want the full surface (cap presets, lock guards, options).
pub use atomic::{WriteOptions, probe_writable_nofollow, write_atomic, write_atomic_with};
pub use dirs::{ensure_private_dir, private_cache_subdir, private_state_subdir};
pub use lock::{FileLock, lock_path_for, with_lock_exclusive, with_lock_shared};
pub use read::{read_capped, read_capped_from, read_to_string_capped, read_to_string_capped_from};
