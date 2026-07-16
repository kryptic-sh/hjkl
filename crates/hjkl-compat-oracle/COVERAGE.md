# Oracle Coverage Matrix

Tracks which vim features have oracle cases. Update this file when adding corpus
cases or shipping new features. The oracle runner lives in `tests/oracle.rs`;
corpus files are in `corpus/`.

Cases run via `cargo nextest run -p hjkl-compat-oracle`. Two driver paths exist:

- **in-process** (`run_oracle`) — fast, key-replay through the engine FSM; used
  by most test functions.
- **nvim-api subprocess** (`run_case_via_nvim_api`) — spawns `hjkl --nvim-api`;
  used for ex-command and sneak cases that need full dispatch.

Intentional divergences are documented in `corpus/known_divergences.toml`.

---

## Motions

| Feature                       | Cases | File(s)                                                   |
| ----------------------------- | ----- | --------------------------------------------------------- |
| h                             | 1     | tier1.toml                                                |
| j                             | 1     | tier1.toml                                                |
| j / k phantom trailing row    | 8     | tier2_vertical_phantom_row.toml                           |
| k                             | 1     | tier1.toml                                                |
| l                             | 2     | sample.toml, tier1.toml                                   |
| w                             | 1     | tier1.toml                                                |
| W                             | 1     | tier1.toml                                                |
| b                             | 1     | tier1.toml                                                |
| B                             | 1     | tier1.toml                                                |
| e                             | 1     | tier1.toml                                                |
| E                             | 1     | tier1.toml                                                |
| ge                            | 1     | tier2_paragraph_word.toml                                 |
| gE                            | 1     | tier2_gaps.toml                                           |
| g\_ (last non-blank)          | 1     | tier2_gaps.toml                                           |
| 0                             | 1     | tier1.toml                                                |
| ^                             | 1     | tier1.toml                                                |
| $                             | 1     | tier1.toml                                                |
| gg                            | 1     | tier1.toml                                                |
| G                             | 1     | tier1.toml                                                |
| f / F                         | 2     | tier1.toml, tier2_search.toml                             |
| t / T                         | 2     | tier1.toml, tier2_search.toml                             |
| ; / ,                         | 2     | tier1.toml                                                |
| % (match bracket)             | 1     | tier1.toml                                                |
| n / N                         | 2     | tier2_search.toml                                         |
| \* / #                        | 2     | tier2_search.toml                                         |
| + / - / \_                    | 6     | tier1.toml                                                |
| [[/]] / [] / ][               | 4     | tier1.toml                                                |
| \| (goto column)              | 2     | tier1.toml                                                |
| { / } (paragraph)             | 2     | tier2_paragraph_word.toml                                 |
| ( / ) (sentence)              | 11    | tier2_sentence.toml                                       |
| H (viewport top)              | 1     | tier2_gaps.toml                                           |
| M (viewport middle)           | 2     | tier2_viewport_bounds.toml                                |
| L (viewport bottom)           | 3     | tier2_viewport_bounds.toml                                |
| zz / zt / zb                  | —     | TODO: not yet implemented (#63)                           |
| sneak s/S (ON)                | —     | intentional divergence — see corpus/tier2_sneak.toml note |
| sneak s/S (disabled fallback) | 2     | tier2_sneak.toml                                          |
| Nw (5w)                       | 1     | tier2_gaps.toml                                           |
| Nb (3b)                       | 1     | tier2_gaps.toml                                           |

## Operators

| Feature                                             | Cases | File(s)                                                               |
| --------------------------------------------------- | ----- | --------------------------------------------------------------------- |
| d (dw, dd, d$, d0, d2j, d+, d-, d\_, d\|, d[[, d]]) | 10+   | tier1.toml                                                            |
| D                                                   | 1     | tier1.toml                                                            |
| c (cw, cc, C)                                       | 3     | tier1.toml                                                            |
| y (yw, yy, Y)                                       | 3     | tier1.toml                                                            |
| p / P                                               | 6     | tier1.toml                                                            |
| x / X                                               | 2     | tier1.toml                                                            |
| r                                                   | 1     | tier1.toml                                                            |
| ~ (toggle case)                                     | 1     | tier1.toml                                                            |
| J                                                   | 2     | tier2_case_indent_join.toml, tier2_gaps.toml                          |
| gJ                                                  | 1     | tier2_case_indent_join.toml                                           |
| gU / gu / g~                                        | 3     | tier2_case_indent_join.toml                                           |
| > / < (indent/outdent)                              | —     | excluded — shiftwidth diverges between hjkl defaults and nvim --clean |
| = (auto-indent)                                     | —     | TODO                                                                  |
| ! (filter)                                          | —     | TODO                                                                  |
| gq (reflow)                                         | —     | TODO (gw is tested via nvim_api_tier)                                 |
| gw (cursor-stable reflow)                           | 3     | nvim_api_tier.toml                                                    |
| gc (comment)                                        | —     | TODO                                                                  |
| 3dd (count)                                         | 1     | tier2_gaps.toml                                                       |
| 2yyp (count yank+paste)                             | 1     | tier2_gaps.toml                                                       |

## Text Objects

| Feature            | Cases | File(s)                        |
| ------------------ | ----- | ------------------------------ |
| aw / iw            | 2     | tier1.toml                     |
| aW / iW            | —     | TODO                           |
| ap / ip            | 2     | tier2_text_objects.toml        |
| as / is            | 2     | tier2_text_objects.toml        |
| a" / i"            | 2     | tier1.toml                     |
| a' / i'            | 2     | tier2_text_objects.toml        |
| a( / i(            | 2     | tier1.toml                     |
| a[ / i[            | 2     | tier2_text_objects.toml        |
| a{ / i{            | 2     | tier1.toml                     |
| a< / i<            | 2     | tier2_gaps.toml                |
| a` / i`            | —     | TODO (backtick string objects) |
| at / it (HTML tag) | 2     | tier2_gaps.toml                |

## Modes

| Feature              | Cases               | File(s)                                   |
| -------------------- | ------------------- | ----------------------------------------- |
| normal               | all                 | various                                   |
| insert (i/a/I/A/o/O) | 6                   | tier1.toml                                |
| visual char (v)      | 4                   | tier2_visual.toml                         |
| visual line (V)      | 4                   | tier2_visual.toml                         |
| visual block (C-v)   | 7                   | tier2_visual_block.toml                   |
| command-line (:)     | via nvim-api driver | nvim_api_tier.toml, tier2_substitute.toml |
| terminal             | —                   | (not shipped)                             |
| replace (R)          | 8                   | tier2_replace_mode.toml                   |

## Insert Primitives

| Feature                          | Cases | File(s)             |
| -------------------------------- | ----- | ------------------- |
| i / a / I / A / o / O            | 6     | tier1.toml          |
| c / C / s / S                    | 3     | tier1.toml          |
| x / X                            | 2     | tier1.toml          |
| C-w (delete word back)           | 1     | tier2_advanced.toml |
| C-u (delete to line start)       | 1     | tier2_advanced.toml |
| C-r (paste register in insert)   | —     | TODO                |
| gi (resume last insert position) | 1     | tier2_advanced.toml |

## Counts

| Feature                | Cases | File(s)                     |
| ---------------------- | ----- | --------------------------- |
| Nw (5w motion)         | 2     | tier1.toml, tier2_gaps.toml |
| Nj (5j motion)         | 1     | tier2_advanced.toml         |
| NG clamped (100G)      | 1     | tier2_advanced.toml         |
| Ndd (3dd)              | 1     | tier2_gaps.toml             |
| Nyy (3yy)              | 1     | tier2_advanced.toml         |
| Np (3p paste)          | 2     | tier1.toml, tier2_gaps.toml |
| Nx (5x)                | 1     | tier1.toml                  |
| N. (3. dot with count) | 1     | tier2_gaps.toml             |

## Marks

| Feature                                       | Cases | File(s)                            |
| --------------------------------------------- | ----- | ---------------------------------- |
| ma / 'a (line jump)                           | 1     | tier2_marks.toml                   |
| ma / \`a (char jump)                          | 1     | tier2_marks.toml                   |
| '< / '> (visual bounds)                       | 2     | tier2_marks.toml                   |
| '[ / '] (change bounds)                       | 2     | tier2_marks.toml                   |
| '. (last edit)                                | 2     | tier2_marks.toml, tier2_jumps.toml |
| g; / g, (changelist)                          | 11    | tier2_jumps.toml                   |
| mark shift on insert-above line (ma, ggO, `a) | 1     | tier2_marks.toml                   |
| mA-Z (global marks)                           | —     | TODO                               |
| '' (last-jump, linewise)                      | 1     | tier2_jumps.toml                   |
| \`\` (last-jump, exact col)                   | 2     | tier2_jumps.toml                   |
| '0-'9 (viminfo marks)                         | —     | (not shipped)                      |

## Registers

| Feature                          | Cases | File(s)                                   |
| -------------------------------- | ----- | ----------------------------------------- |
| " (unnamed / default)            | 4     | tier1.toml                                |
| "0 (yank register)               | 1     | tier2_advanced.toml                       |
| "a-"z (named yank)               | 2     | tier1.toml                                |
| "A-"Z (named append)             | 1     | tier1.toml                                |
| "\_ (black hole)                 | 1     | tier2_advanced.toml                       |
| named paste ("ayy "ap)           | 1     | tier2_gaps.toml                           |
| "+ / "\* (system clipboard)      | —     | not oracle-able (headless has no display) |
| "- (small delete)                | —     | TODO                                      |
| C-r (insert-mode register paste) | —     | TODO                                      |

## Search / Substitute

| Feature                  | Cases | File(s)                                   |
| ------------------------ | ----- | ----------------------------------------- |
| / (forward search)       | 2     | tier2_search.toml                         |
| ? (backward search)      | 1     | tier2_search.toml                         |
| n / N                    | 2     | tier2_search.toml                         |
| \* / #                   | 2     | tier2_search.toml                         |
| word boundary \b         | 1     | tier2_search.toml                         |
| vim default-magic regex (\( \) \+ \? \| \{n,m}, literal ( ) + ? \| {}) | 5 | tier2_regex_magic.toml |
| \v / \V magic-mode switches | 3  | tier2_regex_magic.toml                    |
| :s replacement \u / \l (+ \U/\L interaction) | 2 | tier2_regex_magic.toml   |
| :s/pat/rep/              | 4     | tier2_substitute.toml, nvim_api_tier.toml |
| :s/g flag                | 2     | tier2_substitute.toml                     |
| :s/i flag (case)         | 2     | tier2_substitute.toml, nvim_api_tier.toml |
| :g/ global               | 1     | tier2_substitute.toml                     |
| & / :&                   | —     | TODO                                      |
| bare :s (repeat last sub)| 4     | tier2_bare_s_repeat.toml                  |
| g&                       | 2     | nvim_api_tier.toml                        |
| smartcase + \c/\C        | 4     | nvim_api_tier.toml                        |
| :s/c interactive confirm | —     | oracle skipped (#171)                     |

## Ex Commands

| Feature                        | Cases | File(s)                                    |
| ------------------------------ | ----- | ------------------------------------------ |
| :syntax on/off                 | 4     | nvim_api_tier.toml                         |
| :redraw / :redraw!             | 2     | nvim_api_tier.toml                         |
| :set scrolloff / sidescrolloff | 2     | nvim_api_tier.toml                         |
| :set list / nolist / listchars | 4     | nvim_api_tier.toml                         |
| :retab / :retab!               | 3     | nvim_api_tier.toml                         |
| :earlier / :later              | 3     | nvim_api_tier.toml                         |
| :q / :q! / :qa / :wq           | —     | exit commands can't oracle (process exits) |
| :w / :wall                     | —     | requires a file path, not oracled          |
| :e / :edit                     | —     | requires a file, not oracled               |
| :r                             | —     | TODO                                       |
| :1,3d (range delete)           | —     | TODO                                       |
| :wa                            | —     | requires files, not oracled                |
| :put / :put!                   | —     | TODO                                       |
| :join / :j / :j! (gJ)          | 7     | tier2_ex_join.toml                         |
| :sort                          | —     | TODO                                       |
| :set noignorecase              | —     | TODO                                       |
| :set colorizer                 | —     | (not shipped)                              |
| :set indent_guides             | —     | (not shipped in oracle)                    |
| :set format_on_save            | —     | (not shipped in oracle)                    |
| :set trim_trailing_whitespace  | —     | (not shipped in oracle)                    |

## Macros

| Feature                | Cases | File(s)           |
| ---------------------- | ----- | ----------------- |
| q{a} record            | 3     | tier2_macros.toml |
| @{a} play              | 3     | tier2_macros.toml |
| @: (repeat last ex)    | —     | TODO              |
| @@ (repeat last macro) | 1     | tier2_macros.toml |
| N@{a} counted play     | 1     | tier2_macros.toml |

## Windows / Tabs

| Feature                           | Cases | File(s)       |
| --------------------------------- | ----- | ------------- |
| C-w s / v / h / j / k / l / c / o | —     | (not shipped) |
| :split / :tabnew                  | —     | (not shipped) |

## Misc

| Feature                      | Cases | File(s)                                                                                                                                                                                                                                                                                |
| ---------------------------- | ----- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| . (dot repeat)               | 10    | tier2_dot_repeat.toml                                                                                                                                                                                                                                                                  |
| . with count (3.)            | 1     | tier2_gaps.toml                                                                                                                                                                                                                                                                        |
| J. (join dot)                | 1     | tier2_gaps.toml                                                                                                                                                                                                                                                                        |
| u (undo)                     | 2     | tier1.toml                                                                                                                                                                                                                                                                             |
| U (undo line)                | 5     | not oracled — `nvim_buf_set_lines` seeding is itself undo-tracked by real nvim, so `U`'s restore target ends up pointing at the pre-seed empty buffer; pinned as unit tests in `hjkl-vim/tests/undo_line.rs` instead, each verified against a real `nvim --headless <file>` invocation |
| C-r (redo)                   | 1     | tier1.toml                                                                                                                                                                                                                                                                             |
| ZZ / ZQ                      | —     | exit commands, not oracled                                                                                                                                                                                                                                                             |
| C-a / C-x (incr/decr)        | 2     | tier2_advanced.toml                                                                                                                                                                                                                                                                    |
| C-a / C-x leading-zero width | 5     | tier2_increment_count.toml                                                                                                                                                                                                                                                             |
| gv (reselect)                | 1     | tier2_visual.toml                                                                                                                                                                                                                                                                      |
| :earlier / :later            | 3     | nvim_api_tier.toml                                                                                                                                                                                                                                                                     |
| modeline                     | 3     | nvim_api_tier.toml                                                                                                                                                                                                                                                                     |

---

## Intentional Divergences

See `corpus/known_divergences.toml` for the current list. As of 2026-05-27 the
file contains `cases = []` — all previously tracked divergences were fixed in
hjkl-engine 0.5.8 (issue #83).

Notable design-level divergences (not tracked as bugs):

- **sneak `s`/`S` ON mode** — hjkl's vim-sneak behaviour intentionally diverges
  from nvim's default `s` (substitute-char); sneak-ON is not oracled. Sneak
  disabled (fallback) is covered in `tier2_sneak.toml`.
- **indent `>>`/`<<`** — excluded from oracle because hjkl defaults to
  `shiftwidth=4` while nvim `--clean` uses `shiftwidth=8`; content diverges.
  Indent is exercised via `gU`/`gu`/`g~` case logic tests instead.
- **system clipboard `"+`/`"*`** — headless nvim has no X11/Wayland display;
  clipboard ops silently fall back and cannot be compared.
