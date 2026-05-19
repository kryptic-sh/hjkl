# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.25.1] - 2026-05-19

### Added

- Initial release. Extracted `handle_key` from `hjkl-vim` 0.25 (previously
  behind `hjkl-vim`'s `crossterm` feature gate, dropped as part of #162 phase
  3). `hjkl-engine` and `hjkl-vim` are now fully toolkit-agnostic; crossterm
  coupling lives here and in `hjkl-engine-tui`.

[0.25.1]: https://github.com/kryptic-sh/hjkl/releases/tag/v0.25.1
