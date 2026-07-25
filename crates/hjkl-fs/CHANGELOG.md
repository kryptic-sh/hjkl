# Changelog

All notable changes to `hjkl-fs` are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.37.0] - 2026-07-26

### Added

- `open`: `owner_only_options` / `owner_only_options_no_follow` — owner-only
  (`0600`) handles for append and streaming callers. The crate's three private
  `0600` sites now share one definition.
- `path`: `canonicalize_nearest` / `resolve_under` — confinement that an
  unresolved `..` cannot defeat, working on paths that do not exist yet. Dot
  segments are resolved even when the platform's parser classifies them as
  ordinary components, which is the case inside Windows verbatim (`\\?\`) paths.
- `dir`: `copy_dir_atomic` (staged, then swapped), `move_atomic` (rename, with a
  copy-then-delete fallback across filesystems that verifies before removing the
  source) and `remove_path_all` (unlinks a symlink rather than deleting through
  it).
- `identity`: `guard_not_swapped` — proves an open handle is still the object a
  path names, covering swaps `O_NOFOLLOW` cannot see — and `hardlink_count`.
  Windows reads both from one `GetFileInformationByHandle` call, which adds a
  Windows-only `windows-sys` dependency.

## [0.36.0] - 2026-07-26

### Added

- Initial release: the single seam for hjkl's disk I/O.
- `atomic` — temp → fsync → rename → fsync-parent writes, with `WriteOptions`
  presets for hjkl state (`0600`, always atomic) and user documents (mode
  preserved, non-atomic fallback permitted), plus `probe_writable_nofollow` for
  the `O_NOFOLLOW` write-permission check.
- `lock` — cross-process locking via `std::fs::File::lock` (Rust 1.89+) layered
  over an in-process path-keyed wait set, so concurrent hjkl instances _and_
  threads serialize. `with_lock_exclusive` spans a whole read-modify-write.
- `read` — capped reads (`read_capped`, `read_to_string_capped`) with presets
  for swap headers, undo history, buffer bodies, LSP messages and stdin.
- `dirs` — XDG paths threaded through `hjkl-xdg`, plus `ensure_private_dir` and
  the `private_cache_subdir` / `private_state_subdir` helpers.
