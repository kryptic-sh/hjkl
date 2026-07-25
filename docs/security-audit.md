# Security Audit Report

**Project:** hjkl (terminal text editor) **Date:** 2026-07-23 (pruned
2026-07-25) **Version:** 0.35.0 **Depth:** high

Of 18 original findings, 10 were fixed and 1 confirmed test-only (pruned below).
Remaining: **H2** (tracked externally) and the low-severity by-design set
**L1–L6**.

## Resolved (pruned)

Full finding text removed once resolved — see the commit.

| Was | Fix                                                                                                     | Commit                 |
| --- | ------------------------------------------------------------------------------------------------------- | ---------------------- |
| H3  | stdin read (`hjkl -`) capped at 256 MiB before `read_to_string`                                         | `350ff8d8`             |
| H4  | Wayland CMSG fd extraction bounds-checked vs `msg_controllen`; send verifies fds fit; 4 edge-case tests | `1c9f4314`, `8fdeafd7` |
| M1  | `:make` now honors `shell_disabled()` policy in RPC modes                                               | `b89337a1`             |
| M2  | `git_rev` rejects path separators (`is_safe_component`) + `debug_assert!` at join                       | `c9a964dd`             |
| M4  | save write-perm open uses `O_NOFOLLOW` on Unix (canonicalize→open TOCTOU)                               | `414d03d0`             |
| M5  | fs-watch notify filter uses `try_lock()` — never blocks the event thread                                | `bb4a0e10`             |
| M6  | unsound `unsafe impl Send` removed from `AutoreleasePool`                                               | `de495a2d`             |
| M7  | LSP `command` validation + `..` traversal rejection + trust-boundary doc                                | `6dcad196`             |
| M8  | orphaned LSP join-helper thread documented as intentional (bounded once/process)                        | doc-only               |
| M9  | `:grep` now honors `shell_disabled()`, consistent with `:make`                                          | (with M1)              |
| M3  | confirmed **test-only** (`#[cfg(test)]` `decode_response`) — not production                             | not a bug              |

---

## Open findings

### High — H2: `dlopen` of remotely-compiled grammars (arbitrary code execution)

**`crates/hjkl-bonsai/src/runtime/grammar.rs:79-89`, `compile.rs:136,163-171`**

Tree-sitter grammars are downloaded from remote git repos, compiled with
`$CC`/`$CXX` (or `cc`/`c++` from PATH), and `dlopen`ed into the editor process —
a documented trust boundary allowing native code execution from grammar source,
compiler, and build chain. The manifest pins `git_url`/`git_rev` but does no
signature or artifact hash verification.

> Tracked as [GitHub issue #314](https://github.com/kryptic-sh/hjkl/issues/314).
> Verified accurate; **not remotely reachable today** — the manifest is
> `include_str!`-bundled, not runtime user input. Architectural; needs a
> hardening design (signature/hash pinning), so left open under the tracker.

### Low (by design / adequately mitigated)

- **L1** — Modelines can set any option incl. `makeprg`
  (`crates/hjkl-app/src/modeline.rs:108-114`). Vim parity; file must already be
  open.
- **L2** — `unsafe { set_var("PATH") }` at startup
  (`apps/hjkl/src/main.rs:493-494`). Called before any thread spawns
  (SAFETY-documented); sound.
- **L3** — TOCTOU between `$DISPLAY` check and `xcb_connect`
  (`crates/hjkl-clipboard/src/backend/x11.rs:112-122`). Env vars change rarely;
  low.
- **L4** — `:grep` pattern injection mitigated by `--`/`/c:` separators
  (`apps/hjkl/src/app/quickfix.rs:709-719`). Safe.
- **L5** — `stdin_text` for `-s -` is a separate path from `-`
  (stdin-as-buffer); no path executes stdin content directly
  (`apps/hjkl/src/main.rs:672-673`).
- **L6** — `Relaxed` atomic ordering adequate for policy flags
  (`crates/hjkl-engine/src/policy.rs:22,27,40,44`). Set once before any editor
  thread spawns; loads are happens-after via thread sync.

---

## Positive Findings

The codebase demonstrates strong defensive security practices:

- **No hardcoded secrets, tokens, or keys** anywhere.
- **No weak crypto**: SHA-256 for TOFU integrity (anvil installer); FNV-1a only
  for non-crypto collision-tolerant hashing.
- **No `deserialize_any`/dangerous `untagged`** at runtime; config structs apply
  `#[serde(deny_unknown_fields)]`.
- **Strong path-traversal protections**: `is_safe_component`,
  `is_safe_relative_path`, `validate_relative_path`, `safe_join`, `path_escapes`
  used consistently across anvil, bonsai, engine, save.
- **Command argument injection prevented**: `reject_option_like()` gates package
  manager args in anvil.
- **RPC modes locked down by default**: `--embed`/`--nvim-api`/`--headless`
  disable shell-out (explicit `--allow-shell` opt-in). `restrict_fs()` applies
  to `--embed`/`--nvim-api`; `--headless` deliberately keeps full FS access.
- **Allocation caps everywhere**: swap header (1 MiB), undo (256 MiB), body (64
  MiB); formatter I/O (64 MiB); LSP codec header (64 KiB), body (16 MiB);
  msgpack-RPC body (256 MiB); stdin (256 MiB, added for H3).
- **No `todo!`** in production; **no
  `MaybeUninit`/`ManuallyDrop`/`#[may_dangle]` unsoundness**; **no
  `get_unchecked`**.
- **Lock poisoning handled consistently**: `PoisonError::into_inner` recovery or
  clean `expect("poisoned")` — no silent corruption.
- **Subprocess lifecycle properly managed**: children reaped; timeout+kill;
  bounded I/O with `wait()`.
- **Swap directory `0o700`**: other local users can't read unsaved buffer
  content.
- **`catch_unwind` guards** around fallible workers (grammar loading).
