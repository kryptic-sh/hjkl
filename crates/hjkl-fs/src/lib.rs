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
//!   [`atomic::copy_atomic`] and [`atomic::symlink_atomic`] put file copies and
//!   symlink installs on the same footing, so an installer never rolls its own
//!   staging-and-rename.
//! - [`dir`] — the same policy applied to a *tree*: [`dir::copy_dir_atomic`]
//!   stages a recursive copy beside its destination and swaps it in,
//!   [`dir::move_atomic`] renames and falls back to copy-then-delete across a
//!   filesystem boundary, and [`dir::remove_path_all`] removes a file, tree or
//!   symlink — unlinking a symlink rather than deleting through it.
//! - [`lock`] — cross-process (`std::fs::File::lock`) plus in-process locking, so
//!   two hjkl instances and two threads are equally safe.
//! - [`read`] — reads that are bounded by an explicit cap.
//! - [`open`] — owner-only [`std::fs::OpenOptions`] for callers that keep a
//!   handle and append, which a whole-file atomic write cannot serve.
//! - [`path`] — normalization that cannot be fooled by an unresolved `..`, and
//!   the confinement check built on it.
//! - [`identity`] — [`guard_not_swapped`] proves an open handle is still the
//!   object a path names, and [`hardlink_count`] reports how many names share
//!   that object.
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
//!
//! # Confining a path before writing it
//!
//! A path that came from outside the process is not inside a directory just
//! because it is spelled that way: `root/nonexistent/../../etc/passwd` passes a
//! `starts_with(root)` test and escapes anyway. [`resolve_under`] resolves before
//! it compares, so the check sees the real destination:
//!
//! ```no_run
//! use hjkl_fs::resolve_under;
//! use std::path::Path;
//!
//! // Errors rather than escaping, even though the file does not exist yet.
//! let target = resolve_under(Path::new("/srv/data"), Path::new("notes/today.md"))?;
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! Which roots are allowed, and what a violation means, stay with the caller —
//! this crate owns the mechanism, not the policy.
//!
//! # Checking a path is still the file you opened
//!
//! Confinement and the `O_NOFOLLOW` probe both inspect a *path*. Once a handle is
//! open the two can drift apart: the handle stays on what the path resolved to
//! then, the check inspects what it resolves to now. [`guard_not_swapped`]
//! compares the OS identity pair behind the handle against the one the path
//! resolves to, so a rename, a replacement or a substituted directory is caught
//! after the fact:
//!
//! ```no_run
//! use hjkl_fs::guard_not_swapped;
//! use std::fs::File;
//! use std::path::Path;
//!
//! let path = Path::new("/srv/data/notes.md");
//! let file = File::open(path)?;
//! // ... policy checks on `path` ...
//! guard_not_swapped(&file, path)?; // and they were about `file` after all
//! # Ok::<(), std::io::Error>(())
//! ```

pub mod atomic;
pub mod dir;
pub mod dirs;
pub mod identity;
pub mod lock;
pub mod open;
pub mod path;
pub mod read;

// The flat re-exports are the common vocabulary; the modules stay public for
// callers that want the full surface (cap presets, lock guards, options).
pub use atomic::{
    WriteOptions, copy_atomic, probe_writable_nofollow, symlink_atomic, write_atomic,
    write_atomic_with,
};
pub use dir::{copy_dir_atomic, move_atomic, remove_path_all};
pub use dirs::{ensure_private_dir, private_cache_subdir, private_state_subdir};
pub use identity::{guard_not_swapped, hardlink_count};
pub use lock::{FileLock, lock_path_for, with_lock_exclusive, with_lock_shared};
pub use open::{owner_only_options, owner_only_options_no_follow};
pub use path::{canonicalize_nearest, resolve_under};
pub use read::{
    read_capped, read_capped_from, read_to_string_capped, read_to_string_capped_from,
    read_to_string_unbounded,
};
