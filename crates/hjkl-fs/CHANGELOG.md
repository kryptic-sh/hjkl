# Changelog

All notable changes to `hjkl-fs` are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
