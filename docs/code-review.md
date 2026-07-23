# Code Review

**Project:** hjkl (terminal text editor)
**Date:** 2026-07-23
**Depth:** high

---

## Findings

### High (2)

**H1 — Save fallback path silently drops file permissions**
`apps/hjkl/src/save.rs:108-120`

The atomic save path (lines 88–90) preserves the target file's permission bits
via `f.set_permissions(meta.permissions())`. The fallback path (line 116,
`File::create(path)`) does not — it creates with `umask` defaults.

**Failure scenario:** A script with mode `0755` saved on a filesystem where
`tempfile` + `rename` fails (e.g. `EXDEV` cross-device rename, or unwritable
parent directory) silently loses its executable bit. The user does `:w`, gets no
error, the file is saved but no longer executable.

**Fix:** Copy existing permissions in the fallback path:
```rust
let mut f = std::fs::File::create(path)?;
if let Ok(meta) = std::fs::metadata(path) {
    f.set_permissions(meta.permissions())?;
}
write_body(&mut f, body, trailing_nl)?;
```

---

**H2 — `read_vim_range` dead `+1` code on Exclusive range**
`crates/hjkl-vim/src/vim/command.rs:37-63`

For `RangeKind::Exclusive`, the code computes `hi_unclamped = line.chars().count() + 1`
then clamps to `hi = row_chars.len()`. The `+1` is intended to include the
trailing newline, but clamping always reduces it back to the line length. The
separate `out.push('\n')` handles the newline correctly. The `+1` is dead code
and misleading — not a functional bug now, but a maintenance trap if range
handling is refactored.

---

### Medium (2)

**M1 — Save write-permission check opens target for write (metadata side effect)**
`apps/hjkl/src/save.rs:55-59`

```rust
match std::fs::OpenOptions::new().write(true).open(&target) {
    Ok(_) => {}
    ...
}
```

Opening for write can update `ctime`/`mtime` on some systems (POSIX permits
`O_WRONLY` to update `st_ctime`), causing unnecessary filesystem metadata churn
on every `:w`. Also a TOCTOU race — permissions could change between the check
and the rename.

**Failure scenario:** Metadata-only churn; the rename-after-check race is
harmless (rename fails, temp cleaned up, error propagated). No data loss.

**Fix:** Use `read(true).write(false)` or `std::fs::metadata(&target)` instead.

---

**M2 — `undo_group_enter`/`undo_group_exit` state machine fragile on depth re-entry**
`crates/hjkl-buffer/src/content.rs:193-198`

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

Vim-compatible: `:s/foo/\n/g` inserts `\0` (null byte) per `:h sub-replace-special`.
Valid in Rust `String`, but can confuse terminal display, some filesystem tools,
and C-FFI consumers that treat null as string terminator. Documented behavior —
not a bug, but surprising to users who expect a literal newline.

---

## Summary

| Severity | Count |
|----------|-------|
| High     | 2     |
| Medium   | 2     |
| Low      | 2     |

**Verdict: Safe.** The buffer/rope/undo subsystem, engine FSM, LSP integration,
and clipboard layers are logically sound. The save fallback path dropping
permissions (H1) is the only finding that warrants a fix before a release — it
silently strips the executable bit from scripts and the user gets no error.

No path to data corruption under normal operation found.
