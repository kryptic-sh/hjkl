# hjkl TUI — design scope

Captures the design decisions and phasing plan for the standalone `hjkl` binary
shipped from `apps/hjkl/`. Written 2026-04-27 alongside Phase 1 scaffolding for
historical record. Updated as phases land.

## Goal

A standalone vim-modal terminal editor that ships from `cargo install hjkl`.
Showcases what the hjkl crates already provide (engine, buffer, editor, ratatui
adapters) by combining them into a usable editor. Not a full vim clone — the
target is "demonstrate the primitives + cover the 80% case", not "replace vim".

## Crate layout

```
hjkl/
├── crates/                  # libraries (already published 0.1.1)
│   ├── hjkl-buffer/
│   ├── hjkl-engine/
│   ├── hjkl-editor/
│   └── hjkl-ratatui/
└── apps/
    ├── hjkl/                # umbrella binary crate, ships `hjkl` (TUI)
    └── hjkl-gui/            # future, ships `hjkl-gui` (GUI)
```

Naming convention adopted org-wide:

- `<project>` — TUI default
- `<project>-gui` — GUI variant
- `<project>-sync` / `<project>-helper` — auxiliaries

Mirrors sqeel's umbrella pattern. `cargo install hjkl` lands the TUI; future
`cargo install hjkl-gui` lands the GUI.

## Versioning

Lockstep with the workspace `[workspace.package].version`. The umbrella crate
ships at the same version as the libs (started 0.1.1). Same as sqeel — a single
tag drives all crates' releases.

Tradeoff: a binary at v0.1.1 reads as "early but stable trait surface". Users
who want pre-1.0 churn semantics already understand the 0.x convention.

## Host shape

`TuiHost` lives in `apps/hjkl/src/host.rs`. Implements `hjkl_engine::Host` for
the terminal context. References `sqeel-tui`'s `SqeelHost` as the precedent.

Per the org memory `project_viewport_on_host`: viewport state lives on the Host
trait (not on Buffer, not on Editor). `TuiHost.viewport` is the load-bearing
field; the render loop publishes terminal `width`/`height` via
`host.viewport_mut()` each frame.

`type Intent = ()` for the umbrella binary. Sqeel/buffr use richer Intent enums
to communicate from engine back to UI (e.g. clipboard outbox, search prompt
state); the umbrella editor doesn't need that — terminal state is local to the
binary.

## v0 scope (Phases 1–5)

In:

- Single file load + save (clap arg → seed `Buffer`).
- Normal / Insert / Visual / Command modes (free from FSM).
- All motions / operators / text objects (free from FSM).
- Status line (filename, mode, cursor pos, dirty marker).
- Command line for `:` (free from `hjkl-editor::ex`).
- Ex commands: `:w`, `:q`, `:wq`, `:x`, `:e`, `:set` (free from
  `hjkl-editor::ex`).
- `:%s` search-and-replace (free from `hjkl-editor::ex`; UI confirmation prompt
  is Phase 4 work).
- `:!cmd` shell exec (free; the BrokenPipe fix from 0.1.1 is in).
- `:r !cmd` and `:r file` (free).
- `/` `?` search (free from `hjkl-editor::ex`; render highlighting is Phase 4
  work).
- Undo / redo (free from `Editor`).
- Marks, registers (free from `Editor`).
- Cursor shape per mode (DECSCUSR escapes via `host.emit_cursor_shape`).

Out (deferred):

- Splits / multiple windows.
- Tabs / multiple buffers.
- Syntax highlighting (would need tree-sitter; defer to post-0.2.0).
- Folding rendering (engine has `FoldProvider`; defer).
- Plugins / config files (`.hjklrc`, init.lua-style).
- LSP integration (sqeel-style).

## Phasing

| Phase | Scope                                                                            |
| ----- | -------------------------------------------------------------------------------- |
| 1     | Scaffold crate, TuiHost stub, terminal init/teardown, render empty.              |
| 2     | Event loop wired, motions work, modes switch (cursor shape + status line).       |
| 3     | `:w` saves, `:q` exits, dirty-buffer prompts, file load via clap arg.            |
| 4     | Command line UX, `/`/`?` search render, ex commands, `:%s` prompt.               |
| 5     | Polish — readonly mode, `+linenum`, file-not-found UX, terminal resize, signals. |
| 6     | Ship — extend release.yml binary build matrix, README polish, smoke pass.        |

Phase 1 is in (`a301658` local, not pushed). Phase 2 is the next dispatch.

## Risks / open questions

- **Terminal resize handling** — needs to update `host.viewport()` mid-frame on
  `crossterm::Event::Resize`. Verify Host trait supports the mutation path
  cleanly.
- **`should_cancel` for long shell commands** — `:!cmd` could hang. Phase 5
  wires Ctrl-C interrupt via `should_cancel` polling.
- **Clipboard ergonomics** — `arboard` works on Linux/macOS/Windows; Wayland
  clipboard ownership semantics may need extra care. Phase 3 wires; Phase 5
  polishes.
- **CHANGELOG coupling** — umbrella binary changes mostly don't affect the lib
  crates' `CHANGELOG.md`. Add a separate `apps/hjkl/CHANGELOG.md`? Or a single
  workspace CHANGELOG with sections? **Open.** First release will pick.
- **Binary in release.yml** — current release.yml only runs `publish-crates`.
  Phase 6 extends with a `build` matrix mirroring sqeel's pattern (zigbuild
  glibc, windows-msvc, macos universal).
- **Discovery name on crates.io** — `hjkl` checked, name is available (no
  conflicting crate). Reserve via first publish.

## Future work (beyond v0)

- `hjkl-gui` crate — eframe-based, mirrors sqeel-gui pattern.
- Tree-sitter syntax highlighting (would need a `hjkl-tree-sitter` adapter crate
  to keep the umbrella lean).
- LSP integration (likely under `hjkl-lsp` adapter).
- Init script support (`~/.config/hjkl/init.lua` or similar).
- Splits + tabs.
- Folding render path (engine support is already there via `FoldProvider`).

## Decisions log

- 2026-04-27: Crate layout = single umbrella `apps/hjkl/` + future
  `apps/hjkl-gui/`.
- 2026-04-27: Naming = `hjkl` (TUI) + `hjkl-gui` (GUI).
- 2026-04-27: Versioning = lockstep with workspace.
- 2026-04-27: v0 scope = above (all `?` items in).
- 2026-04-27: Process = scaffold direct, no upfront design doc; this SCOPE.md is
  retroactive for historical record.
- 2026-04-27: Phase 1 landed locally (`a301658`).
- 2026-04-27: Phase 5 arg parsing = approach (a) — manual argv pre-processing
  before clap; `+N` and `+/pattern` tokens are extracted first, remainder parsed
  by hand (no clap — clap can't model vim's `+` prefix). Decision: full
  hand-rolled parser keeps vim parity without fighting clap.
- 2026-04-27: Phase 5 readonly = app-level guard (`do_save` checks
  `editor.is_readonly()`) + engine-level mutation block (engine already blocked
  edits in `mutate_edit`). `Editor::is_readonly()` added to hjkl-engine as a
  minimal Phase 5 lib addition (Settings is not re-exported; accessor is the
  cleaner path).
- 2026-04-27: Phase 5 Ctrl-C during `:!cmd` — DEFERRED. `apply_shell_filter` in
  hjkl-editor calls `child.wait_with_output()` without polling
  `host.should_cancel()`. No safe interrupt point without a deeper change to the
  shell exec path. Candidate for 0.1.2 lib enhancement.
- 2026-04-27: Phase 6 ship-prep complete. Binary publish wired: `build` job
  added to release.yml with 4-target matrix (linux-gnu zigbuild, windows-msvc,
  aarch64- darwin, x86_64-darwin); `publish-crates` extended to 5 crates
  (hjkl-buffer → hjkl-engine → hjkl-editor → hjkl-ratatui → hjkl). Archive name
  pattern: `hjkl-${TAG}-${target}.tar.gz` / `.zip`. README polished for
  crates.io. Smoke pass: binary builds clean; runtime exits with ENXIO (no TTY,
  expected in headless context — not a panic). Pending: user runs BCTP 0.1.2 for
  first user-facing TUI release.
