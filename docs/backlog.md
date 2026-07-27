# hjkl — review backlog

Single source for open findings, deferred decisions, and closed-with-rationale
calls across every audit and review run on this codebase.

**Consolidated 2026-07-27** from six scattered files, all now deleted (history
in git): `audit-2026-07-26.md`, `round2-2026-07-26.md`, `code-review.md`,
`perf-review.md`, `security-audit.md`, `tidy-report.md`. `embed-rpc.md` stays
separate — it is protocol reference, not review output.

**Every claim below was re-verified against the tree at `73c06b1a`** on
2026-07-27: all 26 cited commit hashes resolve and match their descriptions,
every open finding was re-read at its current location, and the lint-based
claims were re-run rather than trusted. Six claims turned out stale or wrong —
see [§4](#4-corrections-from-the-2026-07-27-verification-pass).

Findings are anchored by **symbol name, not line number**. Every line anchor in
the source documents had drifted (typically 5–90 lines); symbol names survive
refactors.

---

## 1. Open work — ranked

### 1.1 Undo keyframe materialization — the only measured user-facing stall

`crates/hjkl-buffer/src/undo.rs`, tracked as
[#302](https://github.com/kryptic-sh/hjkl/issues/302).

`g-`/`:earlier` reaches a cold node by replaying deltas from the nearest warm
ancestor, and the warm LRU is only `WARM_CAP = 16` deep. Per-jump cost is
**linear in depth**, so a tip→root walk is **quadratic**:

Re-measured 2026-07-27 on the current tree (`single_deep_jump` /
`cold_jump_back`); the original figures reproduced within noise:

| undo depth | one cold `g-` | full tip→root walk |
| ---------- | ------------- | ------------------ |
| 16         | (all warm)    | 77.7 µs            |
| 64         | 23.9 µs       | 612 µs             |
| 256        | 107.6 µs      | 11.45 ms           |
| 1024       | 443 µs        | 201.3 ms           |

`:earlier 9999` on a 1024-deep history costs **~200 ms today and ~3.2 s at
4096**. Keyframes every K nodes bound each jump at O(K) and the walk at O(N).

Bench: `cargo bench -p hjkl-buffer --bench undo` (`cold_jump_back/1024`).

### 1.2 Swap `SerTree.base` duplicates the document

`crates/hjkl-app/src/swap.rs`.

The swap file stores the document roughly **twice** — once streamed as the body,
once as `SerTree.base`. Cost is dominated by that copy, not by node count: a
_single-node_ tree on a 20 000-line doc serializes in ~99 µs while the marginal
per-node cost is ~30–40 ns.

**Do not** implement the append-only delta log that #302 pairs with this — it
attacks node count, which is not the cost, and cannot avoid the base copy or the
`fsync`. De-duplicating `base` against the streamed body is the real target.
Worst measured cell is 457 µs (20 000 lines, 1024 nodes) on a path that fires at
most once per `updatetime` idle gap per dirty slot, so this is low-urgency.

### 1.3 `"\n"` → `""` write-back — needs a vim-parity decision first

All three save paths compute `trailing_nl` as `!body.is_empty()`:

- `apps/hjkl/src/app/ex_dispatch.rs` (`needs_trailing_nl`, TUI `:w`)
- `apps/hjkl/src/headless.rs`
- `apps/hjkl/src/embed.rs`

A buffer holding a single empty line therefore writes a **zero-byte file**
rather than `"\n"`. This is the edge B2 deliberately left alone. One decision
settles all three sites; it is a behavior call, not an implementation task.

### 1.4 Round-2 deferred items

From the DRY/YAGNI/idiom/perf round (2026-07-26). Each was a conscious defer,
not an oversight.

| Item                            | Where                                                           | Why deferred                                                                                                                    |
| ------------------------------- | --------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| Settings/Options full collapse  | `hjkl-engine/src/editor.rs` (see the `0.1.0 (Patch C-δ)` note)  | L-sized; staged for 0.1.0. Round-2 item 1 removed the live data-loss hazard only.                                               |
| P6 per-cell span resolve sweep  | engine span layering                                            | M–L, layering-order-sensitive; needs the sortedness guarantee established first.                                                |
| P10 wrap-mode scrolloff O(h²)   | wrap scroll math                                                | Wrap is not the default; deserves the same care as P6.                                                                          |
| R10 stringly errors → enum      | `hjkl-app/src/git.rs` (`Result<(), String>`)                    | Design decision, not mechanical.                                                                                                |
| R13 `unnecessary_wraps` triage  | dispatch tables                                                 | The uniform-signature shapes are deliberate; needs per-family review.                                                           |
| Y5 `hjkl-editor::spec`          | `crates/hjkl-editor/src/lib.rs`                                 | Needs external-consumer confirmation (sqeel/buffr) before deletion — see [§3.4](#34-published-crates-are-not-workspace-local).  |
| Multicursor `lens` vector       | `hjkl-engine/src/editor.rs` (`buf_line_chars` collect)          | O(buffer) per edit, but gated behind unwired multicursor. Fix when wired.                                                       |
| LSP params doc-text copy        | `hjkl-lsp/src/runtime.rs` (`json!` params in didOpen/didChange) | Round-2 item 5 killed the envelope copy; the params literal still copies once. Needs Map-built params + `Arc::unwrap_or_clone`. |
| Engine-side span install ~24 µs | engine recompute path                                           | Style interning + per-row `buf_line` clones under the content mutex. Next candidate if that path is revisited.                  |

---

## 2. Blocked on platform access

Not effort-blocked — these need hardware or a session this host does not have.
Two of the four original clipboard findings **did not survive verification**;
see §4.

| Finding                                                      | Location                                                                                | Status after verification                                                                                                                                                                                                 |
| ------------------------------------------------------------ | --------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| INCR transfer timeout signalled as completion                | `x11_thread.rs` (`prune_expired_incr_sends`), Wayland equivalent                        | **Still open, needs a live session.** The zero-length terminator is now sent deliberately with a rationale comment, but a truncated transfer is still indistinguishable from a completed one to the requestor.            |
| `SELECTION_NOTIFY` refusal arm ignores which selection       | `x11_thread.rs` (`XCB_SELECTION_NOTIFY` match, refusal arm)                             | **Narrowed.** The success arm matches requestor _and_ property. The refusal arm matches any notify to our window, so an unrelated selection's refusal can be read as ours. Low impact; needs a live X server to exercise. |
| `CString::new(..).expect(..)` panics on NUL in a type string | `hjkl-clipboard/src/backend/macos.rs`                                                   | **Open, needs a Mac.** Confirmed present and unchanged.                                                                                                                                                                   |
| Windows FFI paths never executed                             | `hjkl-fs/src/identity.rs` (`MaybeUninit` FFI), `hjkl-fs/src/dir.rs` (`remove_path_all`) | **Open, needs a Windows host.** Read-consistent; no runtime coverage. (The source doc mis-filed `remove_path_all` under `identity.rs`.)                                                                                   |

Today's CI failure is the argument for closing this tier properly:
platform-gated code is only ever verified by CI, and it went red the moment a
workspace lint touched it. A CI-runner-driven test job would convert most of
this table from "blocked" to "tested".

---

## 3. Open by decision — no work planned

### 3.1 H2 — remote grammar compilation and `dlopen` (issue #314)

`hjkl-bonsai/src/runtime/grammar.rs`, `compile.rs`. Tree-sitter grammars are
downloaded from remote git repos, compiled with `$CC`/`$CXX`, and `dlopen`ed
into the editor process. The manifest pins `git_url`/`git_rev` but performs no
signature or artifact-hash verification.

Verified accurate and **not remotely reachable today** — the manifest is
`include_str!`-bundled, not runtime user input. Architectural; needs a hardening
design (signature/hash pinning) before any code changes.

### 3.2 Accepted low-severity security items

| Item | Claim                                               | Verified status                                                                                                            |
| ---- | --------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| L1   | Modelines can set any option including `makeprg`    | **Wrong — withdrawn.** See §4.1.                                                                                           |
| L2   | `unsafe { set_var("PATH") }` at startup             | Accurate. `apps/hjkl/src/main.rs`, before any thread spawns, SAFETY-documented. Sound.                                     |
| L3   | TOCTOU between `$DISPLAY` check and `xcb_connect`   | Accurate. `x11.rs` bails early when `DISPLAY` is unset to dodge a ~50 ms XCB timeout. Env vars change rarely; low.         |
| L4   | `:grep` pattern injection                           | Accurate — mitigated. `quickfix.rs` passes `--` before the pattern on both the ripgrep and grep paths.                     |
| L5   | `stdin_text` for `-s -` is a separate path from `-` | Accurate. No path executes stdin content; the H3 256 MiB cap is in place.                                                  |
| L6   | `Relaxed` ordering adequate for policy flags        | Accurate. `hjkl-engine/src/policy.rs` — set once before any editor thread spawns; loads are happens-after via thread sync. |

### 3.3 Accepted code-review items

- **M2** — `undo_group_enter`/`exit` re-entrancy (`hjkl-buffer/src/content.rs`).
  Verified present and unchanged: a `u32` depth counter with `saturating_add`,
  committing only on the outermost close. No practical bug under single-threaded
  usage with exclusive borrows; would not survive concurrent access.
  Informational.
- **L1** — `set_cursor` clamps beyond-EOF positions
  (`hjkl-buffer/src/buffer.rs`). Verified: row and col are both clamped and
  `last_cursor` records the clamped value, so cross-session persistence loses
  the intended position for buffers that shrink. Documented best-effort.
- **L2** — `\n` in a substitute replacement inserts `\0`
  (`hjkl-engine/src/substitute.rs`, `Some('n') => out.push('\0')`). Verified
  exactly as described. Vim-compatible per `:h sub-replace-special`; valid in a
  Rust `String` but can confuse terminal display and C-FFI consumers.
- **Mis-named test** — `hjkl-fs/tests/multi_process.rs`,
  `shared_locks_do_not_exclude_each_other` acquires and drops two shared locks
  sequentially in one process, and structurally cannot demonstrate cross-process
  sharing since the in-process layer serializes readers by design. The body now
  carries a comment saying so; the name still overpromises. Rename candidate,
  zero urgency.

### 3.4 Published crates are not workspace-local

`hjkl-css` was deleted as YAGNI on workspace-grep evidence and had to be
reverted — an external project consumes it. Every `crates/*` member publishes to
crates.io on release, so **a grep showing zero consumers is not evidence of zero
consumers**. Crate deletion is a user decision, not an audit conclusion. This
bounds the pre-1.0 no-backcompat policy: that covers formats, APIs, and shims
_within_ the workspace, not the existence of published crates. Deleting dead
code _inside_ a crate remains fine.

---

## 4. Corrections from the 2026-07-27 verification pass

Six documented claims did not survive re-checking. Recorded because a stale
finding costs more than no finding — it sends someone to fix what is already
fixed, or to trust a guarantee that does not hold.

### 4.1 Security L1 was wrong: modelines cannot set `makeprg`

The claim was "modelines can set any option incl. `makeprg` — vim parity". The
opposite is true and deliberately so: `Options` has no `makeprg` field at all,
`parse_token` validates every token through `set_by_name` on a scratch `Options`
and silently drops unknown names, and
`hjkl-app/src/modeline.rs::parse_modeline_rejects_makeprg` asserts it —
explicitly citing the CVE-2019-12735 class. **Withdrawn.**

### 4.2 The Wayland fd-queue desync is not present

Reported as an fd-queue desync in `wayland_thread.rs` needing a live compositor.
Current `dispatch_events` pops the fd queue **only** for `data_source.send`
events (`expects_fd`), with a comment describing precisely the misattribution
the finding predicted. No fix commit exists in the range, so the finding appears
to have been wrong when filed. **Closed, not reproducible.**

### 4.3 Tidy residual is fully stale — zero production hits remain

The claim was ~20 `redundant_clone` hits including one production site at
`lsp_glue.rs:1322`. Re-run workspace-wide with `-W clippy::redundant_clone`
after `cargo clean -p` on the named crates (the first run silently returned
nothing because clippy served a cached result — a lint sweep that reuses a
cached check proves nothing):

**34 hits, every one inside `#[cfg(test)]`.** Verified by comparing each hit's
line against its file's `#[cfg(test)]` boundary: `hjkl-engine` 12, `hjkl-ex` 11,
`hjkl-vim-tui` 3, `hjkl-anvil` 3, `hjkl-buffer` 2, `hjkl-fs-watch` 1,
`hjkl-engine-tui` 1, `hjkl-clipboard` 1. The named production site no longer
exists. **No production redundant clones remain workspace-wide.**

### 4.4 The M1 rationale moved crates

The code-review withdrawal of M1 argued that the save write-permission open at
`save.rs:57-66` is load-bearing because M4 added `O_NOFOLLOW` to it. Still true,
but the probe was extracted: `save_file_durable` now calls
`hjkl_fs::probe_writable_nofollow`, and the flag itself is cfg-gated in exactly
one place, `hjkl-fs/src/open.rs`. The reasoning stands — one non-truncating
`O_NOFOLLOW` write-open both tests writability and closes the swap TOCTOU, and
`std::fs::metadata` would reopen the hole.

### 4.5 The unsafe-primitives claim was overstated

"No `MaybeUninit`/`ManuallyDrop`/`#[may_dangle]` unsoundness" reads as "none
present". Both are present and correct:
`MaybeUninit<BY_HANDLE_FILE_INFORMATION>` in `hjkl-fs/src/identity.rs` (Windows
FFI out-param, the idiomatic use) and `ManuallyDrop` in a `wayland_socket.rs`
test guarding fd ownership. `todo!()` and `get_unchecked` are genuinely absent
workspace-wide.

### 4.6 Line anchors had drifted everywhere

Every `file:line` anchor in the source documents was stale, by 5 lines
(`quickfix.rs`) to ~90 (`hjkl-engine/src/editor.rs`, where the round-2 note
pointed at 1419 and 2423 for content now at 1507 and 2530). Symbol-name anchors
are used throughout this document instead.

---

## 5. Closed, with rationale worth keeping

Do not re-litigate these without new evidence — each was investigated and
rejected for a stated reason.

- **P4 statusline diag memoization — WONTFIX.** No representation is both cheap
  and invalidation-safe. Precomputing at the write sites (`lsp_glue.rs` assign,
  `buffer_ops.rs` clear) leaves any other writer producing a stale statusline; a
  per-frame fingerprint is itself an O(n) pass over `lsp_diags`, the same cost
  as the count loop it replaces. The block already early-returns empty when
  there are no diagnostics, so the common case is free.
- **P8-C borrow-from-rope — rejected, do not retry.** Borrowing each line via
  `RopeSlice::as_str()` with an owned fallback gave short lines −4–5% but
  **regressed long/multi-chunk lines +3.1%**, because `as_str()` returns `None`
  there and the chunk probe is pure overhead. The shipped fix (P8-D, buffer
  reuse via thread-local `LineScratch`) wins because it removes the allocation
  without adding a probe.
- **Code-review H1 — withdrawn.** `File::create` = `open(O_CREAT|O_TRUNC)`; on
  an existing file the mode argument is ignored, so a `0755` script keeps
  `0755`. Umask applies only to genuinely new files, which is correct.
- **Tidy #6 — withdrawn.** The `#[allow(unused_imports)]` in
  `apps/hjkl/src/menu.rs` is load-bearing: the bin-only crate re-exports
  `MenuItem` but uses it only under `#[cfg(test)]`, so the allow is what keeps
  `-D warnings` green.
- **Y1 delete `hjkl-css` — cancelled.** External consumer; see §3.4.
- **Y2 remainder — keep.** `guard_not_swapped`, `hardlink_count`,
  `owner_only_options`, `read_capped_from`, `private_state_subdir` all verified
  present in `hjkl-fs`; shipped for issues #315/#317/#318 with consumers
  pending.
- **Y5 `AppConfig` trait with one implementor — keep.** Indirection is cheap.

---

## 6. Shipped index

### 6.1 Earlier review rounds (2026-07-23, pruned 2026-07-25)

Performance — all ranked hotspots shipped:

| Was | Fix                                                                 | Commit     |
| --- | ------------------------------------------------------------------- | ---------- |
| P1  | `HighlightSpan.capture` + `capture_names` → `Arc<str>`              | `7393ad29` |
| P2  | per-row `LineCache` in word motions                                 | `af5ebd8b` |
| P3  | `iskeyword` pre-parsed once via `KeywordSpec`                       | `2d37a385` |
| P5  | `evict_stale` uses `HashSet`                                        | `b0dcdfd4` |
| P6  | prebuilt capture-name→index `HashMap`                               | `b0dcdfd4` |
| P7  | hlsearch painter consults engine per-row cache                      | `0a83b354` |
| P8  | line-prefetch `String` buffers reused across frames (−2.9% / −7.9%) | `35b4c61c` |
| P9  | which-key `chord_to_notation(&[KeyEvent])` — no Vec clone           | `7af516a7` |
| P10 | `HighlightSpan.metadata` → `Option<Box<HashMap>>` (48B → 8B)        | `66617a18` |
| P11 | one redundant `Range` clone removed (other three load-bearing)      | `b0dcdfd4` |

Security — 10 fixed, 1 confirmed test-only:

| Was | Fix                                                                | Commit                 |
| --- | ------------------------------------------------------------------ | ---------------------- |
| H3  | stdin read capped at 256 MiB                                       | `350ff8d8`             |
| H4  | Wayland CMSG fd extraction bounds-checked; send verifies fds fit   | `1c9f4314`, `8fdeafd7` |
| M1  | `:make` honors `shell_disabled()` in RPC modes                     | `b89337a1`             |
| M2  | `git_rev` rejects path separators + `debug_assert!` at join        | `c9a964dd`             |
| M4  | save write-perm open uses `O_NOFOLLOW` (canonicalize→open TOCTOU)  | `414d03d0`             |
| M5  | fs-watch notify filter uses `try_lock()`                           | `bb4a0e10`             |
| M6  | unsound `unsafe impl Send` removed from `AutoreleasePool`          | `de495a2d`             |
| M7  | LSP `command` validation + `..` traversal rejection                | `6dcad196`             |
| M8  | orphaned LSP join-helper thread documented as intentional          | doc-only               |
| M9  | `:grep` honors `shell_disabled()`                                  | (with M1)              |
| M3  | confirmed test-only (`#[cfg(test)]` `decode_response`) — not a bug | —                      |

Tidy — 8 fixed, 1 withdrawn: `7af516a7`, `b0dcdfd4`, `5d19681e`, `c323f553`.

### 6.2 Whole-workspace audit (2026-07-26) — 14 bugs, 6 DRY, 5 YAGNI

All 20 tracked items landed in v0.37.1 (`b1d421d7`..`6b30d797`); Y1 cancelled.
Four implementation defects were caught in review afterward and fixed one per
commit:

- `38379648` **B4 follow-up** — the whole-buffer linewise-delete inverse added a
  phantom trailing newline. The implementation had discovered this and excluded
  it from the property test behind a comment falsely claiming the plan deferred
  it.
- `c0fec96e` **B5 follow-up** — nothing intercepted `:r` at the app layer,
  contradicting the comment the B5 commit added. `read_handler` now resolves
  through `hjkl_fs::resolve_under` under policy, with an isolated test binary
  (`hjkl-ex/tests/fs_policy.rs`) proving the symlink escape is refused. Publish
  order in `ci.yml` already satisfies the new dependency (`hjkl-fs` at :887
  before `hjkl-ex` at :910 — re-verified).
- `dfed035f` **D1 follow-up** — the chord adapter hardcoded `shift: false`, so
  `:nmap <S-Tab> x` stored plain Tab and fired on plain Tab.
- `db5f03b1` **D6 follow-up** — the extracted helper used `remove_dir_all`,
  which fails `NotADirectory` on `compile.rs`'s plain-file staging, leaking
  staging files on the error path.

Plus `1d3937e9` (surface `:map` parse errors, previously silent) and `ce1624b0`
(the required B7 regression test, revert-verified, plus a vacuous assertion
fixed).

### 6.3 Round 2 — DRY / YAGNI / idiom / perf (2026-07-26)

24 items, one commit each, reviewed after landing. Unreleased as of `73c06b1a`.

| #   | Item                                                         | Commit     |
| --- | ------------------------------------------------------------ | ---------- |
| 1   | `to_options` lossy backfill + `cursorline` default           | `3c317109` |
| 2   | `display_keys` = `encode_macro` copy — deleted               | `1c3160ba` |
| 3   | Visual-exit mark math deduped                                | `d89d6fa8` |
| 4   | `begin_insert` / `begin_insert_noundo` twins                 | `45d15f83` |
| 5   | LSP `json!` deep-clone per send (42.5 µs → 0.29 µs)          | `b59ff486` |
| 6   | Undo `diff()` chunk-walk (1.55 ms → ~30 µs)                  | `b9b88501` |
| 7   | `search_count` rescan per cursor move (~500–1750×)           | `c16868d2` |
| 8   | Picker preview reparse per frame (~2500×)                    | `c4c524d8` |
| 9   | Diff mode O(diff) → O(viewport) (7.3 ms → 41 µs)             | `27d4c60c` |
| 10  | Fold API lock+clone per query (−55% `j`/`k`)                 | `b5a2703e` |
| 11  | Viewport span table copied 3× per recompute                  | `6389f02b` |
| 12  | Explorer per-frame O(N) → dirty_gen caches (144 µs → 0.3 µs) | `c76c855f` |
| 13  | Small per-event alloc cleanups                               | `9b19ef91` |
| 14  | Style conversion consolidation (fixes Indexed→black drift)   | `00ef1f7c` |
| 15  | statusline-tui / theme-tui converter dedupe                  | `8fb131ed` |
| 16  | `lsp_glue` ext table → hjkl-lang registry (~840 exts)        | `4144595c` |
| 17  | `BufferSlot` constructor for 11 literal sites                | `ddc825a7` |
| 18  | YAGNI deletions (−99 lines)                                  | `39f09f5d` |
| 19  | Test-helper quartet; char-boundary snapping                  | `b833e68e` |
| 20  | Idiom smalls + deny lints                                    | `6ac4235b` |
| 21  | Idiom mechanical batches + warn lints                        | `ba1996fb` |
| 22  | Invariant unwraps → `expect` with message                    | `069de402` |
| 23  | `needless_pass_by_value` genuine cases                       | `ad27c04d` |
| 24  | Reviewed `redundant_clone` sites                             | `d3280ffb` |
| —   | Follow-up: platform-gated lint failures (§7.2)               | `0d08f60c` |

Correctness bugs found incidentally while doing the above: `current_options()`
silently reset 26 of 50 options on any nvim-API read-modify-apply;
`:later <time>` sampled two different clock instants; new languages never
attached LSP servers; Indexed colors flattened to black in one of two adapters.

---

## 7. Process

### 7.1 Gates

Per item, before commit:
`cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`cargo fmt --all`, `cargo nextest run` (plus `--test e2e` when the app layer is
touched), and the nvim compat oracle **ALL-pass**. The oracle corpus may grow;
it is never adjusted to make a change pass. Perf items must show before/after
bench numbers. Push at phase boundaries with a CI check between.

### 7.2 The local gate is host-only

Round 2 added eight workspace lints and validated them on Linux only. Both
`redundant_pub_crate` (on `MacosBackend`/`WindowsBackend`) and `map_unwrap_or`
(in the windows-only `copy_symlink`) fired on the macOS and Windows clippy jobs
after phase E was already pushed — and the first failure masked the second,
since `hjkl-clipboard` failing to compile skips every dependent crate.

**Any lint addition needs a CI round-trip before it counts as clean.** Partial
local coverage: `cargo clippy -p <crate> --target x86_64-apple-darwin` (and
`x86_64-pc-windows-msvc`) works for crates whose dependency tree has no C build
scripts — `hjkl-fs`, `hjkl-clipboard`, and `hjkl-lsp` all lint clean on both.
Anything pulling tree-sitter, mimalloc, or aws-lc-sys cannot cross-compile from
Linux, so those platform blocks are review-only until CI runs.

### 7.3 Benchmarks

| Bench                               | Measures                                                       |
| ----------------------------------- | -------------------------------------------------------------- |
| `hjkl-buffer-tui/benches/render.rs` | viewport render, short vs long (multi-chunk) lines             |
| `hjkl-buffer/benches/undo.rs`       | cold `g-` jump cost vs undo depth                              |
| `hjkl-buffer/benches/budgets.rs`    | per-operation budget guards                                    |
| `hjkl-app/benches/swap.rs`          | full swap write + undo-section serialization vs size and depth |

Round-2 baselines (criterion, ±4% noise): render 41.2 / 98.6 µs,
`insert_char_1MB` 23.4 µs, `swap write_full/large_d1024` 701 µs. The round-2
note also recorded `undo cold_jump_back/1024` at 277 ms; re-running it on
2026-07-27 gives **201.3 ms**, so treat that one baseline as superseded rather
than as a 27% regression budget.

### 7.4 Standing traps

- **Char vs byte vs grapheme columns** — the repo's documented silent-corruption
  trap. All rope math must preserve exact units.
- **Never assume an `Edit`'s semantics** — read them. Pushing an `Edit`'s
  inverse back through `apply_edit` must restore the previous state exactly (B4
  broke this twice).
- **`hjkl_driver` cannot replay `:` keys**, and `cargo test -p hjkl` skips the
  `--test e2e` binary.
