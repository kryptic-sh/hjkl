# Submodule Absorption Plan — Issue #152

> **Status**: ✅ COMPLETED — historical record. The absorption described below
> has fully landed: there are no git submodules or `.gitmodules`, no
> `[patch.crates-io]` in the root `Cargo.toml`, the `hjkl-ratatui` crate is
> gone, and the workspace is unified at version 0.33.4. Kept for provenance; do
> not treat the version targets (0.22.0/0.23.0) or crate counts below as
> current.
>
> **Generated**: 2026-05-18

---

## Section 1: Submodule Inventory

21 submodules confirmed. All paths follow `crates/<name>`. All git dirs are
`.git` file (gitmodules link) **except hjkl-editor-tui**, which has a `.git/`
directory (likely cloned directly rather than added as a submodule pointer — it
registers fine as a submodule via `git submodule status` so the pointer is still
tracked, but the `.git` dir was cloned separately and lives in-place rather than
in `.git/modules/`).

| Crate           | HEAD SHA  | HEAD tag   | Version | README | CHANGELOG | LICENSE | File count |
| --------------- | --------- | ---------- | ------- | ------ | --------- | ------- | ---------- |
| hjkl-engine     | `8c99450` | `v0.11.3`  | 0.11.3  | yes    | yes       | yes     | 32         |
| hjkl-vim        | `3e9e09b` | `v0.23.1`  | 0.23.1  | yes    | yes       | yes     | 27         |
| hjkl-ex         | `4a5ab3f` | `v0.5.0`   | 0.5.0   | yes    | yes       | yes     | 29         |
| hjkl-buffer     | `843410f` | `v0.8.0`   | 0.8.0   | yes    | yes       | yes     | 31         |
| hjkl-form       | `9ba32fb` | `v0.6.1`   | 0.6.1   | yes    | yes       | yes     | 23         |
| hjkl-picker     | `bc8e5c5` | `v0.9.0`   | 0.9.0   | yes    | yes       | yes     | 25         |
| hjkl-anvil      | `53fa8d5` | `v0.2.4`   | 0.2.4   | yes    | yes       | yes     | 33         |
| hjkl-xdg        | `cbc42b7` | `v0.1.0`   | 0.1.0   | yes    | yes       | yes     | 20         |
| hjkl-config     | `2983e9e` | `v0.2.1`   | 0.2.1   | yes    | yes       | yes     | 19         |
| hjkl-splash     | `bc3d04e` | `v0.3.0`   | 0.3.0   | yes    | yes       | yes     | 19         |
| hjkl-lsp        | `c2e1aa0` | `v0.1.1`   | 0.1.1   | yes    | yes       | yes     | 20         |
| hjkl-keymap     | `1b3841c` | `v0.3.0`   | 0.3.0   | yes    | yes       | yes     | 25         |
| hjkl-editor     | `efa5230` | `v0.8.0`   | 0.8.0   | yes    | yes       | yes     | 20         |
| hjkl-bonsai     | `782f52f` | `v0.7.5`   | 0.7.5   | yes    | yes       | yes     | 44         |
| hjkl-clipboard  | `6dae615` | `v0.5.4`\* | 0.5.5   | yes    | yes       | yes     | 51         |
| hjkl-theme      | `79ff9c5` | `v0.2.0`   | 0.2.0   | yes    | yes       | yes     | 32         |
| hjkl-mangler    | `45bf9b5` | `v0.1.0`   | 0.1.0   | yes    | yes       | yes     | 20         |
| hjkl-ratatui    | `27ba926` | `v0.7.0`   | 0.7.0   | yes    | yes       | yes     | 20         |
| hjkl-editor-tui | `f6911af` | `v0.1.0`   | 0.1.0   | yes    | yes       | yes     | 17         |
| hjkl-picker-tui | `6ad2127` | `v0.4.0`   | 0.4.0   | yes    | yes       | yes     | 17         |
| hjkl-theme-tui  | `80996cf` | `v0.1.1`   | 0.1.1   | yes    | yes       | yes     | 21         |

**Total tracked files across all 21 submodules**: ~565

\* hjkl-clipboard: HEAD tag is `v0.5.4` but Cargo.toml version is `0.5.5`. The
HEAD commit message says "corrects botched 0.5.4". The tag was not re-cut for
0.5.5. This is a known situation (see memory: `[No retag of shipped versions]`).
After absorption, this version is what crates.io gets.

### Git dir anomaly: hjkl-editor-tui

`crates/hjkl-editor-tui/.git` is a **directory**, not the normal `.git` file
symlink that other submodules have. It appears the repo was cloned in-place at
some point (the v0.1.0 hotfix window, likely). `git submodule status` still
reports it correctly. Impact on absorption: `git submodule deinit -f` will still
work; `rm -rf .git/modules/crates/hjkl-editor-tui` will have nothing to remove
(the module data is already in the crate's own `.git/` dir), so skip that step
for this crate specifically.

---

## Section 2: File Collision Audit

### Per-crate `.github/` workflows

Every submodule ships its own `.github/workflows/ci.yml`. These **must be
deleted** after absorption — the monorepo umbrella owns CI. The per-crate CI
references submodule-specific remotes and won't resolve correctly inside the
umbrella workspace anyway.

Do NOT copy any per-crate `.github/` content into the monorepo's `.github/`
after absorption. Delete them in the same PR1 commit that absorbs the files.

### Root-level file collisions

The monorepo root has: `rustfmt.toml`, `deny.toml`, `rust-toolchain.toml`,
`.github/CODEOWNERS`, `.github/dependabot.yml`,
`.github/PULL_REQUEST_TEMPLATE.md`, `.github/ISSUE_TEMPLATE/`.

Every submodule also ships some or all of these at their own root, which after
absorption maps to `crates/<name>/`. Because the files land under
`crates/<name>/` — not at the monorepo root — there are **no true collisions**.
Cargo, rustfmt, cargo-deny, and rust-toolchain all search upward from the
workspace root, so the per-crate copies under `crates/<name>/` become
unreachable dead config. They should be deleted in PR1 for cleanliness.

Files to delete from each absorbed crate directory (if present):

```
crates/<name>/.github/           # entire dir — monorepo owns CI
crates/<name>/rustfmt.toml       # monorepo root rustfmt.toml applies
crates/<name>/rust-toolchain.toml # monorepo root toolchain applies
crates/<name>/deny.toml          # monorepo root deny.toml applies
crates/<name>/.editorconfig      # no root .editorconfig — keep or remove; no conflict
crates/<name>/.gitignore         # monorepo root .gitignore covers /target
```

**Special cases**:

- **hjkl-bonsai**: has `.cargo/config.toml` containing the `xtask` alias and an
  `xtask/` sub-crate. The `.cargo/config.toml` alias
  (`xtask = "run --manifest-path xtask/Cargo.toml ..."`) will conflict with the
  monorepo workflow once merged; the monorepo root does not use
  `.cargo/config.toml`. Action: delete `crates/hjkl-bonsai/.cargo/config.toml`
  after absorption. The `xtask/` crate itself is intentionally not a workspace
  member (it has its own `[workspace]` table to isolate its deps). It can stay
  in-tree at `crates/hjkl-bonsai/xtask/` — cargo will not pick it up as a
  workspace member because the `members = ["crates/*", "apps/*"]` glob matches
  the top-level dir, not nested crates. Verify: `cargo build --workspace` should
  not try to compile `xtask/`.

- **hjkl-engine**: has a `fuzz/` directory with its own `[workspace]` and
  `[patch.crates-io]` entries pointing to `../../hjkl-buffer` and
  `../../hjkl-vim`. After absorption, those relative paths resolve to
  `crates/hjkl-engine/fuzz/../../hjkl-buffer` = `crates/hjkl-buffer` — which is
  correct. The fuzz workspace is a standalone cargo workspace (not a member of
  the umbrella), so it won't interfere. Verify the relative paths still resolve
  after the subtree merge. The `[patch.crates-io]` inside the fuzz workspace
  also becomes redundant (since crates are in the umbrella now) but is harmless
  because it only applies within that fuzz workspace scope.

- **hjkl-clipboard**: has `.github/CODEOWNERS`, `.github/ISSUE_TEMPLATE/`,
  `.github/PULL_REQUEST_TEMPLATE.md`, `.github/dependabot.yml` — all standard,
  all should be deleted.

- **hjkl-bonsai**: has `bonsai.toml` (language manifest) and `themes/` dir with
  `default-dark.toml` and `default-light.toml`. No conflict — these are data
  files used by bonsai itself, not config cargo/rustfmt would pick up.

- **hjkl-theme**: has `themes/default.toml`. No conflict.

- **hjkl-anvil**: has `anvil.toml` (tool manifest). No conflict.

### `target/` and `.cargo/`

- No submodule has a `target/` in its tracked files (they're all gitignored).
- Only `hjkl-bonsai` has `.cargo/config.toml` (tracked). Delete after absorption
  as noted above.
- The monorepo root `.gitignore` already has `/target` which covers the root
  target dir. Per-crate `.gitignore` files (in submodules like hjkl-vim,
  hjkl-config, hjkl-splash, hjkl-lsp, hjkl-editor-tui) contain their own
  `target` rules — these are harmless dead entries once the files live under
  `crates/<name>/` and the monorepo root `.gitignore` governs.

---

## Section 3: `[patch.crates-io]` Entries to Remove

Current root `Cargo.toml` has 48 `[patch.crates-io]` entries. After absorption,
every entry pointing to a workspace member path becomes redundant — cargo
resolves them automatically via `members = ["crates/*", "apps/*"]`.

### Redundant after absorption (remove all 48)

Every entry in the current `[patch.crates-io]` is `{ path = "crates/<name>" }`
or `{ path = "apps/<name>" }` and every such path is already a workspace member.
None of these patches override an external crate — they all patch the
workspace's own crates to redirect crates.io resolution to the local path.

Once all 21 submodule crates are workspace members (they become so the moment
their `Cargo.toml` files are in `crates/<name>/`), the `[patch.crates-io]`
section is **entirely redundant**. Cargo's workspace member resolution takes
precedence over crates.io without any patch needed.

**Action in Phase 1**: Delete the entire `[patch.crates-io]` block from root
`Cargo.toml`. All 48 entries go. The workspace
`members = ["crates/*", "apps/*"]` glob handles everything.

### Entries that are NOT in the submodule list (currently in-tree, still redundant)

All non-submodule entries (`hjkl-keymap-crossterm`, `hjkl-editor-gui`,
`hjkl-statusline`, `hjkl-statusline-tui`, `hjkl-which-key`,
`hjkl-which-key-tui`, `hjkl-completion-tui`, `hjkl-markdown`,
`hjkl-markdown-tui`, `hjkl-hover`, `hjkl-hover-tui`, `hjkl-info-popup`,
`hjkl-info-popup-tui`, `hjkl-prompt`, `hjkl-prompt-tui`, `hjkl-layout`,
`hjkl-app`, `hjkl-holler`, `hjkl-holler-tui`, `hjkl-tabs`, `hjkl-tabs-tui`,
`hjkl-fs-watch`, `hjkl-menu`, `hjkl-menu-tui`, `hjkl-syntax`, `hjkl-syntax-tui`)
are already workspace members today and their patches are already redundant.
They were kept "for safety" during the hybrid submodule phase. All go with the
block deletion.

**Summary**: Remove the entire `[patch.crates-io]` section in Phase 1.

---

## Section 4: CI Workflow Simplifications

### Steps to change in `.github/workflows/ci.yml`

#### 1. `actions/checkout@v6` with `submodules: recursive`

Current: 5 jobs use `submodules: recursive`:

- `fmt` (line 30–31)
- `clippy` (line 43–44)
- `test` (line 64–65)
- `no_std` (line 90–91)
- `build` (line 190–191)
- `publish-crates` (line 683–684)

**Change**: Remove the `with: submodules: recursive` block from all 6
occurrences. Default `actions/checkout@v6` (no `with:` block or
`with: submodules: false`) is sufficient after absorption.

`aur-bin`, `alpine`, and `brew-tap` jobs already use plain `actions/checkout@v6`
(no submodules key) — no change needed there.

#### 2. No `git submodule init` / `git submodule update` steps

None of the current CI jobs use explicit `git submodule init/update` commands —
they rely on `submodules: recursive` in the checkout action. So no additional
step deletions are needed beyond the checkout key change above.

#### 3. `publish-crates` — add all 21 absorbed crates

The current `publish_if_missing` call list covers only in-tree crates. After
absorption, the 21 former submodule crates must be added in topological dep
order. Current list publishes 26 crates (hjkl-statusline through hjkl).

**Correct topo order** for the 21 absorbed crates (deps resolved from Cargo.toml
inspection):

```
Layer 0 (no hjkl-* deps):
  hjkl-xdg, hjkl-anvil, hjkl-clipboard, hjkl-config, hjkl-splash,
  hjkl-lsp, hjkl-keymap, hjkl-buffer, hjkl-theme, hjkl-bonsai,
  hjkl-mangler

Layer 1 (deps only on layer 0):
  hjkl-engine          (no hjkl-* deps)
  hjkl-theme-tui       (depends on hjkl-theme)

Layer 2:
  hjkl-vim             (depends on hjkl-engine, hjkl-keymap)
  hjkl-editor          (depends on hjkl-engine)
  hjkl-editor-tui      (depends on hjkl-engine)
  hjkl-picker-tui      (depends on hjkl-buffer, hjkl-engine)
  hjkl-ex              (depends on hjkl-engine, hjkl-buffer)

Layer 3:
  hjkl-form            (depends on hjkl-engine, hjkl-vim)
  hjkl-picker          (depends on hjkl-buffer, hjkl-engine, hjkl-form)

Layer 4:
  hjkl-ratatui         (depends on hjkl-editor-tui)
```

**Full publish order** for Phase 3 CI (prepend to existing list):

```bash
publish_if_missing hjkl-xdg
publish_if_missing hjkl-anvil
publish_if_missing hjkl-clipboard
publish_if_missing hjkl-config
publish_if_missing hjkl-splash
publish_if_missing hjkl-lsp
publish_if_missing hjkl-keymap
publish_if_missing hjkl-buffer
publish_if_missing hjkl-theme
publish_if_missing hjkl-bonsai
publish_if_missing hjkl-mangler
publish_if_missing hjkl-engine
publish_if_missing hjkl-theme-tui
publish_if_missing hjkl-vim
publish_if_missing hjkl-editor
publish_if_missing hjkl-editor-tui
publish_if_missing hjkl-picker-tui
publish_if_missing hjkl-ex
publish_if_missing hjkl-form
publish_if_missing hjkl-picker
publish_if_missing hjkl-ratatui
# then existing in-tree crates follow:
publish_if_missing hjkl-statusline
...
```

#### 4. `publish-crates` — version resolution already handles workspace

The `publish_if_missing` shell function already falls back to the workspace
version in root `Cargo.toml` when a manifest uses `version.workspace = true` (it
greps root `Cargo.toml` on no-match). No changes needed to the function body.

#### 5. `.gitmodules` file

After Phase 1, `.gitmodules` should be empty or deleted. If `git rm .gitmodules`
is not done as part of the subtree-merge commits, add a cleanup commit in
Phase 3.

---

## Section 5: Version Target

### Current state

| Group                                 | Count | Highest version                          |
| ------------------------------------- | ----- | ---------------------------------------- |
| Workspace version                     | 1     | 0.21.34                                  |
| Submodule crates                      | 21    | hjkl-vim 0.23.1                          |
| In-tree crates (no version.workspace) | 26    | hjkl-app 0.4.10                          |
| In-tree crates (version.workspace)    | 1     | hjkl-compat-oracle 0.1.0 (publish=false) |

### Option A: 0.22.0 + 0.23.0

- **PR1** (absorb): bump workspace to **0.22.0**. Individual crate versions stay
  independent. The workspace version bump signals "monorepo era starts."
- **PR2** (lockstep): bump workspace to **0.23.0**, apply
  `version.workspace = true` to all 43 crates. Many crates reset from lower
  versions (e.g., hjkl-xdg 0.1.0 → 0.23.0, hjkl-anvil 0.2.4 → 0.23.0). This is
  **semver major** for those crates, but project is pre-1.0 (`< 1.0.0`) so
  semver allows it without a major bump requirement. External consumers of
  `hjkl-xdg = "0.1"` on crates.io break, but risk is near zero (project is ~6
  months old, all consumers are in-tree).
- **Rationale**: 0.23.0 is the highest any single crate has reached (hjkl-vim).
  Makes the lockstep version feel earned rather than artificially inflated.

### Option B: 1.0.0

- **PR2** (lockstep): jump to **1.0.0** across all crates.
- **Rationale**: "Monorepo era = stability commitment." Every crate locked at
  1.0.0 is unambiguous messaging.
- **Downside**: hjkl has been < 1.0.0 for a reason — feature set is still
  growing rapidly. 1.0.0 implies stable API surface. Premature given ongoing
  churn in hjkl-engine, hjkl-editor, etc.

### Recommendation: Option A (0.22.0 → 0.23.0)

Rationale:

1. 0.23.0 matches the natural ceiling already reached by hjkl-vim. No artificial
   inflation.
2. Avoids the "stable API" signal of 1.0.0 while the internals are still in
   flux.
3. The two-step (0.22.0 absorb, then 0.23.0 lockstep) gives CI a chance to prove
   the workspace compiles before locking versions together.
4. When the project is genuinely ready for 1.0.0, it can be cut as a deliberate
   milestone rather than a migration artifact.

---

## Section 6: Execution Playbook per Phase

### Phase 1 — Subtree-merge (PR target: v0.22.0)

**Prerequisite**: v0.21.35 hotfix merged and monorepo green. Create a fresh
branch `feat/absorb-submodules`.

For each of 21 submodules, run in sequence (one commit per crate preserves
attribution):

```bash
# Template — repeat for each <name>
git remote add tmp-<name> https://github.com/kryptic-sh/<name>.git
git fetch tmp-<name>

# 1. Remove the submodule pointer from the index
git submodule deinit -f crates/<name>
git rm crates/<name>

# 2. Clear the submodule metadata from .git/modules (skip for hjkl-editor-tui
#    whose .git is a directory, not a modules link)
rm -rf .git/modules/crates/<name>

# 3. Bring the tree in under the same prefix
git read-tree --prefix=crates/<name>/ -u tmp-<name>/main

# 4. Delete per-crate CI and dead config
git rm -rf crates/<name>/.github/
git rm -f crates/<name>/rustfmt.toml \
          crates/<name>/rust-toolchain.toml \
          crates/<name>/deny.toml \
          crates/<name>/.editorconfig \
          crates/<name>/.gitignore 2>/dev/null || true

# 5. Commit (one commit per crate for clean history and easy bisect)
git commit -m "chore(absorb): merge <name> into monorepo (preserve history)"

git remote remove tmp-<name>
```

**Crate-specific steps**:

- **hjkl-editor-tui**: skip `rm -rf .git/modules/crates/hjkl-editor-tui` —
  module data is in the crate's own `.git/` dir. After
  `git rm crates/hjkl-editor-tui` the directory is removed including its
  `.git/`. `git read-tree` then re-creates it without a `.git/` directory.

- **hjkl-bonsai**: after absorbing, also delete
  `crates/hjkl-bonsai/.cargo/config.toml` — it contains a workspace-scoped xtask
  alias that should not apply to the umbrella. The `xtask/` crate
  (`crates/hjkl-bonsai/xtask/`) stays; verify it does not appear as an
  unintended workspace member (it won't because `members = ["crates/*"]` matches
  `crates/hjkl-bonsai`, not `crates/hjkl-bonsai/xtask`).

- **hjkl-clipboard**: has `.editorconfig`, `.github/CODEOWNERS`,
  `.github/ISSUE_TEMPLATE/`, `.github/PULL_REQUEST_TEMPLATE.md`,
  `.github/dependabot.yml` — all included in the
  `git rm -rf crates/<name>/.github/` step.

- **hjkl-clipboard**: also has `DESIGN-0.4.0.md` at the crate root. This is
  crate-specific documentation — keep it (it documents design decisions for
  archaeology).

After all 21 crates absorbed:

```bash
# Remove .gitmodules entirely
git rm .gitmodules
git commit -m "chore(absorb): remove .gitmodules (all submodules now in-tree)"
```

**Root Cargo.toml changes in Phase 1**:

```toml
# Delete the entire [patch.crates-io] block.
# Before (48 entries):
[patch.crates-io]
hjkl-engine = { path = "crates/hjkl-engine" }
... (48 total)

# After: section removed entirely.
```

Also bump workspace version:

```toml
[workspace.package]
version = "0.22.0"
```

Commit:

```bash
git commit -m "chore: remove [patch.crates-io] block + bump workspace to 0.22.0"
```

**Verification after Phase 1**:

```bash
cargo build --workspace --locked
cargo test --workspace --all-features
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
```

---

### Phase 2 — Lockstep versions (PR target: v0.23.0)

All 43 crates (21 absorbed + 22 in-tree, including hjkl-compat-oracle which is
already partial-workspace) get `version.workspace = true`.

**Root Cargo.toml**:

```toml
[workspace.package]
version = "0.23.0"   # was 0.22.0
```

**Per-crate `Cargo.toml` changes** (all 43 crates):

Replace the explicit `version = "X.Y.Z"` line with `version.workspace = true`.

Example diff for hjkl-engine:

```toml
# Before
[package]
name = "hjkl-engine"
version = "0.11.3"
edition = "2021"

# After
[package]
name = "hjkl-engine"
version.workspace = true
edition.workspace = true
```

Apply the same pattern to all workspace-inheritable fields while at it
(`edition`, `license`, `repository`, `authors`, `rust-version`, `homepage`) —
these are already set in `[workspace.package]`. This is optional but aligns with
the monorepo philosophy.

**Crates that need no version change** (already handled):

- `hjkl-compat-oracle`: already uses `edition.workspace = true` etc., just add
  `version.workspace = true` (it currently has `version = "0.1.0"`).

**Crates with `publish = false`** (hjkl-compat-oracle, hjkl-editor-gui, the
bonsai xtask): version.workspace = true is fine; the publish guard prevents
crates.io upload regardless.

**Commit**:

```bash
git commit -m "chore: switch all 43 crates to version.workspace = true (lockstep v0.23.0)"
```

**Verification**:

```bash
cargo build --workspace
cargo test --workspace
# Confirm every crate reports 0.23.0
cargo metadata --format-version 1 | python3 -c "
import json, sys
md = json.load(sys.stdin)
bad = [p['id'] for p in md['packages'] if p['version'] != '0.23.0' and p.get('publish') != [] and 'hjkl' in p['name']]
print('WRONG VERSION:', bad) if bad else print('All hjkl crates at 0.23.0')
"
```

---

### Phase 3 — CI cleanup (PR target: v0.23.1)

**File: `.github/workflows/ci.yml`**

1. Remove `submodules: recursive` from the `with:` block of all 6 checkout steps
   (`fmt`, `clippy`, `test`, `no_std`, `build`, `publish-crates`). If the
   `with:` block has no other keys, remove the `with:` block entirely.

2. Delete `.gitmodules` if not already gone (should be from Phase 1).

3. **Update `publish-crates` job** — add the 21 absorbed crates in topo order
   (see Section 4 for the complete ordered list). Insert them before the
   existing `publish_if_missing hjkl-statusline` call.

4. Update the comment block in `publish-crates` (lines 697–731) to remove the
   note "The hjkl-_ library crates listed as submodules ship from their own
   kryptic-sh/hjkl-_ repos on their own cadence." — that is no longer true.

**No other workflow files need changes**. `cron.yml` and `pages.yml` don't
reference submodules.

**Commit**:

```bash
git commit -m "chore(ci): remove submodule checkout flags, add absorbed crates to publish list"
```

---

### Phase 4 — Repo deletion (no PR)

**Prerequisite**: Phase 3 PR merged, monorepo tag v0.23.1 pushed and CI green,
all 21 crates published on crates.io from the new in-tree location.

```bash
# Ensure delete_repo scope is authorized
gh auth refresh -s delete_repo

# Delete all 21 submodule repos
gh repo delete kryptic-sh/hjkl-engine    --yes
gh repo delete kryptic-sh/hjkl-vim       --yes
gh repo delete kryptic-sh/hjkl-ex        --yes
gh repo delete kryptic-sh/hjkl-buffer    --yes
gh repo delete kryptic-sh/hjkl-form      --yes
gh repo delete kryptic-sh/hjkl-picker    --yes
gh repo delete kryptic-sh/hjkl-anvil     --yes
gh repo delete kryptic-sh/hjkl-xdg       --yes
gh repo delete kryptic-sh/hjkl-config    --yes
gh repo delete kryptic-sh/hjkl-splash    --yes
gh repo delete kryptic-sh/hjkl-lsp       --yes
gh repo delete kryptic-sh/hjkl-keymap    --yes
gh repo delete kryptic-sh/hjkl-editor    --yes
gh repo delete kryptic-sh/hjkl-bonsai    --yes
gh repo delete kryptic-sh/hjkl-clipboard --yes
gh repo delete kryptic-sh/hjkl-theme     --yes
gh repo delete kryptic-sh/hjkl-mangler   --yes
gh repo delete kryptic-sh/hjkl-ratatui   --yes
gh repo delete kryptic-sh/hjkl-editor-tui  --yes
gh repo delete kryptic-sh/hjkl-picker-tui  --yes
gh repo delete kryptic-sh/hjkl-theme-tui   --yes
```

**Note**: `gh repo delete <repo> --yes` does work non-interactively when the
repo is specified explicitly. The `--yes` flag is only ignored when no repo
argument is given (safety guard for "current repo"). All 21 deletions above name
the repo explicitly, so `--yes` suppresses the confirmation prompt correctly.

---

## Section 7: Risks + Rollback Plan

### Risk 1: `git read-tree` brings in wrong files / wrong prefix

**Symptom**: `crates/hjkl-engine/` shows unexpected files or files land at wrong
path.

**Rollback**: Each crate absorption is its own commit. `git reset --hard HEAD~1`
reverts one crate's absorption without affecting earlier or later ones. Re-run
the fetch + read-tree for that crate.

### Risk 2: `cargo build --workspace` fails after Phase 1

**Likely cause**: dependency version constraint in an absorbed crate's
`Cargo.toml` cites a range that doesn't match another absorbed crate's version.
For example, `hjkl-vim` depends on `hjkl-engine = { version = "0.11" }` — if
hjkl-engine is at 0.11.3 and the constraint is `"0.11"`, cargo resolves to
0.11.3 from the workspace. This should be fine. If something breaks, the fix is
updating the constraint in the dependent crate's `Cargo.toml` before Phase 2
lockstep.

**Rollback**: `git reset --hard HEAD~N` where N is the number of absorption
commits made. Or revert the specific crate causing issues.

### Risk 3: lockstep version jump breaks existing `Cargo.lock` of downstream repos

**Scope**: sqeel uses `hjkl-*` crates. After lockstep, any sqeel `Cargo.lock`
pinned to `hjkl-engine = "0.11"` will stop resolving the old version from
crates.io (old version still exists there, so this is fine for external users).
For the in-tree sqeel (if applicable), update deps to `"0.23"`.

**Rollback**: Revert the `version.workspace = true` migration for any specific
crate. Non-destructive since the versions on crates.io are immutable.

### Risk 4: `.gitmodules` entry left behind

**Symptom**: `git submodule status` still lists entries after Phase 1.

**Rollback**: `git rm .gitmodules` and re-commit.

### Risk 5: `hjkl-editor-tui` `.git` dir not cleared by `git rm`

**Symptom**: After `git rm crates/hjkl-editor-tui`, the directory persists
because git refuses to remove a directory containing an untracked `.git/`.

**Fix**: `rm -rf crates/hjkl-editor-tui` manually, then `git read-tree ...`
brings it back clean. Or `git rm -rf --force crates/hjkl-editor-tui`.

### Risk 6: bonsai `xtask/` becomes an unintended workspace member

**Symptom**: `cargo build --workspace` tries to compile
`crates/hjkl-bonsai/xtask/` and fails because it's not a workspace member.

**Verification**: `cargo metadata --format-version 1 | grep '"name":"xtask"'`
should return nothing. If it does appear, add
`exclude = ["crates/hjkl-bonsai/xtask"]` to the workspace `[workspace]` table.

### Risk 7: `gh repo delete` fails — `delete_repo` scope missing

**Fix**: `gh auth refresh -s delete_repo`. Run this before the deletion batch.
Repos are recoverable within 90 days via `gh repo undelete kryptic-sh/<name>`.

### Risk 8: crates.io rate limit during `publish-crates` in Phase 3

**Symptom**: publish waits time out for the 6th or 7th crate.

**Mitigation**: `publish_if_missing` already polls for 5 minutes per crate. Rate
limit is per-account (not per-crate), so 21 new publishes in sequence may hit
the 1-crate-per-10-minute soft limit. If this occurs, the `publish_if_missing`
idempotency guard ensures rerunning the workflow continues from where it left
off.

### Rollback of Phase 4 (repo deletion)

If a repo is deleted prematurely: `gh repo undelete kryptic-sh/<name>`. GitHub
retains deleted repos for 90 days for org owners.

---

## Section 8: Scope Estimate + Open Questions

### Approximate scope

| Metric                       | Estimate                                                                                       |
| ---------------------------- | ---------------------------------------------------------------------------------------------- |
| Total tracked files absorbed | ~565 files                                                                                     |
| Commits introduced (Phase 1) | 21 subtree merges + 2 cleanup commits = 23                                                     |
| Crates updated in Phase 2    | 43 (all workspace crates)                                                                      |
| CI edits (Phase 3)           | ~30 line removals (6× submodules:recursive), ~25 line additions (publish list), comment update |
| Repos deleted (Phase 4)      | 21                                                                                             |

### Time estimate per phase

| Phase                   | Estimated hands-on time                        | Wall-clock (CI)                          |
| ----------------------- | ---------------------------------------------- | ---------------------------------------- |
| Phase 1 — subtree merge | 1–2 hours (scripted)                           | 30–45 min CI                             |
| Phase 2 — lockstep      | 30 min (sed/script across 43 Cargo.toml files) | 20 min CI                                |
| Phase 3 — CI cleanup    | 30 min (manual edit + verify publish order)    | 60+ min CI (publish with indexing waits) |
| Phase 4 — deletion      | 5 min                                          | N/A                                      |

**Total estimated elapsed**: 3–4 hours of active work, 2–3 hours of CI wait.

### Crates with unusual structure needing special handling

| Crate           | Unusual element                                                                                           | Special action                                                                       |
| --------------- | --------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| hjkl-editor-tui | `.git/` is directory, not file                                                                            | skip `rm -rf .git/modules/...`; may need `git rm -rf --force`                        |
| hjkl-bonsai     | `.cargo/config.toml` with xtask alias; `xtask/` sub-crate; `bonsai.toml` language manifest; `themes/` dir | delete `.cargo/config.toml`; verify `xtask/` not a workspace member; keep data files |
| hjkl-engine     | `fuzz/` standalone workspace with relative `[patch.crates-io]` paths                                      | verify relative paths still resolve after absorption                                 |
| hjkl-clipboard  | Version mismatch: tag `v0.5.4`, Cargo.toml `0.5.5`                                                        | absorb as-is; document in CHANGELOG note                                             |
| hjkl-clipboard  | DESIGN-0.4.0.md design doc                                                                                | keep (crate-specific archaeology)                                                    |
| hjkl-anvil      | `anvil.toml` tool manifest                                                                                | keep                                                                                 |
| hjkl-theme      | `themes/default.toml`                                                                                     | keep                                                                                 |

### Open questions

1. **hjkl-editor-gui**: listed in `[patch.crates-io]` but not in the submodule
   list — it's already in-tree at `crates/hjkl-editor-gui`. Nothing to absorb.
   Confirmed in scope as "already in-tree."

2. **Topo publish order**: The ordering in Section 4 is derived from
   `Cargo.toml` grep. Verify with `cargo metadata` dep graph before Phase 3 goes
   live, especially if Phase 1/2 add or reorganize any deps.

3. **hjkl-clipboard tag mismatch** (v0.5.4 tag, 0.5.5 version): After
   absorption, `publish_if_missing` will check crates.io for
   `hjkl-clipboard/0.5.5`. If 0.5.5 was never published (it was "corrected
   botched 0.5.4" but may not have been re-published under 0.5.5), the first
   release from the monorepo will publish it. This is safe — crates.io accepts
   new versions.

4. **sqeel dependency updates**: After lockstep, sqeel's `Cargo.toml` deps like
   `hjkl-engine = "0.11"` will need updating to `"0.23"`. This is a downstream
   BCTP-level task for sqeel, not part of this plan.

5. **bonsai xtask workspace exclusion**: Verify with `cargo metadata` that
   `crates/hjkl-bonsai/xtask/` is excluded. If needed, add to workspace
   `exclude` list.

6. **CHANGELOG note**: Issue #152 suggests a post-mortem note in CHANGELOG
   explaining the migration. Add to `## [0.23.0]` section during Phase 2.

---

## Appendix: Submodule URLs (for Phase 1 git remote add)

```
hjkl-engine      https://github.com/kryptic-sh/hjkl-engine.git
hjkl-vim         https://github.com/kryptic-sh/hjkl-vim.git
hjkl-ex          https://github.com/kryptic-sh/hjkl-ex.git
hjkl-buffer      https://github.com/kryptic-sh/hjkl-buffer.git
hjkl-form        https://github.com/kryptic-sh/hjkl-form.git
hjkl-picker      https://github.com/kryptic-sh/hjkl-picker.git
hjkl-anvil       https://github.com/kryptic-sh/hjkl-anvil.git
hjkl-xdg         https://github.com/kryptic-sh/hjkl-xdg.git
hjkl-config      https://github.com/kryptic-sh/hjkl-config.git
hjkl-splash      https://github.com/kryptic-sh/hjkl-splash.git
hjkl-lsp         https://github.com/kryptic-sh/hjkl-lsp.git
hjkl-keymap      https://github.com/kryptic-sh/hjkl-keymap.git
hjkl-editor      https://github.com/kryptic-sh/hjkl-editor.git
hjkl-bonsai      https://github.com/kryptic-sh/hjkl-bonsai.git
hjkl-clipboard   https://github.com/kryptic-sh/hjkl-clipboard.git
hjkl-theme       https://github.com/kryptic-sh/hjkl-theme.git
hjkl-mangler     https://github.com/kryptic-sh/hjkl-mangler.git
hjkl-ratatui     https://github.com/kryptic-sh/hjkl-ratatui.git
hjkl-editor-tui  https://github.com/kryptic-sh/hjkl-editor-tui.git
hjkl-picker-tui  https://github.com/kryptic-sh/hjkl-picker-tui.git
hjkl-theme-tui   https://github.com/kryptic-sh/hjkl-theme-tui.git
```
