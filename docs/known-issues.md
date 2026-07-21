# Known issues

Actionable, unresolved items for `hjkl`. Only the **Security** track remains —
every vim-parity divergence found during the compatibility rounds has been fixed
and shipped on `main`.

Intentional trade-offs (deliberate no-ops, safer-than-nvim choices) and
engine-limited impossibilities are **not** listed here — only things worth
actually fixing. Each entry gives the file, the defect, and a fix direction. The
security items are not yet tracked as GitHub issues.

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
