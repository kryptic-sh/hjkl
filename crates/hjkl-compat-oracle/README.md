# hjkl-compat-oracle

Headless neovim diff harness for vim-compatibility regression testing of
`hjkl-engine`. For background and design rationale see
[issue #23](https://github.com/kryptic-sh/hjkl/issues/23).

Each test case encodes an initial buffer state, a sequence of vim keystrokes (in
vim macro notation), and expected outcomes — buffer content, cursor position,
mode, and/or register contents. The oracle drives both `hjkl-engine` and a
headless `nvim --headless --embed` process with identical inputs, then diffs the
results to surface regressions before they ship. This crate is workspace-only
(`publish = false`).

## Running

Requires neovim on `$PATH`. On most distros:

```sh
# Arch
sudo pacman -S neovim

# Ubuntu / Debian
sudo apt-get install neovim
```

Then:

```sh
cargo test -p hjkl-compat-oracle
```

Or with full output (useful for the divergence report):

```sh
cargo test -p hjkl-compat-oracle --all-targets -- --nocapture
```

If neovim is absent all tests skip gracefully.

## Adding a corpus case

Cases live in TOML files under `corpus/`. Each entry maps to `OracleCase` in
`src/lib.rs`. The minimum required fields are `name`, `initial_buffer`, `keys`,
and `expected_buffer`. All others are optional.

```toml
[[cases]]
name = "motion_w_basic"
initial_buffer = "foo bar baz\n"
initial_cursor = [0, 0]   # (row, col), 0-based byte-col; default [0, 0]
keys = "w"
expected_buffer = "foo bar baz\n"
expected_cursor = [0, 4]  # optional
# expected_mode = "normal"          # optional: "normal" | "insert" | "visual" …
# expected_register = ['"', "foo "] # optional: [register_char, contents]
```

Keys use vim macro notation: `<Esc>`, `<C-r>`, `<CR>`, `dd`, `"ayy`, etc. Cursor
positions are 0-based `(row, col)` in bytes (ASCII-only buffers keep char-col ==
byte-col). Add new passing cases to `corpus/tier1.toml`. If a case exposes a
known engine bug, add it to `corpus/known_divergences.toml` instead (see below).

## Known divergences

These 8 cases in `corpus/known_divergences.toml` confirm engine bugs vs neovim
semantics. Tracked under
[issue #24](https://github.com/kryptic-sh/hjkl/issues/24). The cron CI job
reports progress without failing the build.

| Case                              | Description                                                            |
| --------------------------------- | ---------------------------------------------------------------------- |
| `motion_G_file_bottom`            | `G` lands on phantom row past the last line (no clamp)                 |
| `operator_dd_last_line`           | after `dd` on the last line, cursor stays past EOF instead of clamping |
| `operator_d_dollar_to_eol`        | `d$` leaves cursor one column past the new EOL (off-by-one)            |
| `textobj_da_doublequote`          | `da"` does not eat adjacent trailing whitespace per vim spec           |
| `textobj_daB_delete_around_brace` | cursor column off-by-one after `daB` on a multiline brace block        |
| `textobj_diB_delete_inner_brace`  | `diB` collapses interior newlines; nvim correctly preserves `{\n}\n`   |
| `count_5x_delete_chars`           | `x`/`X` does not write to the default register `"` (breaks `xp` swap)  |
| `undo_insert_then_u`              | cursor col not clamped to last valid column after undo of insert       |

## Neovim version

Not pinned. The cron CI installs whatever `neovim` package `ubuntu-24.04` ships
(currently nvim 0.10+). We accept any nvim version reachable via
`apt-get install neovim`; the oracle targets stable neovim semantics, not
nightly. If a case breaks on a specific nvim version, note it in the case
comment.
