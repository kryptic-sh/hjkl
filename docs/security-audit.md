# Security Audit Report

**Project:** hjkl (terminal text editor) **Date:** 2026-07-23 **Version:**
0.35.0 **Depth:** high

---

## Findings

### High Severity (3)

**H2 — `dlopen` of remotely-compiled shared objects (arbitrary code execution)**
`crates/hjkl-bonsai/src/runtime/grammar.rs:79-89`, `compile.rs:136,163-171`

Tree-sitter grammars are downloaded from remote git repositories, compiled with
a compiler resolved from `$CC`/`$CXX` (or `cc`/`c++` from PATH), and `dlopen`ed
into the editor process. This is a documented trust boundary: arbitrary native
code execution from the grammar source, compiler, and build chain. The manifest
pins `git_url`/`git_rev` but performs no signature verification or hash pinning
of the compiled artifact. The `validate_clone_args` function rejects empty
strings and leading dashes but does not reject path separators in `git_rev`
values.

> Tracked as [GitHub issue #314](https://github.com/kryptic-sh/hjkl/issues/314)
> (with M2, the related `git_rev` cache-path traversal). Verified accurate
> 2026-07-23; not remotely reachable today (manifest is `include_str!`-bundled).

**H3 — Unbounded stdin read with `hjkl -`** `apps/hjkl/src/main.rs:672-673`

> **Resolved 2026-07-23:** Added `.take(256 MiB)` bound before `read_to_string`
> (commit `350ff8d8`). Residual (benign): if input reaches exactly 256 MiB the
> cut can fall mid-UTF-8-char, so `read_to_string` returns `InvalidData` and the
> buffer is skipped (error printed) rather than truncated-and-loaded — only
> reachable at a 256 MiB stdin, acceptable.

The `-` flag reads stdin to EOF with `read_to_string` and no size cap. If stdin
is connected to `/dev/zero`, a named FIFO that never closes, or a malicious
source, this allocates until OOM. Every other read path in the codebase (swap
files, formatter I/O, LSP codec, msgpack-RPC) has explicit caps (64 MiB, 256
MiB, etc.).

**H4 — ~30 unsafe FFI blocks in Wayland clipboard socket I/O**
`crates/hjkl-clipboard/src/backend/wayland_socket.rs:113-398`

> **Resolved 2026-07-23:** CMSG fd extraction now bounds-checked against
> `msg_controllen` (kernel-authoritative length); `send_with_fds` verifies fds
> fit the CMSG buffer before copying; 4 edge-case unit tests added covering
> socketpair fd send+receive, no-ancillary-data recv, path-too-long rejection,
> and malformed-advertised-size handling (commits `1c9f4314`, `8fdeafd7`).

Raw `libc::socket`, `libc::sendmsg`, `libc::recvmsg`, `libc::close`,
`libc::getuid`, and `libc::CMSG_*` macro expansion for Wayland data-device
communication (~26 `unsafe` blocks in the cited range, not literally 30). Each
block has SAFETY comments, but the cmsg parsing and raw fd handling is the
highest-risk FFI surface in the codebase. A malicious Wayland compositor sending
malformed cmsg data could trigger undefined behavior.

---

### Medium Severity (9)

**M1 — `:make` runs user-configured `makeprg` as command**
`apps/hjkl/src/app/quickfix.rs:781,787`

> **Resolved 2026-07-23:** Added `hjkl_engine::policy::shell_disabled()` guard
> at the top of `qf_run_make`, closing the gap where `:make` bypassed the
> shell-out policy in RPC modes (commit `b89337a1`).

`resolve_make_argv` splits `makeprg` config value by whitespace; the first token
becomes the program executed by `Command::new()`. A `:set makeprg=...` (with no
validation) picks any binary on PATH. By design (vim parity), but the program is
user-configurable with no allowlist.

> **Correction (verified 2026-07-23):** the original claim "Guarded by
> `policy::shell_disabled()` in RPC modes" is **wrong**. `shell_disabled()` has
> exactly three call sites (`:!` in `shell.rs:22`, `:r !` in `builtins.rs`, the
> engine range-filter in `editor.rs`); **none** covers the `:make` path.
> `quickfix.rs:787` runs `Command::new(&program)` unconditionally, so if `:make`
> is reachable in an RPC mode it executes `makeprg` even with shell-out
> "disabled." This makes M1 a genuine gap, not a guarded convenience — worth a
> `shell_disabled()` check on the make/quickfix run path.

**M2 — `git_rev` joined into cache path without `..` sanitization**
`crates/hjkl-bonsai/src/runtime/source.rs:93`

> **Resolved 2026-07-23:** `validate_clone_args` now rejects path separators in
> `git_rev` via `is_safe_component`, and `source_dir` adds a `debug_assert!`
> defense-in-depth check (commit `c9a964dd`).

`self.base.join(format!("{name}-{}", spec.git_rev))` interpolates `git_rev` into
a directory path. `name` is validated by `is_safe_component`, but `git_rev` is
not — a manifest specifying `git_rev = "../../etc"` would create a directory
outside the cache tree. Low practical risk: manifests are pinned by crate
maintainers, not runtime user input, and `validate_clone_args` rejects
empty/leading-dash but not path separators.

**M3 — `.expect()` on external msgpack decode in nvim-api**
`apps/hjkl/src/nvim_api.rs:2563-2565`

> **Confirmed 2026-07-23:** `decode_response` and the test helper at line 2573
> are inside `#[cfg(test)] mod tests` — test-only code, not production dispatch.
> Not a bug.

`decode_response` calls `.expect("decode_response")` on msgpack input from an
external Neovim process. If the external process sends malformed msgpack, this
panics the editor process. The test helper at line 2573 similarly panics on
unexpected response formats. These are in `#[cfg(test)]`-gated code paths, not
production dispatch.

**M4 — TOCTOU between `canonicalize` and atomic rename in save**
`apps/hjkl/src/save.rs:46-93`

> **Resolved 2026-07-23:** Write-permission open on Unix now uses `O_NOFOLLOW` —
> if the target is swapped with a symlink after canonicalize, the open fails
> instead of following the link (commit `414d03d0`). The rename at line 93 is
> already safe (rename doesn't follow symlinks on the destination).

The save path canonicalizes the target to resolve symlinks (line 46), checks
write permissions (line 55), then performs an atomic rename of a temp file to
the target (line 93). Between the canonicalize and the rename, the target file
could be replaced with a symlink to a sensitive file by a concurrent local
process. Partially mitigated: the temp file uses `create_new(true)` with a PID
in the name, making the temp path unpredictable. Inherent to filesystem
operations without `openat`+`renameat`+`O_NOFOLLOW`.

**M5 — `filter_set.lock()` blocks notify event callback thread**
`apps/hjkl/src/app/fs_watch.rs:55-57`

> **Resolved 2026-07-23:** Replaced `lock()` with `try_lock()`, explicitly
> handling `WouldBlock` and `Poisoned` variants without blocking the notify
> thread (commit `bb4a0e10`). Tradeoff (accepted): under lock contention the
> filter now returns `false` and skips that event, so a watch event arriving
> during a set update could be dropped — contention is rare and brief, and not
> blocking the OS event thread is the priority.

The filter closure is called from the `notify` library's raw event thread. Using
`lock()` (blocking) rather than `try_lock()` means the notify thread can be
stalled under lock contention. The `unwrap_or(false)` fallback silently swallows
a poisoned lock, dropping all events until the lock is replaced.

**M6 — `AutoreleasePool: Send` over-approximates thread safety**
`crates/hjkl-clipboard/src/backend/macos.rs:36`

> **Resolved 2026-07-23:** Removed the unsound `unsafe impl Send` — the struct
> is only used as a local RAII guard in synchronous method bodies and does not
> need to be `Send` (commit `de495a2d`).

The struct wraps an `objc_autoreleasePoolPush` token; `Drop` calls `pop`, which
must run on the same thread. The `unsafe impl Send` is technically incorrect: if
the pool were ever sent to another thread, `pop` would operate on the wrong
thread's autorelease stack. In practice the struct is only created and dropped
locally within public method bodies (lines 48–54) and is never actually sent, so
no bug manifests.

**M7 — LSP `command` from user config only — explicit choke point**
`crates/hjkl-lsp/src/config.rs:43-46`

> **Resolved 2026-07-23:** Security comment expanded to document the trust
> boundary and required hardening steps for project-local config. Added
> defense-in-depth `..` path traversal rejection (commit `6dcad196`).

The LSP server binary is taken from user config TOML with no validation beyond
rejecting empty strings. The code explicitly comments this as "trusted user
configuration" and "the single choke point to extend if project-local
(untrusted) config is ever added." Low risk today; would become medium/high if
`.hjkl.toml` project-local config is ever supported.

**M8 — Orphaned join-helper thread in LSP shutdown**
`crates/hjkl-lsp/src/manager.rs:56-61`

> **Resolved 2026-07-23:** Documented as intentional — one detached helper
> thread per shutdown, bounded to once per process lifetime. No safe timed-join
> API exists in stable Rust; the OS reclaims the thread at exit. Doc comment
> added in `shutdown` explaining the design.

`LspManager::shutdown` spawns a helper thread to join the LSP io task with a
2-second `recv_timeout`. If the LSP process hangs indefinitely, the helper
thread is orphaned — it stays alive as a detached OS thread until process exit.
Bounded: one thread per shutdown call, typically once per process lifetime.

**M9 — `:grep` spawns a subprocess unguarded in RPC modes** (resolved)
`apps/hjkl/src/app/quickfix.rs:699,709-715`

> **Resolved 2026-07-23:** Added `shell_disabled()` guard — `:grep` now honors
> the shell-out policy, consistent with `:make` (M1).
>
> **Found 2026-07-23 while reviewing the M1 fix.** The `shell_disabled()` gate
> added for `:make` (M1, commit `b89337a1`) was **not** applied to
> `qf_run_grep`, which forks `rg` / `grep` / `findstr` (lines 709–715). So an
> `--embed` / `--nvim-api` / `--headless` host without `--allow-shell` can still
> cause hjkl to spawn a subprocess via `:grep`, which is inconsistent with the
> shell-out policy.
>
> Severity is **lower than M1**: the program is a fixed binary (never
> user-configurable, unlike `makeprg`), and the pattern is passed with a `--`
> separator (findstr `/c:`), so there is **no arbitrary code execution or
> argument injection** — only an unexpected process spawn.
>
> **Fix options:** (a) gate `qf_run_grep` behind `shell_disabled()` too,
> matching `:make`; or (b) treat `:grep` as an intentional allowance (fixed
> binary, safe args) and document why the shell-disabled contract excludes it.
> Not yet decided.

---

### Low Severity (6)

**L1 — Modelines can set any option including `makeprg`**
`crates/hjkl-app/src/modeline.rs:108-114` (the original
`apps/hjkl/src/modeline.rs` path does not exist)

File content modelines (`vim: set ts=2 makeprg=evil:`) can set arbitrary editor
options via `Options::set_by_name`. Integer values are parsed without range
checking (clamped later by `set_by_name`). Vim parity — the file must already be
opened for this to apply.

**L2 — `unsafe { set_var("PATH") }` at startup** `apps/hjkl/src/main.rs:493-494`

Called before any threads spawn (documented in SAFETY comment). The value is
assembled from the anvil binary directory and the existing PATH, with
deduplication. Sound given the single-threaded call site.

**L3 — TOCTOU between `$DISPLAY` check and `xcb_connect`**
`crates/hjkl-clipboard/src/backend/x11.rs:112-122`

`var_os("DISPLAY")` is checked for presence, then `xcb_connect(NULL, NULL)` is
called. If `$DISPLAY` is unset between the check and the connect, the connection
will use the default display. Low risk — env vars change rarely at runtime.

**L4 — `:grep` pattern injection mitigated by `--` separator**
`apps/hjkl/src/app/quickfix.rs:709-719`

Patterns are passed to `rg`/`grep`/`findstr` with appropriate injection
prevention: rg uses `--` separator, findstr uses `/c:`, grep uses `--`. Safe.

**L5 — `stdin_text` used for `scriptin` on `-s -`**
`apps/hjkl/src/main.rs:672-673`

The dedicated `-` (stdin-as-buffer) and `-s -` paths are separate in the CLI
parser. No path where stdin content is executed directly.

**L6 — `Relaxed` atomic ordering adequate for policy flags**
`crates/hjkl-engine/src/policy.rs:22,27,40,44`

`SHELL_DISABLED` and `FS_RESTRICTED` are monotonic bools set once at startup
before the editor is built. `Relaxed` is correct because: the store happens
before any editor thread spawns, and loads are on paths that are happens-after
via thread synchronization.

---

## Positive Findings

The codebase demonstrates strong defensive security practices:

- **No hardcoded secrets, tokens, or keys** anywhere in the codebase.
- **No weak cryptographic algorithms**: SHA-256 used for TOFU integrity (anvil
  installer); FNV-1a used only for non-crypto collision-tolerant hashing.
- **No `deserialize_any` or dangerous `untagged` enums** at runtime. Serde
  config structs consistently apply `#[serde(deny_unknown_fields)]`.
- **Strong path traversal protections**: `is_safe_component`,
  `is_safe_relative_path`, `validate_relative_path`, `safe_join`, and
  `path_escapes` guards are used consistently across anvil, bonsai, engine, and
  save paths.
- **Command argument injection prevented**: `reject_option_like()` gates all
  package manager command arguments in anvil.
- **RPC modes default to locked down**: `--embed`, `--nvim-api`, and
  `--headless` disable shell-out by default (explicit `--allow-shell` opt-in).
  Filesystem restriction (`restrict_fs()`) is narrower — it applies to `--embed`
  and `--nvim-api` only; `--headless` deliberately keeps full filesystem access
  (`main.rs:533`). (The original report said all three restrict FS; corrected
  2026-07-23.)
- **Allocation caps everywhere except the stdin path**: swap file header (1
  MiB), undo (256 MiB), body (64 MiB); formatter I/O (64 MiB); LSP codec header
  (64 KiB), message body (16 MiB); msgpack-RPC body (256 MiB).
- **No `todo!` macros** anywhere in production code.
- **No `MaybeUninit`, `ManuallyDrop`, or `#[may_dangle]` unsoundness**.
- **Lock poisoning handled consistently**: `PoisonError::into_inner` recovery or
  clean `expect("poisoned")` panic — no silent corruption.
- **Subprocess lifecycle properly managed**: all children reaped; timeout+kill
  patterns; bounded I/O with `wait()`.
- **Swap directory protected**: `0o700` permissions on the swap directory
  prevent other local users from reading unsaved buffer content.
- **`catch_unwind` guards around fallible workers**: grammar loading panics
  don't kill worker threads permanently.
- **No use of `get_unchecked`/`get_unchecked_mut`** anywhere.

---

## Summary

| Severity  | Count  | Resolved                                              |
| --------- | ------ | ----------------------------------------------------- |
| High      | 3      | 2 (H3, H4)                                            |
| Medium    | 9      | 8 (M1, M2, M4, M5, M6, M7, M8, M9) + 1 test-only (M3) |
| Low       | 6      | 0 (by design / low risk)                              |
| **Total** | **18** | **10 fixed + 1 confirmed test-only**                  |

**Resolved 2026-07-23:**

- **H3:** stdin read capped at 256 MiB.
- **H4:** Wayland CMSG fd extraction bounds-checked against `msg_controllen`;
  `send_with_fds` verifies fds fit before copy; 4 edge-case unit tests added.
- **M1:** `:make` now respects `shell_disabled()` policy.
- **M2:** `git_rev` validated for path separators at the boundary +
  `debug_assert!` at join site.
- **M5:** fs-watch notify filter uses `try_lock()` — never blocks the event
  thread.
- **M6:** Unsound `unsafe impl Send` removed from `AutoreleasePool`.
- **M9:** `:grep` now respects `shell_disabled()` policy, consistent with
  `:make`.
- **M4:** Write-permission open on save now uses `O_NOFOLLOW` on Unix to close
  the TOCTOU gap between `canonicalize` and the permission check.
- **M7:** LSP `command` validation expanded with `..` traversal rejection and
  explicit security documentation for when project-local config is added.
- **M8:** Orphaned join-helper thread documented as intentional — bounded to
  once per process lifetime. Doc comment added in `LspManager::shutdown`.
- **M3:** Confirmed test-only (`#[cfg(test)]`) — not production code.
- **H1:** Pruned — intentional unrestricted shell access (vim parity), fully
  guarded in RPC modes. Module-level doc comment added in `shell.rs` explaining
  the design to future auditors.

**Not fixed (by design / tracked / infrastructure):**

- **H2:** Tracked as
  [GitHub issue #314](https://github.com/kryptic-sh/hjkl/issues/314).
- **L1–L6:** Low severity, by design or adequately mitigated.
