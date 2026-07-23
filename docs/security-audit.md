# Security Audit Report

**Project:** hjkl (terminal text editor)
**Date:** 2026-07-23
**Version:** 0.35.0
**Depth:** high

---

## Findings

### High Severity (4)

**H1 — `:!cmd` passes unsanitized user input to `sh -c`**
`crates/hjkl-ex/src/shell.rs:32,80`

The `:!cmd` and `:[range]!cmd` ex commands pass the user-typed string directly
to `Command::new("sh").arg("-c").arg(cmd)`. This is the single largest attack
surface: any command the terminal user types executes as their local user.
This is by design (full vim parity), but means `:!` is unrestricted shell
access. In `--embed`, `--nvim-api`, and `--headless` modes, shell-out is
disabled by default via `policy::disable_shell()` and gated behind an explicit
`--allow-shell` flag. In TUI mode it is always available. No shell
metacharacter filtering is applied.

**H2 — `dlopen` of remotely-compiled shared objects (arbitrary code execution)**
`crates/hjkl-bonsai/src/runtime/grammar.rs:79-89`, `compile.rs:136,163-171`

Tree-sitter grammars are downloaded from remote git repositories, compiled with
a compiler resolved from `$CC`/`$CXX` (or `cc`/`c++` from PATH), and
`dlopen`ed into the editor process. This is a documented trust boundary:
arbitrary native code execution from the grammar source, compiler, and build
chain. The manifest pins `git_url`/`git_rev` but performs no signature
verification or hash pinning of the compiled artifact. The `validate_clone_args`
function rejects empty strings and leading dashes but does not reject path
separators in `git_rev` values.

**H3 — Unbounded stdin read with `hjkl -`**
`apps/hjkl/src/main.rs:672-673`

The `-` flag reads stdin to EOF with `read_to_string` and no size cap. If
stdin is connected to `/dev/zero`, a named FIFO that never closes, or a
malicious source, this allocates until OOM. Every other read path in the
codebase (swap files, formatter I/O, LSP codec, msgpack-RPC) has explicit caps
(64 MiB, 256 MiB, etc.).

**H4 — ~30 unsafe FFI blocks in Wayland clipboard socket I/O**
`crates/hjkl-clipboard/src/backend/wayland_socket.rs:113-398`

Raw `libc::socket`, `libc::sendmsg`, `libc::recvmsg`, `libc::close`,
`libc::getuid`, and `libc::CMSG_*` macro expansion for Wayland data-device
communication. Each block has SAFETY comments, but the cmsg parsing and raw fd
handling is the highest-risk FFI surface in the codebase. A malicious Wayland
compositor sending malformed cmsg data could trigger undefined behavior.

---

### Medium Severity (8)

**M1 — `:make` runs user-configured `makeprg` as command**
`apps/hjkl/src/app/quickfix.rs:781,787`

`resolve_make_argv` splits `makeprg` config value by whitespace; the first
token becomes the program executed by `Command::new()`. A `:set makeprg=...`
(with no validation) picks any binary on PATH. By design (vim parity), but the
program is user-configurable with no allowlist. Guarded by `policy::shell_disabled()`
in RPC modes.

**M2 — `git_rev` joined into cache path without `..` sanitization**
`crates/hjkl-bonsai/src/runtime/source.rs:93`

`self.base.join(format!("{name}-{}", spec.git_rev))` interpolates `git_rev`
into a directory path. `name` is validated by `is_safe_component`, but
`git_rev` is not — a manifest specifying `git_rev = "../../etc"` would create
a directory outside the cache tree. Low practical risk: manifests are pinned by
crate maintainers, not runtime user input, and `validate_clone_args` rejects
empty/leading-dash but not path separators.

**M3 — `.expect()` on external msgpack decode in nvim-api**
`apps/hjkl/src/nvim_api.rs:2563-2565`

`decode_response` calls `.expect("decode_response")` on msgpack input from an
external Neovim process. If the external process sends malformed msgpack, this
panics the editor process. The test helper at line 2573 similarly panics on
unexpected response formats. These are in `#[cfg(test)]`-gated code paths, not
production dispatch.

**M4 — TOCTOU between `canonicalize` and atomic rename in save**
`apps/hjkl/src/save.rs:46-93`

The save path canonicalizes the target to resolve symlinks (line 46), checks
write permissions (line 55), then performs an atomic rename of a temp file to
the target (line 93). Between the canonicalize and the rename, the target file
could be replaced with a symlink to a sensitive file by a concurrent local
process. Partially mitigated: the temp file uses `create_new(true)` with a PID
in the name, making the temp path unpredictable. Inherent to filesystem
operations without `openat`+`renameat`+`O_NOFOLLOW`.

**M5 — `filter_set.lock()` blocks notify event callback thread**
`apps/hjkl/src/app/fs_watch.rs:55-57`

The filter closure is called from the `notify` library's raw event thread.
Using `lock()` (blocking) rather than `try_lock()` means the notify thread can
be stalled under lock contention. The `unwrap_or(false)` fallback silently
swallows a poisoned lock, dropping all events until the lock is replaced.

**M6 — `AutoreleasePool: Send` over-approximates thread safety**
`crates/hjkl-clipboard/src/backend/macos.rs:36`

The struct wraps an `objc_autoreleasePoolPush` token; `Drop` calls `pop`, which
must run on the same thread. The `unsafe impl Send` is technically incorrect:
if the pool were ever sent to another thread, `pop` would operate on the wrong
thread's autorelease stack. In practice the struct is only created and dropped
locally within public method bodies (lines 48–54) and is never actually sent,
so no bug manifests.

**M7 — LSP `command` from user config only — explicit choke point**
`crates/hjkl-lsp/src/config.rs:43-46`

The LSP server binary is taken from user config TOML with no validation beyond
rejecting empty strings. The code explicitly comments this as "trusted user
configuration" and "the single choke point to extend if project-local
(untrusted) config is ever added." Low risk today; would become medium/high if
`.hjkl.toml` project-local config is ever supported.

**M8 — Orphaned join-helper thread in LSP shutdown**
`crates/hjkl-lsp/src/manager.rs:56-61`

`LspManager::shutdown` spawns a helper thread to join the LSP io task with a
2-second `recv_timeout`. If the LSP process hangs indefinitely, the helper
thread is orphaned — it stays alive as a detached OS thread until process exit.
Bounded: one thread per shutdown call, typically once per process lifetime.

---

### Low Severity (6)

**L1 — Modelines can set any option including `makeprg`**
`apps/hjkl/src/modeline.rs:108-114`

File content modelines (`vim: set ts=2 makeprg=evil:`) can set arbitrary
editor options via `Options::set_by_name`. Integer values are parsed without
range checking (clamped later by `set_by_name`). Vim parity — the file must
already be opened for this to apply.

**L2 — `unsafe { set_var("PATH") }` at startup**
`apps/hjkl/src/main.rs:493-494`

Called before any threads spawn (documented in SAFETY comment). The value is
assembled from the anvil binary directory and the existing PATH, with
deduplication. Sound given the single-threaded call site.

**L3 — TOCTOU between `$DISPLAY` check and `xcb_connect`**
`crates/hjkl-clipboard/src/backend/x11.rs:112-122`

`var_os("DISPLAY")` is checked for presence, then `xcb_connect(NULL, NULL)` is
called. If `$DISPLAY` is unset between the check and the connect, the
connection will use the default display. Low risk — env vars change rarely at
runtime.

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
before any editor thread spawns, and loads are on paths that are
happens-after via thread synchronization.

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
  `--headless` disable shell-out and restrict filesystem access by default,
  with explicit `--allow-shell` opt-in.
- **Allocation caps everywhere except the stdin path**: swap file header (1
  MiB), undo (256 MiB), body (64 MiB); formatter I/O (64 MiB); LSP codec header
  (64 KiB), message body (16 MiB); msgpack-RPC body (256 MiB).
- **No `todo!` macros** anywhere in production code.
- **No `MaybeUninit`, `ManuallyDrop`, or `#[may_dangle]` unsoundness**.
- **Lock poisoning handled consistently**: `PoisonError::into_inner` recovery
  or clean `expect("poisoned")` panic — no silent corruption.
- **Subprocess lifecycle properly managed**: all children reaped; timeout+kill
  patterns; bounded I/O with `wait()`.
- **Swap directory protected**: `0o700` permissions on the swap directory
  prevent other local users from reading unsaved buffer content.
- **`catch_unwind` guards around fallible workers**: grammar loading panics
  don't kill worker threads permanently.
- **No use of `get_unchecked`/`get_unchecked_mut`** anywhere.

---

## Summary

| Severity | Count |
|----------|-------|
| High     | 4     |
| Medium   | 8     |
| Low      | 6     |
| **Total** | **18** |

**Overall risk: Medium.** The codebase is well-structured for a local terminal
application. The `:!cmd` shell-out path is the dominant attack surface — it is
by design, fully guarded in RPC modes, and inherent to the vim-parity model.
The grammar compilation/dlopen pipeline is a documented trust boundary with a
wide surface (network, build chain, shared library loading) that warrants
continuous scrutiny. The unbounded stdin read is the only finding that could
produce a crash from external input and is the simplest to fix.

**Top 3 to fix first:**
1. **Cap stdin read** (`apps/hjkl/src/main.rs:672`) — add a bounded `take(N)`
   before `read_to_string`.
2. **Audit Wayland cmsg parsing** (`wayland_socket.rs`) — review unsafe
   invariants for the cmsg walk and fd extraction paths; add fuzz coverage.
3. **Validate `git_rev` for path separators** (`source.rs:93`) — reject `..`
   and `/` in `git_rev` like `name` already does.
