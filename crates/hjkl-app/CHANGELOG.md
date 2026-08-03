# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.40.0...HEAD
