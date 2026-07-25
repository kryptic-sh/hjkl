# hjkl-fs

The single seam for [hjkl](https://hjkl.kryptic.sh)'s disk I/O.

Every read and write hjkl performs goes through this crate — its own state (swap
files, undo history, cursor index, trash, config) and the user's documents
(`:w`, buffer loads). One seam means atomicity, locking, permissions and size
caps are decided once, instead of being re-derived — and occasionally forgotten —
at each call site.

## Modules

| Module   | Purpose                                                                                     |
| -------- | ------------------------------------------------------------------------------------------- |
| `dirs`   | XDG-rooted paths, threaded through `hjkl-xdg`; owner-only directory creation                 |
| `atomic` | temp → fsync → rename → fsync-parent writes; permission and fallback policy in `WriteOptions` |
| `lock`   | cross-process (`std::fs::File::lock`) **and** in-process locking                             |
| `read`   | reads bounded by an explicit cap                                                            |

## Writing

```rust
use hjkl_fs::{WriteOptions, write_atomic};
use std::path::Path;

// hjkl's own state: owner-only (0600), fully durable, never non-atomic.
write_atomic(Path::new("state.bin"), b"...", &WriteOptions::state())?;

// A user's document: keep its existing mode, and prefer a non-atomic write
// over refusing to save.
write_atomic(Path::new("doc.txt"), b"...", &WriteOptions::document())?;
# Ok::<(), std::io::Error>(())
```

Use `write_atomic_with` when the payload is not one contiguous buffer (a rope's
chunks, a length-prefixed record stream) so it is never materialized just to be
handed over.

## Locking

An atomic rename makes each individual write self-consistent. It does **not**
make read-modify-write atomic: two instances can both load, both mutate their
own copy, and the second rename silently discards the first's changes. Anything
with a shared on-disk record must hold the lock across the whole sequence.

```rust
use hjkl_fs::with_lock_exclusive;
use std::path::Path;

with_lock_exclusive(Path::new("index.bin"), || {
    // load, mutate and store all inside the lock
    Ok(())
})?;
# Ok::<(), std::io::Error>(())
```

Locks are taken on a sidecar `<path>.lock`, never on the target — the mere
existence of a swap or undofile is meaningful to the recovery paths, so creating
the target just to lock it would read as "there is a swap here".

Two layers, both required: `std::fs::File::lock` (`flock` on Unix, `LockFileEx`
on Windows) for other processes, and an in-process wait set because Unix `flock`
is per-open-file-description — two threads in one process would otherwise each
get their own description and not block each other.

## Paths

`hjkl-xdg` remains the one resolver; this crate re-exports it. The layout is
uniform on every platform, including macOS and Windows: `$XDG_*` when set to an
absolute path, else `~/.config`, `~/.local/share`, `~/.cache`, `~/.local/state`.
No `%APPDATA%`, no `~/Library/Application Support`.

## License

MIT
