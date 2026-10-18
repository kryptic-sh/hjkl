# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed (BREAKING)

- `git::explorer_key_for(path)` is now `explorer_key_for(path, root)`: the key
  is spelled like the explorer root it will be looked up under, the same way
  `explorer_status_map(root)` now spells its keys.

### Fixed

- `git::explorer_status_map` keys are spelled like `root` instead of like git2's
  workdir. On Windows a cwd with an 8.3 short-name component
  (`C:\Users\RUNNER~1`, common under `%TEMP%`) or in different letter case never
  matched the explorer's node paths, so every file showed no git status and a
  deleted tracked file vanished instead of staying listed as deleted.

### Changed

- The explorer, the fuzzy file picker, the fuzzy grep picker and `:grep` all
  select files through `hjkl_fs::project`, so what the tree shows is what the
  pickers find and what `:grep` searches. In practice that means dotfiles are
  now listed AND searchable (the picker skipped them, ripgrep skipped them),
  gitignored paths stay out of all four, and `.git` is excluded everywhere. The
  explorer's `H` and `I` keys stay as session overrides on that policy rather
  than a second copy of it. `:grep`'s `grep` fallback gains
  `--exclude-dir=.git`; it cannot read gitignore rules at all.
- `explorer.open` is a startup preference rather than a mirror of the live dock.
  Toggling the explorer (`<leader>e`, `<C-w>c` on it) no longer writes the key,
  so a toggle late in a session can no longer overwrite a value the user set
  deliberately. `:set explorer.open=true` / `=false` is now the only writer; it
  applies the value immediately and persists it, and `:set explorer.open?`
  reports it.

### Added

- Added changelog.

### Fixed

- Hunk stage, unstage and revert build the `git apply` patch with `/` in the
  file path. The path came from a native relative path, `src\main.rs` on
  Windows, which `git apply` takes literally, so any file in a subdirectory
  failed there.

[unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.40.0...HEAD
