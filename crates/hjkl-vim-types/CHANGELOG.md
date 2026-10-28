# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed (BREAKING)

- `Pending`, `Motion`, `LastChange` and `InsertEntry` are `#[non_exhaustive]`: a
  `match` over them outside this crate needs a wildcard arm. From here on a new
  variant on any of them is not a breaking change. Building with
  `--cfg hjkl_exhaustive` drops the attribute, which `hjkl`'s CI uses to keep
  `hjkl-vim`'s matches exhaustive.

### Added

- `Motion::ScreenLineStart`, `Motion::ScreenLineFirstNonBlank` and
  `Motion::ScreenLineEnd`, for `g0` / `g^` / `g$`. More variants on the
  exhaustive `Motion` enum: an external `match` over it breaks.
- `LastChange::OpenLine` carries the `count` of a `[count]o` / `[count]O`, so
  `.` repeats it; constructing or destructuring the variant must add the field.
- `InsertEntry::GShiftI`, for `gI` (insert at column 0) and its dot-repeat.
  Another variant on an exhaustive enum: an external `match` breaks.
- `Pending::OpMark { op, linewise }` and
  `LastChange::OpMark { op, ch, linewise, inserted, register }`, for an operator
  over a mark motion (`d'a`, ``d`a``) and its dot-repeat. Both enums are
  exhaustive, so an external `match` over either breaks.

- `Motion::ChangeWordEnd { big }` — the word-end motion `cw` / `cW` runs, which
  is not the same as `e` / `E`. Adding a variant to the non-`non_exhaustive`
  `Motion` enum breaks any external `match` over it.

### Changed

- Every `LastChange` variant that writes a register carries a
  `register: Option<char>` field, matching `LineOp`: `OpMotion`, `OpTextObj`,
  `DeleteToEol`, `Paste`, `CharDel`, `GnOp` and `VisualOp`. Dot-repeat needs it
  to replay a change into the register the original named (`:h redo-register`).

### Added

- Added changelog.

[unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.40.0...HEAD
