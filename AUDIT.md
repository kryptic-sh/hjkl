# Security and Reliability Audit

**Date:** 2026-07-18
**Scope:** Rust workspace source review, with focus on untrusted inputs,
filesystem operations, process lifecycle, and error handling.

## Summary

| Severity | Count |
| -------- | ----: |
| High     |     4 |
| Medium   |     5 |
| Low      |     0 |

`cargo clippy --all-targets --all-features -- -D warnings` and
`cargo test --workspace --all-features` passed. Passing tests do not cover the
findings below.

## Findings

### High — Config lock always appears stale, allowing concurrent writes to lose updates

**Location:** `crates/hjkl-config/src/write.rs:146-160`

`lock_is_stale` returns `true` after any successfully read lock file, including
when its parsed PID is alive. A contending `write_key_at` therefore removes a
live lock at `write.rs:106-111`; two read-modify-write operations may proceed,
and the final atomic rename silently overwrites the other process's setting.

**Remediation:** Return `false` for a readable, parseable lock owned by a live
process. Reclaim only expired, unreadable, malformed, or dead-owner locks. Add a
concurrent live-lock regression test.

### High — Grammar manifest path fields can escape the cloned source root

**Locations:** `crates/hjkl-bonsai/src/runtime/source.rs:198-202`;
`crates/hjkl-bonsai/src/runtime/compile.rs:106-110`;
`crates/hjkl-bonsai/src/runtime/manifest.rs:31-55`

A parsed `LangSpec.subpath` is joined directly to the clone directory, and each
manifest-provided `c_files` path is joined directly to that resulting root.
Neither parsing nor use rejects absolute paths or parent-directory components. A
custom manifest can consequently make the runtime compiler consume arbitrary
local files outside its grammar cache.

**Remediation:** Validate all path-like manifest fields at parse time: reject
absolute, prefix/root, and parent-directory components. Before use, verify
resolved paths remain below the intended clone root. Cover `../` and absolute
paths with parser and compiler tests.

### High — nvim-api inbound size guard runs after reads instead of before them

**Location:** `apps/hjkl/src/nvim_api.rs:2394-2405,2421-2430`

`LimitedReader::read` fills the decoder-provided buffer before testing the
per-message limit. The MessagePack decoder receives this reader directly; a peer
can declare an oversized string, binary value, or container before the reader
records the limit breach, potentially inducing excessive allocation.

**Remediation:** Limit the supplied read slice to remaining budget before
calling `inner.read`, and error when no budget remains. Prefer decoder limits
that reject declared container/string sizes before allocation. Add oversized
`str32` and `bin32` regression tests.

### High — Exited LSP servers remain registered and cannot restart

**Locations:** `crates/hjkl-lsp/src/runtime.rs:28-98,112-114,148-161`;
`crates/hjkl-lsp/src/server.rs:593-625`; `apps/hjkl/src/app/lsp_glue.rs:349-352`

The child wait task emits `LspEvent::ServerExited`, but the runtime actor only
receives commands and never consumes exit events. It retains both the dead
`Server` and its attached buffers. Later attach requests for those buffers are
ignored, while the UI merely removes display state, leaving requests directed at
a dead writer until the editor restarts.

**Remediation:** Feed child-exit state into the runtime actor; remove the
server, detach its buffers, and clear/fail its outstanding requests so a later
attach spawns a replacement. Add an integration test that kills a server then
verifies reattachment restarts it.

### Medium — Swap recovery reads full swap bodies without a size cap

**Location:** `crates/hjkl-app/src/swap.rs:359-363`

`read_swap` caps header allocation but calls `read_to_string` for every
remaining byte in the swap. A corrupt or oversized cache entry can force an
unbounded allocation during recovery/startup scanning.

**Remediation:** Enforce a maximum body/file size using metadata and a bounded
reader before allocating. Return `InvalidData` for oversized swaps and test that
recovery continues safely after rejection.

### Medium — Filesystem watcher discards backend errors without requesting rescan

**Location:** `crates/hjkl-fs-watch/src/lib.rs:382-401`

The worker handles `Ok(Err(_))` by continuing. Unlike channel overflow, a
watcher backend error neither records context nor sets `overflow`, so consumers
receive no `FsEvent::Rescan` and can retain stale editor or explorer state.

**Remediation:** Log and surface backend errors, or set `overflow` so the
existing rescan path reconciles state. Add a test injecting a `notify::Error`.

### Medium — Async grammar loads can wait forever after worker dispatch failure

**Location:** `crates/hjkl-bonsai/src/runtime/async_loader.rs:153-167`

The code inserts subscribers into `in_flight`, then discards `job_tx.send`
failure. If all workers have exited, the returned handle's receiver is retained
in the map but will never receive a result; later callers subscribe to the same
unserviceable entry.

**Remediation:** On send failure, remove the entry and notify every subscriber
with a dispatch/cancelled error. Add a closed-worker-channel regression test.

### Medium — Grammar cache identity truncates pinned revisions to 12 characters

**Location:**
`crates/hjkl-bonsai/src/runtime/source.rs:90-95,138,151-154,188-195`

Cache paths, staging paths, and lock keys use the first 12 bytes of `git_rev`.
Two distinct pinned revisions sharing that prefix reuse the same cached clone,
which can compile and load a revision other than the manifest-selected one.

**Remediation:** Use the full validated commit SHA, or a collision-resistant
hash of it, for cache identity. Keep shortened revisions only for display.

### Medium — Non-TUI modes ignore configuration flags and validation

**Location:** `apps/hjkl/src/main.rs:508-525,535-557`

`--nvim-api`, `--embed`, and `--headless` return before the only configuration
load/validation block. Their parsed `--config` and `--clean` options therefore
have no effect, and invalid default configuration is not reported.

**Remediation:** Resolve and validate configuration before dispatching modes,
then pass the resolved config and persistence policy to each constructor. If
configuration is intentionally unsupported in a mode, reject these flags rather
than silently ignoring them.

## Notes

No source code changed during this audit. Findings are ordered by severity, then
by impact on data integrity, local resource exhaustion, and runtime recovery.
