# Code Review

**Project:** hjkl (terminal text editor) **Date:** 2026-07-23 **Depth:** high

---

## Findings

### High (0)

> **Verified 2026-07-23 — the original H1 does not hold; there are no High
> findings.**

**H1 (WITHDRAWN) — Save fallback path does _not_ drop permissions on existing
files** `apps/hjkl/src/save.rs:108-120`

Original claim: the atomic path preserves mode via `set_permissions` but the
fallback (`File::create(path)`) creates with `umask` defaults, stripping the
executable bit off a `0755` script when `tempfile`+`rename` fails (`EXDEV`,
unwritable parent).

**Why it's withdrawn:** `std::fs::File::create` =
`open(O_WRONLY|O_CREAT|O_TRUNC)`. On an **already-existing** file
`O_CREAT|O_TRUNC` truncates the contents but leaves the inode's permission bits
untouched — the `mode` argument is ignored when the file exists. So a
pre-existing `0755` script routed through the fallback **retains** `0755`; the
executable bit is not lost. Umask defaults apply only when the fallback creates
a genuinely **new** file, where there is no prior mode to preserve and umask is
the correct behaviour.

The originally-proposed fix was also incorrect: it read
`std::fs::metadata(path)` _after_ `File::create(path)` had already
created/truncated the file, so it observed the new file's own permissions and
set them back onto themselves — a no-op preserving nothing. No change is needed.

---

**H2 → L3 — `read_vim_range` dead `+1` code (non-final-row branch)**
`crates/hjkl-vim/src/vim/command.rs:37-63`

The code computes `hi_unclamped = line.chars().count() + 1` then clamps to
`hi = row_chars.len()`. The `+1` is intended to include the trailing newline,
but clamping always reduces it back to the line length; the separate
`out.push('\n')` handles the newline correctly. Dead, misleading code — a
maintenance trap if range handling is refactored, but not a functional bug.
(Correction: this lives in the non-final-row `else` arm and is shared by both
`Inclusive` and `Exclusive` ranges — not Exclusive-specific as first stated.
Reclassified High → Low: cosmetic.)

---

### Medium (2)

**M1 — Save write-permission check opens target for write (metadata side
effect)** `apps/hjkl/src/save.rs:55-59`

```rust
match std::fs::OpenOptions::new().write(true).open(&target) {
    Ok(_) => {}
    ...
}
```

This is a permission probe only (the handle is dropped unused). The original
"ctime/mtime churn" concern is **overstated**: on Linux a bare `O_WRONLY` open
with no subsequent write does not update the file's timestamps, so there is no
metadata churn — the only real cost is one extra syscall. The TOCTOU race
(permissions changing between probe and rename) is real but harmless: the rename
simply fails, the temp file is cleaned up, and the error is propagated — no data
loss.

**Fix (cosmetic):** Use `std::fs::metadata(&target)` for the readability/perms
check instead of an `OpenOptions::write(true).open`, dropping the extra
write-open syscall.

---

**M2 — `undo_group_enter`/`undo_group_exit` state machine fragile on depth
re-entry** `crates/hjkl-buffer/src/content.rs:193-198`

If a group is entered, `depth` increments from 0→1, snapshot armed. If a nested
group is entered (1→2), `armed` is preserved. If the outer group exits (2→1),
then enters again (1→2), `armed` and `open_gen` are not reset — correct. But if
a future async/interleaved path managed to `exit` then `enter` at depth 0→1→0→1
without closing, the snapshot would be lost.

**Verdict:** No practical bug given single-threaded usage and exclusive borrows.
The state machine is fragile and would not survive concurrent access.

---

### Low (2)

**L1 — `set_cursor` clamps beyond-EOF positions, losing intended cursor**
`crates/hjkl-buffer/src/buffer.rs:149-162`

When a cursor is set past EOF, it is clamped to the last valid position. The
`last_cursor` field records the clamped value, so cross-session persistence
loses the "intended" position for buffers that shrink. Documented as
"best-effort." Not a bug, but worth noting.

---

**L2 — `\n` in substitute replacement string maps to null byte**
`crates/hjkl-engine/src/substitute.rs:756`

Vim-compatible: `:s/foo/\n/g` inserts `\0` (null byte) per
`:h sub-replace-special`. Valid in Rust `String`, but can confuse terminal
display, some filesystem tools, and C-FFI consumers that treat null as string
terminator. Documented behavior — not a bug, but surprising to users who expect
a literal newline.

---

## Summary

| Severity | Count | Note                                               |
| -------- | ----- | -------------------------------------------------- |
| High     | 0     | original H1 withdrawn on verification              |
| Medium   | 2     | M1 side-effect claim overstated; both non-blocking |
| Low      | 3     | +H2 reclassified (dead `+1`); L1, L2               |

**Verdict: Safe — no blocking findings.** The buffer/rope/undo subsystem, engine
FSM, LSP integration, and clipboard layers are logically sound. After
verification against source (2026-07-23), the original "must-fix" H1 does not
hold — `File::create` truncates an existing file without resetting its mode, so
saved scripts keep their executable bit. Nothing here warrants a fix before
release; the remaining items are cosmetic (dead code, one extra syscall on `:w`)
or documented vim-parity behaviour.

No path to data corruption under normal operation found.
