# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `HjklOutcome::pinned_register` / `NvimOutcome::pinned_register` — the contents
  of the register a case names in `expected_register`, read from both engines so
  a case can pin `"a` and have it diffed. Previously only the unnamed register
  was ever read, so the four corpus cases pinning `"a` compared nothing.
- Added changelog.

### Changed

- **A corpus case may hold non-ASCII text.** `NvimOutcome::cursor` is a char
  column now, converted from the byte column nvim's API reports, and
  `nvim_driver` converts `initial_cursor` the other way before calling
  `set_cursor`. The oracle previously compared hjkl's char column straight
  against nvim's byte column — the comment in `diff.rs` said the two were
  "equivalent for ASCII-only cases", which confined every case to ASCII and left
  the wide-character column math with no oracle coverage at all. `tier1.toml`
  gains a wide-character group (`l`, `$`, `w`, `x`, `dw` and a `j` off a wide
  line onto an ASCII one).

  This is a semantic change to `OracleCase::initial_cursor` and
  `expected_cursor`: both count CHARS now. Every existing case is ASCII, where
  the two agree, so none needed editing.

### Fixed

- **A corpus case's `expected_cursor`, `expected_mode` and `expected_register`
  are now checked against nvim.** `diff.rs::run_single` sanity-checked only
  `expected_buffer` against nvim's outcome before diffing the two engines, so
  the other three fields could hold any value at all and the case still passed
  as long as hjkl and nvim agreed with each other — they documented intent
  without being able to fail. Turning the check on found thirteen mis-authored
  expectations, every one of which has been corrected to neovim 0.12.4's
  measured value (confirmed independently of the oracle's own driver):
  `incr_C-a`, `decr_C-x`, `mark_rbr_after_cw_typed_text`,
  `count_backspace_wraps_rows`, `count_3w_motion`,
  `register_yy_5p_pastes_five_times`, `register_yy_10p_pastes_ten_times`,
  `register_yy_3P_pastes_three_times_before`, `motion_F_find_backward`,
  `motion_T_till_backward`, `join_trailing_space`, `join_trailing_tab`, and
  `operator_Y_yank_line_shortcut` (whose register said `"hello\n"`, from vim's
  linewise `Y`, where nvim's `Y-default` mapping makes it charwise `y$`).
  `a_wrong_expected_cursor_fails_as_an_author_error` and its two siblings pin
  each new guard by authoring a field wrong on purpose.

[unreleased]: https://github.com/kryptic-sh/hjkl/compare/v0.40.0...HEAD
