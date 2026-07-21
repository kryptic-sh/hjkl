# Known issues

Actionable, unresolved items for `hjkl`. Two tracks:

1. **Security** — from the 2026-07-18 workspace audit (17 findings; the rest are
   fixed and shipped on `main`).
2. **Vim parity** — real behavioral divergences from nvim found during
   compatibility rounds.

Intentional trade-offs (deliberate no-ops, safer-than-nvim choices) and
engine-limited impossibilities are **not** listed here — only things worth
actually fixing. Each entry gives the file, the defect, and a fix direction.

---

## Security

### S1. [HIGH] Arbitrary native code execution via grammar dlopen

- **File:** `crates/hjkl-bonsai/src/runtime/grammar.rs:79`, `compile.rs`,
  `source.rs`
- **Defect:** Tree-sitter grammars are cloned from remote git repos (per
  `bonsai.toml`), compiled with `$CC`/`$CXX`, and loaded via
  `libloading::Library::new(&so)` (dlopen) with full process privilege. No
  content-hash or signature verification, so a compromised upstream repo or a
  MITM on the clone injects C that runs in-process on the next compile→dlopen;
  `$CC` can also redirect compilation to an attacker binary.
- **Mitigations present:** path/name traversal validation (`is_safe_component`,
  `is_safe_relative_path`), full-SHA cache identity.
- **Reachability:** the manifest is bundled-only today (`include_str!`), so this
  is not remotely reachable — but the trust boundary must be hardened before any
  user-supplied manifest is allowed.
- **Fix:** pin grammar sources by content hash in `bonsai.toml`, verify before
  compile/dlopen; optionally run grammars out-of-process. Large, architectural.

### S2. [MEDIUM] macOS ObjC `transmute(objc_msgSend)` — type-safety hazard

- **File:** `crates/hjkl-clipboard/src/backend/macos.rs:120,130,140`
- **Defect:** `msg0`/`msg1`/`msg2` `transmute` the variadic `objc_msgSend` stub
  to concrete fn-pointer types. A signature mismatch is UB; a future call site
  with wrong arg types would corrupt the stack / segfault. macOS-only, not
  driven by external input — maintenance hazard.
- **Mitigations present:** documented ABI constraints; all args/results are
  pointer/usize-sized (no float/SIMD/large-struct returns).
- **Fix:** adopt `objc2`'s type-safe message-send macros, or add a compile-time
  ABI assertion per call site.

### S3. [LOW] `Buffer::line()` panics on out-of-bounds index

- **File:** `crates/hjkl-engine/src/buffer_impl.rs:130-139`
- **Defect:** `Query::line()` deliberately `panic!`s when
  `row >= rope.len_lines()`. An LSP server or ex command acting on a stale row
  count crashes the editor; in RPC modes this is a DoS vector.
- **Fix:** return `Option`/`Result` and let callers handle OOB gracefully. API
  change touching many call sites.

---

## Vim parity

### V1. Phantom trailing row on linewise deletes that reach EOF

Two symptoms of one root cause — ropey reports one extra empty "line" after a
buffer's own trailing `\n`, and the generic linewise-delete path (unlike the
`dd` handler) has no phantom-row clamp.

- **V1a — register capture.** `crates/hjkl-engine` `apply_visual_operator`,
  `Mode::VisualLine` `Operator::Delete` arm → `cut_vim_range` `Linewise` branch.
  A linewise Visual delete reaching the true last row captures one extra
  trailing `\n` in the register. Repro: `"one\ntwo\nthree\nfour\n"`, `VGd` →
  register `"…four\n\n"` (nvim: `"…four\n"`). Buffer/cursor correct; only the
  register text is wrong.
- **V1b — cursor placement.** `run_operator_over_range` `Operator::Delete` arm,
  `crates/hjkl-vim/src/vim/op_motion.rs`. A linewise operator-range delete
  consuming through the true last row mis-lands the cursor on the phantom
  trailing row and leaves an extra blank line in `content()`. Affects
  `dap`/`dip`/`d}`/`d{` and any linewise-range operator reaching EOF. Repro:
  `"foo\n\nbar\n"`, cursor on `"bar"`, `dap` → hjkl cursor `(1,0)` + trailing
  blank; nvim `"foo\n"`, cursor `(0,0)`.
- **Fix:** give the generic linewise-delete path the same phantom-row clamp
  `linewise.rs`'s `dd` handler already applies; audit `cut_vim_range`'s
  `Linewise` byte-range computation when the end row is phantom-adjacent.

### V2. `;` range separator not supported

- **File:** `parse_range` (hjkl-ex).
- **Defect:** only `,` is recognized as a range separator. `;` should move the
  cursor to the first resolved address before resolving the second. Repro:
  `"aaa\nfoo\nbbb\nfoo\nccc\n"`, `:/foo/;/foo/d` → nvim deletes rows 1-3
  (`"aaa\nccc\n"`); hjkl parses only the first address and errors on the
  `;/foo/d` remainder. (Errors cleanly — no silent corruption.)
- **Fix:** thread a "virtual cursor override" through `resolve_address` /
  `resolve_search_address` so the second address (every kind, not just search)
  sees the first address's resolved row. Broader plumbing than a new `Address`
  variant.

### V3. Visual-BLOCK operators do not participate in dot-repeat

- **File:** `apply_block_operator` (`Mode::VisualBlock` arm); block-change path
  (`InsertReason::BlockChange`).
- **Defect:** `.` after a visual-block operator is a no-op —
  `LastChange:: VisualOp` is only recorded for charwise (`v`) / linewise (`V`),
  not block. nvim repeats the block op over a same-size rectangle at the cursor.
- **Fix:** add a third extent shape (rows×cols rectangle + `$`-ragged `to_eol`
  flag), synthesize `vim_state`'s `block_anchor`/`block_vcol` on replay, and
  wire block-`c` through a dot-repeat patch site (it currently returns early via
  `BlockChange`, never touching `last_change`).

### V4. `:g/pat/normal {cmd}` unsupported

- **Defect:** `:g/foo/normal x` returns a clean "unsupported sub-command" error;
  nvim runs `normal x` on every matching line.
- **Fix:** blocked on there being no general `:normal {cmd}` ex command yet.
  Implement `:normal` first, then have `:g` replay it per matching line.

### V5. Magic `~` in a search/substitute PATTERN

- **Defect:** `~` in the pattern half of `:s` (or a `/`/`?` search) is treated
  as a literal tilde; nvim expands it to the previous `:s` replacement text.
  Repro: `:s/foo/BAR/` then `:s/~/baz/`. (`~` in the REPLACEMENT already works.)
- **Fix:** expand pattern-side `~` to the last substitute string in
  `hjkl-engine::search` before compiling the regex. Small, low usage.
