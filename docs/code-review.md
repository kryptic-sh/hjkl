# Code Review

**Project:** hjkl (terminal text editor) **Date:** 2026-07-23 (pruned
2026-07-25) **Depth:** high

**Verdict: Safe — no blocking findings.** The buffer/rope/undo subsystem, engine
FSM, LSP integration, and clipboard layers are logically sound. No path to data
corruption under normal operation found. Remaining items below are cosmetic or
documented vim-parity behaviour.

## Resolved (pruned)

- **H1 — withdrawn.** Claim: save fallback (`File::create`) strips the exec bit
  off a `0755` script. False — `File::create` = `open(O_CREAT|O_TRUNC)`; on an
  existing file the mode arg is ignored, so a pre-existing `0755` script keeps
  `0755`. Umask applies only to genuinely new files, which is correct. No change
  needed.
- **H2 → L3 — fixed.** `read_vim_range` dead `+1` on the non-final-row branch
  removed; it now uses `line.chars().count()` (commit `5d19681e`).
- **M1 — withdrawn, superseded by the security-audit M4 fix (`414d03d0`).** The
  original suggestion was to replace the save write-permission probe
  (`OpenOptions::write(true).open(&target)` at `save.rs:57-66`) with
  `std::fs::metadata` to save one syscall. That open is now **load-bearing**: M4
  added `custom_flags(O_NOFOLLOW)` to it as the symlink-swap TOCTOU guard.
  `std::fs::metadata` follows symlinks (reopening the M4 hole), and
  `symlink_metadata` + a manual mode check is racy and doesn't actually test
  writability. The single non-truncating `O_NOFOLLOW` write-open does both jobs
  in one syscall — keep it. No change.

---

## Open (non-blocking)

### M2 — `undo_group_enter`/`exit` state machine fragile on depth re-entry

**`crates/hjkl-buffer/src/content.rs:193-198`**

No practical bug given single-threaded usage and exclusive borrows. The state
machine would not survive concurrent access (a `0→1→0→1` interleave without
closing would lose the snapshot). Informational — no fix warranted today.

### L1 — `set_cursor` clamps beyond-EOF positions

**`crates/hjkl-buffer/src/buffer.rs:149-162`** — a cursor set past EOF is
clamped and `last_cursor` records the clamped value, so cross-session
persistence loses the intended position for buffers that shrink. Documented
"best-effort." Not a bug.

### L2 — `\n` in substitute replacement maps to null byte

**`crates/hjkl-engine/src/substitute.rs:756`** — `:s/foo/\n/g` inserts `\0`
(vim-compatible, `:h sub-replace-special`). Valid in Rust `String` but can
confuse terminal display / C-FFI consumers. Documented behavior, not a bug.
