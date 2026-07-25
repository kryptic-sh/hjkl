# hjkl-fs

The single seam for [hjkl](https://hjkl.kryptic.sh)'s disk I/O.

Every read and write hjkl performs goes through this crate — its own state (swap
files, undo history, cursor index, trash, config) and the user's documents
(`:w`, buffer loads). One seam means atomicity, locking, permissions and size
caps are decided once, instead of being re-derived — and occasionally forgotten
— at each call site.

## Modules

| Module   | Purpose                                                                                       |
| -------- | --------------------------------------------------------------------------------------------- |
| `dirs`   | XDG-rooted paths, threaded through `hjkl-xdg`; owner-only directory creation                  |
| `atomic` | temp → fsync → rename → fsync-parent writes; permission and fallback policy in `WriteOptions` |
| `lock`   | cross-process (`std::fs::File::lock`) **and** in-process locking                              |
| `read`   | reads bounded by an explicit cap                                                              |
| `open`   | owner-only `OpenOptions` for callers that hold a handle and append                            |
| `path`   | normalization that cannot be fooled by an unresolved `..`, plus confinement                   |

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

Installers get the same footing instead of rolling their own staging-and-rename:
`copy_atomic` streams one file into another through that machinery and, like
`fs::copy`, carries the source's mode across; `symlink_atomic` stages a symlink
beside its destination and renames it over the old one, so replacing a link
never leaves a window with no link at all.

## Appending

A whole-file atomic write is the wrong shape for a caller that holds a
long-lived handle and appends per event — a log, an event transcript, an
incrementally written index. Those callers take the options instead of the
write, and get the same `0600` hjkl gives swap and undo files without
re-deriving `#[cfg(unix)] opts.mode(0o600)` at the call site:

```rust
use hjkl_fs::owner_only_options;

let log = owner_only_options().append(true).create(true).open("events.log")?;
# Ok::<(), std::io::Error>(())
```

`owner_only_options_no_follow` adds `O_NOFOLLOW` on Unix, so a symlinked final
component fails the open instead of redirecting the stream — the open _is_ the
symlink check. On Windows neither sets an explicit ACL; confidentiality comes
from the containing per-user directory, the same way `ensure_private_dir`
describes.

## Confinement

`root/nonexistent/../../etc/passwd` passes a `starts_with(root)` check and
escapes anyway, because the `..` has not been resolved yet. `fs::canonicalize`
resolves it but fails when the path does not exist — exactly the case for a file
about to be created. `canonicalize_nearest` canonicalizes the nearest existing
ancestor and resolves the remainder lexically against it, so a `ParentDir`
component never survives into the result, and `resolve_under` compares only
after that:

```rust
use hjkl_fs::resolve_under;
use std::path::Path;

let target = resolve_under(Path::new("/srv/data"), Path::new("notes/today.md"))?;
# Ok::<(), std::io::Error>(())
```

Which roots are allowed and what a violation means stay with the consumer; the
crate owns the mechanism, not the policy.

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
