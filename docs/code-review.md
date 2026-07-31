# hjkl — full-codebase code review (2026-08-01)

**Scope.** Whole workspace at `main` (v0.39.1, tree clean): 60 `hjkl-*` crates
plus `apps/hjkl`, ~230 000 lines of Rust across 445 files.

**Method.** Read-only. Structural inventory (LOC per crate, panic density in
non-test code, `unsafe` sites, process spawns, filesystem mutation sites),
followed by direct reading of the risk-bearing modules. Every finding below
cites a file and was traced by hand in the source; none is a lint hit.

**Deliberate bias.** The 2026-07-29 full-codebase review in `docs/backlog.md`
covered `hjkl-vim`, `hjkl-engine`, and `hjkl-buffer` core and listed its own
uncovered areas: `apps/hjkl`, `hjkl-lsp`, `hjkl-ex`, `hjkl-editor(-tui)`,
`hjkl-completion`, `hjkl-prompt`, `hjkl-menu`, `hjkl-picker`, and the remaining
non-core crates. This pass targets exactly those, plus `hjkl-anvil`, `hjkl-app`,
`hjkl-fs`, `hjkl-clipboard`, and `hjkl-bonsai`. Findings already tracked in
`backlog.md` are **not** restated — see
[Already tracked](#already-tracked-not-re-reported).

**Overall.** The codebase is in unusually good shape for its size. The
filesystem seam (`hjkl-fs`), the save path (`apps/hjkl/src/save.rs`), the LSP
framing codec, the OSC 52 cap, and the anvil archive extractor are all correctly
hardened, documented with their threat model, and regression-tested. The
findings below are real but none is a critical break; the two worth doing first
are C1 (silent data-loss shape in `git.rs`) and C2 (duplicate-file
`WorkspaceEdit` corruption).

---

## 1. Correctness

### C1. `git.rs` substitutes an empty pathspec for a non-UTF-8 path

`crates/hjkl-app/src/git.rs:681,691,701`

```rust
pub fn discard_path(root: &Path, path: &Path) -> Result<(), String> {
    run_git_cmd(root, &["checkout", "--", path.to_str().unwrap_or("")])
}
```

`stage_path`, `unstage_path`, and `discard_path` all convert `&Path` to `&str`
and fall back to `""` when the conversion fails. A path that is not valid UTF-8
therefore does not error — it becomes `git checkout -- ""`.

An empty pathspec is not a no-op in git's history: older git treats it as "match
everything", which turns "discard changes to this one file" into "discard the
entire worktree". Git 2.16+ rejects it with
`fatal: empty string is not a valid pathspec`, so on current git the user gets a
confusing error instead of a destructive action — but the code should not be
relying on the git version to be safe.

```
Repro: explorer over a file whose name contains invalid UTF-8 bytes
       (e.g. b"caf\xe9.txt" on Linux) → `discard`
       → run_git_cmd(root, ["checkout", "--", ""])
Expect: an error naming the file, no git invocation
Actual: git invoked with an empty pathspec
```

**Fix.** `std::process::Command::arg` takes `AsRef<OsStr>`; pass `path` directly
and drop the lossy conversion. Change `run_git_cmd` to take `&[&OsStr]` (or
build the `Command` per call site).

### C2. `apply_workspace_edit` mis-applies two edit groups targeting the same file

`apps/hjkl/src/app/lsp_glue.rs:2060-2194`

`file_edits` is a `Vec<(Url, Vec<TextEdit>)>`. Each entry is sorted
end-descending and applied independently, and the rope is re-read per edit
inside the group. That is correct **within** one group and only within one
group: the end-descending order is what keeps earlier-applied edits from
shifting later ones.

`documentChanges` is an array, and the LSP spec permits more than one
`TextDocumentEdit` for the same document. When two entries name the same file,
the second group's ranges are computed against the original document but applied
to a rope the first group already mutated.

```
Repro: server returns documentChanges = [
         { textDocument: a.rs, edits: [{ range 5:0-5:3, "XXX" }] },
         { textDocument: a.rs, edits: [{ range 2:0-2:1, "Y" }] } ]
       → group 1 applies at row 5, group 2 applies at row 2 against the
         already-edited rope
Expect: both edits placed as the server computed them
Actual: correct only when the groups happen not to interact; a group
        ordering that puts an earlier range first shifts every later group
```

The same shape occurs if `changes` and `document_changes` both existed — the
`else if` makes that unreachable, which is correct.

**Fix.** Merge `file_edits` by resolved path before applying: group by
`PathBuf`, concatenate the `TextEdit` vectors, then sort once. The existing
end-descending sort then covers the whole file. As a side effect this also fixes
the returned `count`, which currently counts entries rather than distinct files.

### C3. Modeline drops every bare boolean option whose name starts with `no`

`crates/hjkl-app/src/modeline.rs:117-119`

```rust
} else if let Some(bare) = token.strip_prefix("no") {
    // nokey → Bool(false)
    (bare, OptionValue::Bool(false))
```

The `no` prefix is stripped before the name is validated, and there is no
fallback to trying the token whole. `number` is a real option
(`hjkl-engine/src/types.rs:641`, `"number" | "nu"`), and it starts with `no`.

```
Repro: file containing `# vim: number`
       → strip_prefix("no") → "umber"
       → set_by_name("umber", Bool(false)) errors → parse_token returns None
Expect: 'number' enabled for this buffer
Actual: the option is silently dropped
```

`nonumber` still works (strips to `number`, `false`), so the bug is invisible
unless someone writes the positive form. `:set number` is unaffected — this is
the modeline path only.

**Fix.** Try the whole token first; only fall back to the `no`-stripped form
when the full name is unknown to `set_by_name`.

### C4. Modeline does not stop at the terminating colon

`crates/hjkl-app/src/modeline.rs:78-88`

Vim ends a modeline at the first `:` after the options; everything past it is
comment text. `parse_line` splits the body on whitespace, strips a trailing
colon from **every** token, and only breaks when a token is empty after the
strip — so the trailing comment keeps getting parsed.

```
Repro: `/* vim: set ts=2: list of pending items */`
       → tokens: "ts=2" (colon stripped, loop continues), "list", "of", …
       → "list" is a valid option name → 'list' is turned on
Expect: only ts=2 applied; the rest is comment text
Actual: any comment word that collides with an option name is applied
```

`list`, `wrap`, `number`, and `expandtab` are all plausible words in a trailing
comment.

**Fix.** Break out of the token loop after processing a token that _had_ a
trailing colon, not only when the stripped token is empty.

### C5. `listchars` measures character width with a private approximation

`crates/hjkl-buffer/src/listchars.rs:246-253`

```rust
fn unicode_width(ch: char) -> usize {
    // Use a simple approximation: CJK wide = 2, everything else = 1.
    // This avoids adding unicode-width as a direct dep here; buffer-tui
    // uses the real UnicodeWidthChar for rendering.
    if is_wide(ch) { 2 } else { 1 }
}
```

The stated reason is not true: `hjkl-buffer` already depends on `unicode-width`
(`crates/hjkl-buffer/Cargo.toml:17`) and its sibling module `wrap.rs:6` imports
`UnicodeWidthChar`. So one module of one crate approximates while the module
next to it, and the renderer that consumes the result
(`hjkl-buffer-tui/src/render.rs:467`), use the real table.

The approximation is wrong in both directions:

- Emoji (U+1F300–U+1FAFF) are width 2 in `unicode-width` and are not in
  `is_wide`'s range list → counted as 1.
- Combining marks (U+0300–U+036F) are width 0 → counted as 1.
- `UnicodeWidthChar::width` returns `None` for control characters (treated as 0
  by the renderer) → counted as 1 here.

Because `col` drives trailing-space detection and the `eol` marker placement,
`:set list` on a line containing an emoji or a combining mark produces listchars
output whose column accounting disagrees with the renderer's.

**Fix.** Delete `unicode_width`/`is_wide` and use
`UnicodeWidthChar::width(ch) .unwrap_or(0)`, matching `wrap.rs` and `render.rs`.

### C6. `errorformat` silently never matches a real vim `&errorformat`

`crates/hjkl-quickfix/src/errorformat.rs:74-78`

```rust
Some(other) => {
    // Unknown specifier: treat literally.
    re_src.push_str(&regex::escape(&other.to_string()));
}
```

Real-world `errorformat` values are dominated by the multi-line and qualifier
specifiers: `%E`, `%W`, `%C`, `%Z`, `%+`, `%-G`, `%*[^ ]`, `%#`. Every one of
them lands in the `other` arm and is compiled as a **literal** — so the pattern
still compiles, still runs, and never matches anything.

```
Repro: :set errorformat=%E%f:%l:\ error:\ %m,%C%m
       → "%E" compiles to the literal "E" at the start of the pattern
       → no input line ever matches
Expect: entries parsed, or a diagnostic that the pattern is unsupported
Actual: :cexpr silently produces zero entries
```

Silent zero-results is the worst failure mode here — the user cannot tell an
unsupported specifier from a genuinely empty build log.

**Fix.** Either implement the qualifiers, or classify unknown `%X` as
unsupported and return `None` from `compile_efm_pattern` so the caller can warn
("errorformat pattern N uses unsupported %E"). Do not compile them literally.

### C7. Anvil's backup path collides for tool names containing a dot

`crates/hjkl-anvil/src/installer.rs:588`

```rust
let bak = final_pkg.with_extension("bak");
```

`Path::with_extension` **replaces** the existing extension rather than
appending. A tool named `foo.bar` produces `<packages>/foo.bak`, so:

- two tools `foo.bar` and `foo.baz` share one backup path, and a concurrent
  reinstall of one can have its rollback target clobbered by the other;
- a tool literally named `foo` reinstalling alongside `foo.bar` collides the
  same way.

Both are then `remove_dir_all`'d on the success path (line 601). Tool names are
validated as a single safe path component (`store::validate_name`) but dots are
legal in one.

**Fix.** Build the sibling by name rather than by extension:
`final_pkg.with_file_name(format!("{}.bak", file_name))`, or better, stage the
backup under a per-install unique suffix.

### C8. `pid_is_alive(0)` reports alive

`crates/hjkl-app/src/swap.rs:458`

`kill(0, 0)` signals every process in the caller's process group and returns 0,
so a swap file whose `writer_pid` field decodes as `0` (truncated or corrupted
header) is always classified as owned by a live process. The multi-instance
refusal then blocks the user from opening the file with no way to clear it
except deleting the swap by hand.

Low severity — it needs a corrupt swap file to trigger — but the guard is one
line.

**Fix.** `if pid == 0 { return false; }` before the `kill` call.

---

## 2. Security

The security posture is deliberate and documented throughout — `:!` shell-out is
gated by `policy::shell_disabled()` in non-interactive modes, archive entry
paths go through `safe_join`, tool names through `validate_name`, `:w` through
an `O_NOFOLLOW` probe, downloads through a size cap and a TOFU checksum sidecar.
The findings here are gaps in that scheme, not holes in it.

### S1. `hjkl_lsp::uri::to_path_within` is a security control with no callers

`crates/hjkl-lsp/src/uri.rs:31-45`

The function is documented as "Defense-in-depth against a server returning URIs
outside the workspace". Nothing in the workspace calls it — every consumer
(`lsp_glue.rs:450,1333,1452,1485,2123`) uses the unchecked `to_path`.

The one place containment actually matters, `apply_workspace_edit`, implements
its own check inline (`lsp_glue.rs:2113-2139`) and deliberately _warns rather
than refuses_, which is a defensible call and is commented as such. So this is
not an exploitable gap today — but a dead security helper is worse than no
helper: the next author greps, finds it, and assumes it is enforced somewhere.

There is also a real bug inside it, should it ever be wired up: it validates the
**normalized** path and then returns the **raw** one.

```rust
let normalized = normalize_lexical(&p);
...
if normalized.starts_with(&root_norm) { Some(p) } else { None }
```

`normalize_lexical` folds `..` lexically, which is not how the kernel resolves
it when a component is a symlink. `<root>/link/../secret` normalizes to
`<root>/secret` (accepted) but resolves through `link`'s target's parent.
Returning `p` hands the caller the un-normalized path to act on.

**Fix.** Either delete it, or wire it in and return `normalized`. If it stays,
prefer `hjkl_fs::resolve_under` — that is the seam that already does this
correctly, symlinks included, and is used by the `:w` path.

### S2. Anvil interpolates unvalidated manifest fields into the download URL

`crates/hjkl-anvil/src/installer.rs:510-517`

```rust
let asset = gh.asset_pattern
    .replace("{triple}", triple)
    .replace("{version}", &spec.version);
let url = format!("https://github.com/{}/releases/download/{}/{}",
                  gh.repo, spec.version, asset);
```

`repo`, `version`, and `asset_pattern` are formatted into a URL path with no
validation. A manifest value containing `../`, `?`, or `#` redirects the fetch
to a different path on github.com. The host is fixed and the response is
checksum-verified when a pin exists, so the impact is bounded — but on the TOFU
path (no pin, first install) the redirected artifact becomes the trusted
baseline.

The manifest is user-controlled config today, which is why this is low. It stops
being low the moment a shared/remote registry lands.

**Fix.** Validate `repo` as `owner/name` (`[A-Za-z0-9._-]+/[A-Za-z0-9._-]+`),
reject `/`, `..`, `?`, `#` in `version` and in the expanded `asset`, and build
the URL with `Url::join` rather than `format!`.

### S3. `:Anvil uninstall` leaves the TOFU checksum and `.rev` sidecars behind

`apps/hjkl/src/app/ex_dispatch.rs:2998-3016`

Uninstall removes the package dir and the `bin/` symlink. It does not remove the
checksum sidecar under `<data>/checksums/<name>.toml`, so the TOFU baseline
recorded from a previous install survives an uninstall/reinstall cycle. That is
arguably the _safe_ direction (a changed artifact still trips
`ChecksumMismatch`), but it is undocumented, and a user who uninstalls to
recover from a bad install will hit a mismatch they cannot clear from the UI.

**Fix.** Decide and document: either delete the sidecar on uninstall, or add
`:Anvil forget <name>` to clear the pin. State which in the `anvil` module doc.

### S4. Explorer's cross-device move can hang on a fifo

`apps/hjkl/src/app/explorer_reconcile.rs:348-373`

`copy_dir_recursive` classifies entries with `symlink_metadata` and handles
symlinks correctly (the comment explains why, and there is a test). It does not
handle fifos or device nodes: they fall into the `else` branch and hit
`std::fs::copy`, which blocks forever on a fifo waiting for a writer.

`hjkl-fs` already documents this exact case as one of the three reasons its own
mover exists (`crates/hjkl-anvil/src/installer.rs:278-280`: "A fifo or device
node in the tree is an error rather than a hang"). The explorer does not use it
— see D1.

```
Repro: mkfifo inside a directory; move that directory across a filesystem
       boundary via the explorer (rename → EXDEV → copy_dir_recursive)
Expect: an error naming the fifo
Actual: the editor hangs in fs::copy with no way to cancel
```

Fixed by adopting `hjkl_fs::move_atomic` (D1).

---

## 3. DRY

### D1. Explorer re-implements the filesystem seam it was built to use

`apps/hjkl/src/app/explorer_reconcile.rs:315-394` defines `move_file`,
`move_dir`, `copy_dir_recursive`, and two `copy_symlink` variants. All four are
re-implementations of `hjkl_fs::move_atomic` / `hjkl_fs::dir`, which
`crates/hjkl-app/src/trash.rs:109` and `crates/hjkl-anvil/src/installer.rs:288`
both already call for exactly this operation. `hjkl-fs/src/dir.rs:103` even has
its own `copy_symlink`.

The local copies are strictly weaker than the seam:

| Property                              | `hjkl_fs::move_atomic` | explorer copy   |
| ------------------------------------- | ---------------------- | --------------- |
| Cross-device copy staged then renamed | yes                    | no, in place    |
| Fifo / device node                    | error                  | hangs (see S4)  |
| Symlinks reproduced, not followed     | yes                    | yes (dirs only) |
| Source unlinked only after completion | yes                    | yes             |
| Durability (fsync)                    | configurable           | none            |

The seam's whole stated purpose is that "two copies of a path resolver is how a
fix lands in one of them and silently misses the other"
(`hjkl-config/src/loader.rs:52-56`). This is that situation, in the one place
where the operation is user-initiated deletion and renaming.

**Fix.** Replace all four functions with `hjkl_fs::move_atomic`. The existing
`copy_dir_recursive_preserves_symlinks_without_following` test
(`explorer_reconcile.rs:2246`) should keep passing against the seam.

### D2. `is_safe_component` exists three times

- `crates/hjkl-anvil/src/installer.rs:165`
- `crates/hjkl-anvil/src/store.rs:57` (as `validate_name`, same predicate,
  different error type)
- `crates/hjkl-bonsai/src/runtime/source.rs:183` (`pub`)

Three byte-equivalent implementations of the same security predicate, in two
crates, one of them public. `hjkl-anvil` already depends on `hjkl-fs`; that is
the natural home.

**Fix.** One `pub fn is_safe_component` in `hjkl-fs::path`, re-exported.
`validate_name` keeps its error mapping and calls it.

### D3. Four independent display-width truncators

`hjkl-statusline/src/lib.rs:22,443-454`, `hjkl-prompt-tui/src/lib.rs:44-55,222`,
`hjkl-editor-tui/src/lib.rs:52`, `hjkl-which-key/src/lib.rs:16`,
`hjkl-buffer-tui/src/render.rs:467` each hand-roll the same "accumulate
`UnicodeWidthChar::width` until the budget is exhausted" loop, with slightly
different tab handling and different `unwrap_or` defaults.

`hjkl-buffer/src/geom.rs` already owns `char_col_to_visual_col` /
`visual_col_to_char_col` — the canonical version of this arithmetic, and the one
the 2026-07-29 review's finding #1 was about.

**Fix.** A single `truncate_to_width(s, budget, tab_width) -> &str` (plus the
existing `geom` pair) in one crate, consumed everywhere. This also removes the
window in which a future width fix reaches the statusline but not the prompt.

### D4. `normalize_lexical` duplicates path normalization

`crates/hjkl-lsp/src/uri.rs:50` re-implements lexical `..`/`.` folding that
`hjkl_fs::path` already provides in a symlink-aware form (`resolve_under`,
`hjkl-fs/src/path.rs:159`). Folded into S1's fix.

---

## 4. YAGNI / maintenance

### Y1. `hjkl-vim`'s module doc is a roadmap for work that shipped

`crates/hjkl-vim/src/vim/mod.rs:21-74`

The `# Roadmap` block lists as "still outstanding": registers (`RegisterBank`,
named/append/blackhole/clipboard), marks (`m{a-z}`, `''`, `'[`/`']`/`'<`/`'>`),
`:reg` / `:marks`, macros (`q`/`@`/`@@`), the `>` / `<` indent operators, the
`gU` / `gu` / `g~` case operators, `H` / `M` / `L`, `zz` / `zt` / `zb`,
insert-mode `Ctrl-t` / `Ctrl-d` / `Ctrl-r`, and "`/` and `?` search prompts
still live in the host".

All of it is implemented. Registers are `hjkl-engine/src/registers.rs`; marks
are `Editor::global_marks`; macros have a dedicated test
(`hjkl-vim/tests/macro_digit_register.rs`); the search prompt is
`hjkl-vim/src/search_prompt.rs` in this very crate. The block also points at
`~/.claude/plans/look-at-the-vim-curried-fern.md`, which is not in the
repository.

This is the first thing a new contributor reads in the largest vim module.

**Fix.** Delete the roadmap; keep the grammar description above it, which is
accurate and useful. Genuinely open items belong in `docs/backlog.md`.

### Y2. `hjkl-css` has no consumer in the workspace

`crates/hjkl-css` (2 617 lines, plus `examples/edge.rs`) is not referenced by
any other crate or by `apps/hjkl`. Per the standing policy — a published crate
may have external consumers and must not be deleted on workspace-grep evidence
alone — this is **not** a deletion recommendation. It is a flag: the crate is
carried through every workspace-wide refactor, clippy run, and version bump with
no in-repo test of its integration.

**Fix.** Record its status explicitly in `CONTRIBUTING.md` or the crate's own
doc: "published for external consumers; no in-workspace user". That converts a
recurring "is this dead?" question into a one-line answer.

### Y3. The trash directory is never reclaimed

`crates/hjkl-app/src/trash.rs`

Every explorer deletion moves the entry into `$XDG_CACHE_HOME/hjkl/trash/` with
a monotonic `.N` suffix. Nothing ever removes them. On a cache directory this is
defensible (the user can clear it), but it is undocumented, unbounded, and the
`MAX_RETRIES: u64 = 1000` cap means the 1001st deletion of a same-named file
starts failing rather than reusing a slot.

**Fix.** Document the growth in the module doc, and either age entries out on
startup (older than N days) or raise/remove the retry cap with a comment saying
why 1000 is enough.

### Y4. Mutex poisoning is a whole-editor kill switch

`crates/hjkl-buffer/src/buffer.rs` and `crates/hjkl-engine/src/editor.rs`
contain ~110 `lock().unwrap()` calls between them on `content`, `registers`,
`global_marks`, `change_bank`, `search`, and `abbrevs`.

This is idiomatic and mostly fine — but the consequence is that a panic anywhere
while any one of those locks is held converts every subsequent access into a
panic, including the ones on the save path. An editor's last useful act after an
internal panic is writing the user's buffer to a swap file.

Not a bug, and not worth a mechanical sweep. Worth a decision: either document
"a poisoned lock is a fatal, unrecoverable state" once at the top of
`buffer.rs`, or use `unwrap_or_else(|e| e.into_inner())` on the specific paths
that emergency-persist (the pattern is already used in `trash.rs:135`).

### Y5. Module sizes past the point of navigability

| File                                    | Lines |
| --------------------------------------- | ----- |
| `crates/hjkl-bonsai/src/highlighter.rs` | 2 572 |
| `crates/hjkl-vim/src/editor_ext.rs`     | 2 422 |
| `apps/hjkl/src/nvim_api.rs`             | 5 673 |
| `apps/hjkl/src/app/explorer.rs`         | 4 373 |
| `apps/hjkl/src/render.rs`               | 3 822 |
| `apps/hjkl/src/app/lsp_glue.rs`         | 3 169 |
| `apps/hjkl/src/app/ex_dispatch.rs`      | 3 154 |

Recording only; splitting these is a large, disruptive change with no
correctness payoff, and the code inside them is well-commented. Flagged because
C2 (in `lsp_glue.rs`) is the kind of bug that survives in a file this size.

---

## 5. Hardening notes (low confidence / low severity)

- **`ex_dispatch.rs:681`** — `line[..matches[0].byte_start as usize]` slices a
  `String` at a byte offset supplied by the substitute match set. If the buffer
  is mutated between match collection and `ExEffect::SubstituteConfirm`
  dispatch, `byte_start` can exceed the line length or land off a char boundary
  and panic. I could not construct the interleaving, so this is a guard
  recommendation, not a reported bug: use `line.get(..byte_start)` with a
  fallback.

- **`installer.rs:566` `find_bin`** — searches the extracted tree recursively
  for the first file whose name matches `spec.bin`, so an archive that ships
  `docs/examples/rg` alongside `bin/rg` can have the wrong one symlinked
  (traversal order is `read_dir` order). The tar path skips symlinks, so this
  cannot escape the tree; it can pick the wrong file inside it. Prefer a
  shallow, ordered probe (`bin/<name>`, `<name>`, then recursive).

- **`modeline.rs:93-102` `find_marker`** — returns the first marker in the fixed
  list order `["vim:", "ex:", "vi:"]` rather than the earliest by position, so
  `# vi: ts=2 vim: sw=4` parses from `vim:` and drops the `vi:` options. Vim
  uses the first marker in the line. Cosmetic divergence.

- **`shell.rs:51`** — bare `:!cmd` runs under `Command::output()`, which gives
  the child a null stdin and captures its output. Interactive commands
  (`:!git commit`, `:!less`) cannot work. Vim suspends the TUI and hands the
  child the tty. Worth a `:!` doc note if not planned.

---

## Already tracked (not re-reported)

The following were verified as still-open and are already in `docs/backlog.md`;
this review adds nothing to them.

- Undo-tree per-step O(N) cost after keyframes (§1.1)
- Swap `SerTree.base` document duplication (§1.2)
- `hjkl-editor::spec` external-consumer question (§1.3, Y5) — related to Y2
  above
- LSP full-sync copy and `attach_buffer` boundary copy (§1.4)
- `styled_spans` write-only public field (§1.4)
- `SnapshotFoldProvider::next_visible_row` unbounded `+= 1` (hardening)
- `rope_line_char_count` / `rope_line_bytes` unbounded public API (hardening)
- Remote grammar compilation and `dlopen` (§3, issue #314) — `hjkl-bonsai`'s
  `compile_into` was reviewed and its path validation
  (`runtime/compile.rs:99-111`) is correct for the traversal case; the
  outstanding risk is the design one already tracked there.

## Suggested order

1. **C1** — `git.rs` `OsStr` conversion. One-line-per-call-site, removes a
   data-loss shape.
2. **C2** — merge `WorkspaceEdit` groups by path. Contained to one function.
3. **D1 + S4** — explorer adopts `hjkl_fs::move_atomic`. Deletes ~80 lines and
   closes the fifo hang.
4. **C3, C4** — modeline parser. Small, well-covered by existing tests.
5. **C5** — listchars width. Delete two functions, import the real one.
6. **C6** — errorformat unsupported-specifier diagnostic.
7. **S1** — decide `to_path_within`'s fate; **Y1** — delete the stale roadmap.
8. Everything else as encountered.
