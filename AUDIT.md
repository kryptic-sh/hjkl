# Phase 0 Audit Findings

Run date: 2026-04-26. Source: sqeel main @ tag `pre-hjkl-extraction`.

## License

- `sqeel/LICENSE`: MIT, copyright mxaddict.
- `sqeel-vim/Cargo.toml`, `sqeel-buffer/Cargo.toml`: no `license` field
  (inherits from workspace, also MIT).
- **Compatible** with hjkl MIT. Code transfer clean.

## Source size

| File                            | Lines      |
| ------------------------------- | ---------- |
| `sqeel-vim/src/vim.rs`          | 8 534      |
| `sqeel-vim/src/ex.rs`           | 1 913      |
| `sqeel-vim/src/editor.rs`       | 1 539      |
| `sqeel-vim/src/registers.rs`    | 177        |
| `sqeel-vim/src/input.rs`        | 259        |
| `sqeel-vim/src/lib.rs`          | 62         |
| **`sqeel-vim` total**           | **12 484** |
| `sqeel-buffer` total (12 files) | **4 463**  |

`vim.rs` at 8 534 is the dominant module. Will need internal split during phase
5 (FSM grammar / motions / operators / text objects). Pre-split in sqeel before
subtree-split is recommended.

`editor.rs` under 2 000 — no pre-split refactor needed there.

## Test baseline

`cargo test -p sqeel-vim`: **448 passed**, 2 suites, 0 failures. Lock as
non-regression target.

## `Buffer` trait surface (audit-derived)

45 unique methods in `sqeel-vim` reach into `ed.buffer()` / `ed.buffer_mut()`.
Categorized:

| Category        | Count | In hjkl-engine `Buffer`?   | Notes                                                                                                                                  |
| --------------- | ----- | -------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| Cursor + motion | 24    | Yes (move into engine FSM) | Most are motion helpers; should live in `hjkl-engine::motion`, not `Buffer`. Buffer keeps `cursor`, `set_cursor`, `cursor_screen_row`. |
| Query           | 4     | Yes                        | `line`, `lines`, `row_count`                                                                                                           |
| Edit            | 1     | Yes                        | `replace_all`                                                                                                                          |
| Search          | 4     | Yes                        | `search_forward/backward`, `(set_)search_pattern`                                                                                      |
| Folds           | 8     | **No** — host concern      | `add_fold`, `close_*`, `open_*`, `remove_fold_at`, `toggle_fold_at`, `folds`. Move to `Host::FoldProvider`.                            |
| Viewport        | 3     | **No** — host concern      | `viewport`, `viewport_mut`, `move_viewport_*`. Engine tracks viewport state; buffer doesn't.                                           |

After relocating folds + viewport: **~34 buffer-relevant methods**, but most are
motions that become engine-side functions, not trait methods. Real `Buffer`
trait surface is closer to **12-15 methods** split across `Cursor`, `Query`,
`Edit`, `Search` sub-traits — within the <40 cap.

Full list: `audit-buffer-methods.txt`.

## UI coupling (ratatui / crossterm)

Three coupling points in `sqeel-vim`:

- **`crossterm::event`** — `KeyEvent`, `KeyCode`, `KeyModifiers`. Used in
  `vim.rs:5066`, `editor.rs:12, 1032`, `ex.rs:1126`. → Engine ships own `Input`
  enum; `From<crossterm::KeyEvent>` lives behind `crossterm` feature flag in
  `hjkl-engine`.
- **`ratatui::style::Style`** — leaked into engine state:
  - `style_table: Vec<ratatui::style::Style>` (`editor.rs:67`)
  - `styled_spans: Vec<Vec<(usize, usize, ratatui::style::Style)>>`
    (`editor.rs:76`)
  - `intern_style(style: ratatui::style::Style)` (`editor.rs:276`)
  - 4 hits in `vim.rs:7914-7955` (function-local imports) → Engine needs own
    `Style` struct; conversion lives in `hjkl-ratatui`.
- **`ratatui::layout::Rect`** — `editor.rs:13, 1511, 1522, 1533`. Lines
  1511-1533 are tests; only line 13 is import. Real usage minimal.

## Editor constructors

`Editor::new` callsites: **52** total across `sqeel-vim`, `sqeel-tui`,
`sqeel-gui`. Production callsites (~3) in `sqeel-tui/src/lib.rs`; rest are
tests. **Codemod is essential** — manual rewrite is a serious time sink.

Other public Editor methods reached: `last_search`, `mutate_edit`,
`set_syntax_fold_ranges`, `settings_mut`, `style_resolver`. Manageable.

## Clipboard

- Engine side (`sqeel-vim/src/registers.rs`): `set_clipboard(text, linewise)` —
  pure register slot mutation, no I/O.
- Host side (`sqeel-tui/src/clipboard.rs`): real `Clipboard::set_text(...)` —
  synchronous, returns bool.

**No async/await on any clipboard call.** Validates hybrid model (sync trait,
host queues async if needed).

## Dot-repeat shape

`sqeel-vim::vim::LastChange` is a **parsed-action enum** (not raw input).
Variants: `OpMotion`, `OpTextObj`, `LineOp`, `CharDel`, `ReplaceChar`,
`ToggleCase`, `JoinLine`, `Paste`, `DeleteToEol`, `OpenLine`, `InsertAt`. Each
carries operator + count + inserted text where relevant.

**Multicursor fan-out** can replay this directly per selection. No refactor
needed. Plan validated.

## sqeel-buffer SQL extensions

Searched for `sql`, `SQL`, `token`, `sqeel_core` in `sqeel-buffer/src/`:

- `lib.rs:11`: comment "shaped for SQL editing inside sqeel". Aspirational, not
  concrete.
- `span.rs:4`: comment "doesn't have to re-tokenise each frame". Generic spans,
  opaque style IDs.
- **No SQL-specific symbols, no `sqeel-core` dependency.**

Deps: `ratatui = "0.29"` (Style coupling), `regex`, `unicode-width`. Edition
2024 (sqeel) — hjkl will pin 2021 to match buffr.

**Decision**: sqeel-buffer becomes `hjkl-buffer` wholesale. No `SqlBuffer`
extension trait needed. ratatui Style usage will be replaced with engine-native
`Style` during phase 4 decoupling.

## buffr current state

(Audit deferred — not yet pulled this session. Phase 10 work.)

## sqeel-tui / sqeel-gui coordination

Both crates live in same workspace as `sqeel-vim` and `sqeel-buffer` (single
`sqeel/` repo, single `Cargo.toml` at root). Phase 8 dep swap is one PR, not
coordinated multi-PR.

## crates.io names

All four published as 0.0.0 placeholders 2026-04-26:

- https://crates.io/crates/hjkl-engine
- https://crates.io/crates/hjkl-buffer
- https://crates.io/crates/hjkl-editor
- https://crates.io/crates/hjkl-ratatui

## Tag

`sqeel main` @ `pre-hjkl-extraction` (rollback path locked).

## Surprises vs MIGRATION.md estimates

| Item                        | Plan estimated        | Audit found                                                      |
| --------------------------- | --------------------- | ---------------------------------------------------------------- |
| `Buffer` trait method count | ~30                   | 45 raw → ~12-15 in trait after motion + fold/viewport relocation |
| `Editor::new` callsites     | "N test files"        | 52 — codemod critical                                            |
| `vim.rs` size               | unspecified           | 8 534 lines — needs internal split before subtree-split          |
| sqeel-buffer SQL extensions | unknown               | None — wholesale move OK                                         |
| Dot-repeat shape            | parsed-action assumed | confirmed                                                        |
| Clipboard async             | sync assumed          | confirmed sync                                                   |
| License compat              | unknown               | MIT — clean                                                      |

## Next

- Pre-split `vim.rs` in sqeel into smaller modules before phase 3 subtree-split.
  Land as one commit: `refactor: pre-split vim.rs for hjkl extraction`.
- Draft `SPEC.md` from audit + Spec Lock.
- Phase 1: governance files, CI, dependabot, cargo-deny.
