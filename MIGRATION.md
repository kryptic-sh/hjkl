# hjkl Extraction Plan

Pluck vim logic out of sqeel into this standalone monorepo. Three consumers:
sqeel (existing), buffr (browser), hjkl binary (full vim clone, future).

## Target Shape

```
github.com/kryptic-sh/hjkl       # this repo
├── Cargo.toml                   # workspace
├── crates/
│   ├── hjkl-engine/             # FSM + grammar, traits, no I/O deps
│   ├── hjkl-buffer/             # text rope + cursor + edits + undo
│   ├── hjkl-editor/             # glue: engine + buffer + registers + ex
│   └── hjkl-ratatui/            # optional: ratatui Style adapters, KeyEvent conversions
└── apps/
    └── hjkl/                    # binary: vim clone (later)
```

`hjkl-ratatui` keeps ratatui types out of the engine entirely. sqeel and the
hjkl binary opt in; buffr ignores it.

## Key Design Decisions (Lock Before Phase 5)

These choices ripple through the trait surface — decide up front.

- **Edition**: pin workspace to `2021`. buffr is on 2021, sqeel can stay
  on 2024. Lower adoption friction beats marginal 2024 features.
- **`no_std` posture**: `hjkl-engine` is `no_std + alloc` compatible (target
  buffr wasm). `hjkl-buffer` and `hjkl-editor` may use `std`. Gate with
  `#![cfg_attr(not(feature = "std"), no_std)]` from day one — retrofitting is
  expensive.
- **Clipboard hybrid (non-blocking write, cached read)**: matches the
  neovim/helix OSC52 model. Engine stays sync; host absorbs all async.
  - `fn write_clipboard(&mut self, text: String)` — fire-and-forget. Host queues
    internally and flushes on its own task (OSC52 escape, shell-out to
    `wl-copy`/`pbcopy`, etc.). Never blocks engine.
  - `fn read_clipboard(&mut self) -> Option<String>` — returns last-known cached
    value. Host refreshes cache async on focus events / OSC52 reply / explicit
    poll. Stale reads acceptable (same limitation OSC52 has in practice).
  - No `async fn` in trait. No async cascade into `Editor::execute`. `no_std` +
    wasm friendly.
- **Engine owns its `Style`**: `hjkl_engine::Style` is a plain struct (fg, bg,
  attrs as bitflags). `hjkl-ratatui` provides
  `From<engine::Style> for ratatui::Style`. No ratatui type appears in engine or
  editor signatures.
- **`Host` intent is generic**:
  `trait Host { type Intent; fn emit(&mut self, intent: Self::Intent); }` with a
  default `type Intent = ();`. sqeel sets `type Intent = SqeelIntent`. No
  `LspIntent` enum lives in engine.
- **Versioning scheme**: use `0.0.x` for the churn phase (every crate bumps in
  lockstep, breaking allowed). Promote to `0.1.0` only when trait surface is
  stable. Avoids the `^0.1` semver resolution trap where minor bumps don't
  satisfy callers.
- **Trait sub-division**: 45 unique methods on `sqeel-buffer` per phase 0 audit.
  After relocating folds (8) + viewport (3) to host and moving motion logic (24)
  into engine FSM functions, real `Buffer` trait surface drops to **~12-15
  methods** across `Cursor`, `Query`, `Edit`, `Search` sub-traits.
  `Buffer: Cursor + Query + Edit + Search`. Sealed via
  `mod sealed { pub trait Sealed {} }` to prevent downstream impls until 1.0.
  See `AUDIT.md` for full method list.
- **Selection model: multicursor (helix-style)**. Engine state is
  `Vec<Selection>` always; single-cursor is `len == 1`.
  `Selection { anchor: Pos, head: Pos }`. Visual range = `anchor != head`. Vim
  mode operates on `selections.primary` only; multi-cursor available as
  superset. Cannot be retrofitted — bake in day one.
- **`Send + Sync` bounds on engine types**: `Editor`, `Buffer`, `Host` required
  `Send`. buffr wasm Web Worker needs it; cheap to add now, expensive later.
- **MSRV: latest stable, tracks stable**. Pin `rust-version` to current stable
  (~1.95 as of 2026-04-25). Treat as floor, not ceiling. Bump freely as new
  features land. Document policy in `CONTRIBUTING.md`. Consumers are in-house
  (sqeel, buffr, hjkl binary) + niche library users — all on latest stable. No
  upside to pinning old.
- **Error policy**: `Result<T, EngineError>` for user-facing failures (regex
  compile, ex parse, invalid range). `debug_assert!` for internal invariants.
  Public APIs never panic on valid input. `thiserror` for error types.
- **Unicode**: column counting uses graphemes via `unicode-segmentation`
  (`no_std` compatible with `alloc`). Motions (`l`, `0`, `$`, `w`, `b`) operate
  on graphemes, not bytes or chars. Document `iskeyword` semantics for word
  motions.
- **Highlighting model**: engine emits `Vec<Highlight { range, kind }>` per
  query. `HighlightKind` covers `Selection`, `SearchMatch`, `IncSearch`,
  `MatchParen`. Host renders. No styling in engine.
- **Cursor shape intent**: engine emits `CursorShape::{Block, Bar, Underline}`
  on mode change via `Host::emit`. Default variants exist on `Host::Intent`
  regardless of consumer's custom intent type.

## Spec Lock (Resolve Before Phase 5)

These are vim-feature scope decisions. Single-page sketch lives in
`hjkl-engine/SPEC.md` once locked.

- **Mappings**: `:map` registry lives in engine (`Keymap` struct, mode-keyed).
  Recursive vs `noremap` honored. `<leader>` configurable. Per-mode tables.
- **Settings/options**: `Options` struct passed to `Editor`. Schema covers
  `tabstop`, `shiftwidth`, `expandtab`, `iskeyword`, `ignorecase`, `smartcase`,
  `hlsearch`, `incsearch`, `wrapscan`. Per-buffer override optional. `:set` ex
  command mutates.
- **Marks / jump list / change list**:
  - Marks `'a`–`'z`: per-buffer; `'A`–`'Z`: global. Engine state.
  - Jump list, change list: engine state. Bounded ring (default 100).
- **Macros**: `q{reg}` records raw `Input` events into register; `@{reg}`
  replays. Records inputs (not parsed actions) — closer to vim semantics +
  simpler for replay across mode changes.
- **Dot-repeat**: captures parsed action (operator + motion + count + insert
  text). Replays per-selection in multi mode. Distinct from macros.
- **Ex commands in 0.0.1**: minimum viable —
  - `:w`, `:q`, `:wq`, `:q!` (intent-emitted; host handles I/O)
  - `:s/pat/rep/flags` with range
  - `:set {opt}[={val}]`
  - `:map`, `:noremap`, `:unmap` (per-mode variants)
  - `:[range]d`, `:[range]y`, `:[range]p`
  - `:nohlsearch`
  - Range parser: `%`, `'a,'b`, `1,$`, `.,.+5`, line numbers
  - Defer: `:!cmd`, `:read`, `:make`, `:vimgrep`, `:argdo` to post-0.1.0.
- **Search regex flavor**: `regex` crate (PCRE-ish). Smartcase via
  `Options::smartcase`. No vim magic mode — document divergence in README.
  `\v`/`\V` parse to no-ops or warn.
- **Visual block**: degenerate multi-selection (one `Selection` per line in
  block range). Falls out of multicursor model for free.
- **Multi-buffer model**: **host-managed**. `Editor` owns one buffer. Buffer
  list, `:ls`, `:b{n}`, `:bn`/`:bp`, tab/window switching all delegated to host
  via `Host::Intent` variants (e.g., `SwitchBuffer(id)`, `ListBuffers`). Cleaner
  separation; sqeel + buffr both already manage multiple open files at host
  level.
- **`Input` enum spec**: exhaustive. Define in `hjkl-engine::input`:

  ```rust
  pub struct Modifiers { ctrl: bool, shift: bool, alt: bool, super_: bool }
  pub enum Input {
      Char(char, Modifiers),
      Key(SpecialKey, Modifiers),
      Mouse(MouseEvent),
      Paste(String),       // bracketed paste
      FocusGained,
      FocusLost,
      Resize(u16, u16),
  }
  pub enum SpecialKey {
      Esc, Enter, Backspace, Tab, BackTab,
      Up, Down, Left, Right,
      Home, End, PageUp, PageDown,
      Insert, Delete,
      F(u8),
  }
  ```

- **Mouse support**: in for 0.0.1, basic. Click → cursor move, drag → visual
  select, wheel → viewport scroll intent. `MouseEvent { kind, pos, mods }`.
- **Multi-key timeout**: `Host::now() -> Instant` (or `Duration` since start,
  no_std friendly). Engine tracks pending sequence start; emits resolution when
  `now - start > Options::timeout_len`. Vim's `timeoutlen` semantics. Cheap to
  design now.
- **Render frame contract**: engine exposes `fn render(&self) -> RenderFrame`
  returning the full frame state. Host diffs. No partial updates from engine.
  Struct:

  ```rust
  pub struct RenderFrame<'a> {
      mode: Mode,
      selections: &'a SelectionSet,
      highlights: Vec<Highlight>,
      cursor_shape: CursorShape,
      mode_indicator: &'a str,
      command_line: Option<&'a str>,
      search_prompt: Option<&'a str>,
      status_line: &'a str,
      viewport: Viewport,
  }
  ```

- **File I/O + encoding + line endings**: engine has no I/O. Host provides:
  - `Host::buffer_path() -> Option<&Path>`
  - `Host::write_buffer(path, bytes)` — async at host's discretion
  - UTF-8 only in engine; host transcodes on read/write.
  - Engine assumes `\n`; host normalizes `\r\n` ↔ `\n` at boundaries. Editor
    exposes `Options::fileformat` (unix/dos/mac) for round-trip.
- **Tree-sitter / syntax highlighting**: out of scope for engine. Host supplies
  highlights via `Host::syntax_highlights(range) -> Vec<Highlight>`. Engine
  merges syntax highlights with engine-emitted highlights (selection, search)
  for the render frame. sqeel uses tree-sitter-sql; buffr can no-op or use
  tree-sitter-wasm.
- **Determinism guarantee**: engine output is a pure function of input stream +
  initial state + `Host` queries. No clock reads except via `Host::now()`.
  Required for fuzz, macro replay, record/replay debugging. State explicitly in
  `hjkl-engine` README.
- **Snapshot for testing**: `Editor::snapshot() -> EditorSnapshot` —
  serde-serializable. Golden tests assert `input_stream → snapshot`. Replay
  debugging restores state + replays input.
- **LSP intent variant set** (sqeel-side, but document expected shape so
  hjkl-binary later can match): `Hover(Pos)`, `Complete(Pos, trigger)`,
  `GotoDef(Pos)`, `Rename(Pos, new_name)`, `Diagnostic(line)`,
  `FormatRange(Range)`. sqeel's `Host::Intent` includes these; engine knows
  nothing.
- **Plugin scope**: explicit **no plugins in 0.x**. Stated in README. Hook
  surface is `Host` trait + `Intent` variants only. Reduces 0.x churn surface.
- **Unsafe policy**: `#![forbid(unsafe_code)]` in `hjkl-engine`, `hjkl-editor`,
  `hjkl-ratatui`. `hjkl-buffer` may opt out for rope perf — document each
  `unsafe` block with safety comment + miri test.
- **Buffer change observation (pull)**: host calls
  `Editor::take_changes() -> Vec<Edit>` once per render frame. Engine
  accumulates edit log between calls; drains on take. No callbacks, no
  re-entrancy. Host uses changes for: redraw region invalidation, dirty flag,
  LSP `didChange` (sqeel), DOM sync (buffr). Cleaner than push — no callback
  ordering issues, host controls cadence.
- **Edit transactions / undo grouping**: hybrid model — implicit by mode
  boundaries + explicit API for hosts. Storage is an undo **tree**, not a stack
  (vim `g-`/`g+` requires branching). Selections restored on undo. See dedicated
  **Undo Model** section below for full spec.
- **External edit remapping**: host modifies buffer outside engine (LSP rename,
  formatter, external reload). API: `Editor::apply_external_edit(Edit)` mutates
  buffer + remaps selections, marks, jumps, change list to track. Without this,
  LSP rename desyncs cursor.
- **Initial state**: `Editor::new(buffer, host, options)` produces: Normal mode,
  single selection at `(0, 0)`, empty registers + marks + jump list + change
  list, viewport top-left, no pending key sequence, empty undo history. State
  explicitly in docs so hosts and tests agree.
- **Default keymap**: engine ships `Keymap::vim_defaults()` covering motions,
  operators, text objects, mode switches. Hosts may layer `:map`/`:noremap` on
  top, or replace entirely with `Keymap::empty()`. Defaults live in
  `hjkl-engine/src/keymap/defaults.rs`.
- **Read-only mode**: `Options::readonly: bool` and per-buffer override. `Edit`
  trait methods checked before dispatch; rejected with `EngineError::ReadOnly`.
  `:set ro` / `:set noro` flip.
- **Error reporting**: two paths.
  - Transient (last command result): `RenderFrame::status_line` carries error
    string. Host displays in status bar. Cleared on next action.
  - Sticky (host should react): `Host::Intent::Error(String)` variant. Document
    expected variants on `Host::Intent`.
- **Viewport**: engine owns
  `Viewport { top_line: u32, height: u32, scroll_off: u32 }`. Commands like `zz`
  / `zt` / `zb` / `Ctrl-D` / `Ctrl-U` / `Ctrl-E` / `Ctrl-Y` mutate it. Render
  frame exposes viewport for host to render within. Host informs of resize via
  `Input::Resize`.
- **Incremental search caching**: regex compile is ~50μs/pattern for simple
  patterns — fine per-keystroke, but cache compiled `Regex` by pattern string in
  LRU (size 32). Cache lives in `Editor`. Bench validates inc-search step <100μs
  end-to-end.
- **Cancellation hook**: `Host::should_cancel() -> bool` checked in long-running
  loops (regex on huge buffer, large multi-cursor edit). Cooperative — engine
  polls every N iterations. Reserve trait method in 0.0.1 even if no callsites
  yet, so adding interruptibility later is non-breaking.
- **History**: `:`-cmd history + `/`-search history. Engine state (bounded ring,
  default 100 each). Snapshot includes history. Host may persist snapshot across
  sessions (shada-equivalent) or not. No engine I/O.
- **Undo storage layering**: engine owns `UndoTree` (per-buffer). Buffer trait
  stays minimal — no undo methods. Edits flow through `Editor::edit()` which
  records into undo tree before delegating to `Buffer::Edit`. Vim-style
  branching undo (`g-`/`g+`) supported from day one — undo is a tree, not a
  stack.
- **Snapshot format**:

  ```rust
  pub struct EditorSnapshot {
      version: u32,        // bump on incompatible change
      mode: Mode,
      selections: SelectionSet,
      options: Options,
      keymap: Keymap,
      marks: MarkTable,
      jump_list: JumpList,
      change_list: ChangeList,
      registers: Registers,
      cmd_history: VecDeque<String>,
      search_history: VecDeque<String>,
      undo_tree: UndoTree,
      viewport: Viewport,
  }
  ```

  Buffer text is **not** in snapshot — host serializes separately (file path or
  content blob).

- **`:set` schema**: typed options.

  ```rust
  pub enum OptionValue { Bool(bool), Int(i64), String(String) }
  pub struct Options { /* typed fields */ }
  impl Options {
      pub fn set_by_name(&mut self, name: &str, val: OptionValue) -> Result<()>;
      pub fn get_by_name(&self, name: &str) -> Option<OptionValue>;
  }
  ```

  Parser for `:set` dispatches by name. Validation per option (e.g., `tabstop`
  must be `Int >= 1`).

- **Rope choice**: **Ropey** for `hjkl-buffer`. Mature, well-tested, std-only
  (fine — `hjkl-buffer` allows std). Stable API. Decision doesn't leak through
  trait surface — engine sees `Buffer` trait, not Ropey directly.
- **Config file**: **out of scope for 0.0.x**. No `.vimrc`-equivalent in engine.
  Hosts may build one on top of `:source` (deferred to post-0.1.0). State in
  README to stop feature creep.
- **Modeline**: `# vim: ts=4 sw=4` headers — **out of scope**. If host wants
  modelines, host scans on load + applies via
  `Editor::execute_ex(":set tabstop=4")`. Document.
- **Re-export strategy**: `hjkl-editor` re-exports `hjkl-engine` public types
  (`pub use hjkl_engine::{Mode, Selection, Pos, Input, SpecialKey, Modifiers, EngineError, ...};`).
  Consumers add 1 dep, not 3. sqeel + buffr ergonomics.
- **Vim compatibility oracle**: README will claim "vim-compatible". Pick one
  (decide phase 0):
  - **Headless vim diff**: run real vim `--clean -e` on golden inputs, compare
    buffer state. Slow (~1s per case) but truthful. Best for credibility.
  - **Hand-curated tests from `:help`**: faster, but selection bias.
  - **Trust existing sqeel tests**: zero new work, weakest claim.
  - Recommend headless-vim diff in cron CI for ~50 canonical cases; hand-curated
    for the rest.
- **Type aliases**: ship `pub type VimEditor = Editor<Rope, DefaultHost>;` in
  `hjkl-editor`. Vim-mode consumers (sqeel) avoid generic noise.
- **Security note (deferred features)**: when `:!cmd`, `:source`, and
  arbitrary-source macro replay land post-0.1.0, they need explicit opt-in via
  `Options::allow_shell_exec`, `allow_source` flags. Note in SPEC so design
  doesn't paint into a corner.
- **`SelectionKind`**: `v` (char), `V` (line), `Ctrl-V` (block) need different
  semantics. Add to `Selection`:

  ```rust
  pub enum SelectionKind { Char, Line, Block }
  pub struct Selection {
      anchor: Pos, head: Pos,
      kind: SelectionKind,
      // anchor/head columns ignored when kind == Line; full lines covered.
      // Block: rectangle from min(col) to max(col), each line a sub-range.
  }
  ```

  Without this `V`+`d` deletes wrong amount.

- **Empty buffer + end-of-buffer cursor semantics**:
  - Empty buffer: cursor at `(0, 0)`, `Buffer::len() == 0`, `line_count() == 1`
    (single empty line).
  - `dd` on empty buffer: no-op (no error).
  - `0`/`$` on empty line: stay at column 0.
  - Cursor past last line: forbidden in normal/visual; allowed in insert at
    virtual `(line_count, 0)` only when buffer ends without trailing `\n` and
    `o` was used.
- **Display vs logical line**: `j`/`k` move logical lines (engine internal).
  `gj`/`gk` move display lines (wrap-aware). Engine doesn't know wrap width;
  queries `Host::display_line_for(pos) -> u32` and
  `Host::pos_for_display(display_line, col) -> Pos`. No-op default (returns
  logical) for hosts that don't wrap.
- **Tab char + `expandtab`**: `Tab` in insert mode →
  - `expandtab=false`: insert literal `\t`.
  - `expandtab=true`: insert `tabstop - (col % tabstop)` spaces. Engine never
    expands `\t` in stored buffer; host renders with `Options::tabstop` for
    column math.
- **`autoindent`**: Enter in insert mode copies previous line's leading
  whitespace to new line. `Options::autoindent: bool` (default true).
  `smartindent` / `cindent` deferred.
- **Paste mode**: `Input::Paste(String)` bypasses insert-mode mappings,
  abbreviations, autoindent. Inserted as raw text. Document — otherwise pasted
  text gets mangled by mappings.
- **Fold manipulation**: `zo`/`zc`/`zM`/`zR`/`za`/`zA` emit
  `Host::Intent::FoldOp(FoldOp)` variants. Host owns fold state; engine emits
  intent only.
- **Numbered registers**: `"0` (last yank), `"1`–`"9` (delete history, shift on
  each delete). In scope for 0.0.1. Vim semantics.
- **Macro replay determinism vs time**: macro replay uses recorded sequence as
  authoritative — timeout-based mapping resolution disabled during replay. No
  `Host::now()` reads inside replay. Guarantees reproducibility regardless of
  replay environment.
- **`MockHost` for tests**: ships in `hjkl-editor::testing` behind
  `feature = "testing"`:

  ```rust
  pub struct MockHost {
      now: Cell<Duration>,
      clipboard: RefCell<Option<String>>,
      intents: RefCell<Vec<Intent>>,
      // ...
  }
  impl MockHost {
      pub fn advance(&self, d: Duration);
      pub fn intents_drained(&self) -> Vec<Intent>;
  }
  ```

  Required for any test touching timeouts or clipboard.

- **Property test corpus**: seed proptest from real sqeel session traces. Record
  via `apps/hjkl-trace/` (later) or hand-curated. Stored in
  `crates/hjkl-engine/tests/corpus/`. Better coverage than pure random.
- **`BufferId`**: opaque newtype in engine. `pub struct BufferId(u64);` — host
  constructs and tracks. Engine echoes in intents for buffer-switch operations.
- **`Edit` type**: explicit shape for change log + undo:

  ```rust
  pub struct Edit {
      range: Range<Pos>,        // pre-edit range replaced
      replacement: String,      // text inserted (empty = pure delete)
  }
  ```

  Multi-cursor edits = `Vec<Edit>` ordered **reverse byte offset** so each
  edit's positions remain valid after the prior edit applies.

- **Trait object support**: **generic only**, no `Box<dyn Buffer>`. Sealed +
  `Send` makes dyn-compat awkward; monomorphization gives ~5-15% perf on hot
  paths. Document in `IMPLEMENTERS.md`.
- **`take_changes()` return shape**: `Vec<Edit>` (allocates per call). Simpler
  than iterator-tied-to-engine-lifetime. Revisit if alloc bench shows dominance
  — switch to `Drain<'_, Edit>` then.
- **Re-entrancy model**: `Host` methods receive `&mut Host` only. `Editor` is
  borrowed `&mut` during the call. `Host` methods MUST NOT call back into
  `Editor` (compile-time prevented by borrow checker). Single-threaded model —
  `Host::should_cancel()` etc. are pure reads of host state, never editor state.
- **Operator-motion-count FSM**: vim's `[count]operator[count]motion` is one
  unified parser. Lives in `hjkl-engine::fsm::grammar` as a single module, NOT
  split per-operator. Tempting to split; resist. Operators differ only in the
  action applied to the resolved range.

## Out of Scope (Document in README)

Stop "is X supported?" issues post-publish. Each gets a one-line README section
with status + alternative.

| Feature                        | Status                                                |
| ------------------------------ | ----------------------------------------------------- |
| Plugin / scripting system      | Never in 0.x. `Host` trait + `Intent` is the surface. |
| Text object extensibility      | Fixed set in 0.0.1; plugin hook post-1.0 maybe.       |
| `inccommand` live `:s` preview | Deferred to 0.1.x.                                    |
| Expression register `"=`       | Never. Requires vim-script interpreter.               |
| Abbreviations `:ab`            | Deferred to 0.1.x.                                    |
| Spell check / conceal / signs  | Never in engine. Host concern via `Highlight` enum.   |
| `:source` / `:runtime`         | Deferred to post-0.1.0; needs security review.        |
| `:!cmd` shell exec             | Deferred to post-0.1.0; opt-in via `Options`.         |
| `.vimrc`-style config files    | Out of engine scope. Hosts may build via `:source`.   |
| Modeline parsing               | Host scans on file load + applies via `:set`.         |
| `smartindent` / `cindent`      | Deferred. `autoindent` only in 0.0.1.                 |
| `swap`/`undofile` persistence  | Host concern via `EditorSnapshot` serialization.      |
| Localization / i18n            | English-only strings. UTF-8 content fully supported.  |
| Multi-buffer in `Editor`       | Host-managed. `Editor` owns one buffer.               |
| Async ex commands              | Host implements via `Intent` + own task; engine sync. |

- **Logging strategy**: `tracing` levels —
  - TRACE: every FSM transition + buffer query.
  - DEBUG: parsed actions, ex command dispatch.
  - INFO: mode changes, ex command success.
  - WARN: invalid input, regex compile fail, range parse fail.
  - Spans: `editor.execute`, `ex.dispatch`, `fsm.step`. No PII (text content) at
    INFO+.

## Platform Support

| Platform | Minimum                                         | Targets                                                    |
| -------- | ----------------------------------------------- | ---------------------------------------------------------- |
| Linux    | glibc 2.28 (Debian 10+, Ubuntu 18.04+, RHEL 8+) | `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`    |
| macOS    | 12 Monterey                                     | `x86_64-apple-darwin` + `aarch64-apple-darwin` (universal) |
| Windows  | Windows 10                                      | `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`        |
| RISC-V   | Deferred / untested                             | `riscv64gc-unknown-linux-gnu` (works in theory, no CI)     |

**RISC-V status**: hjkl is portable pure-rust + `no_std + alloc` clean, so
`cargo build --target riscv64gc-unknown-linux-gnu` should succeed today — just
untested. Promote to tier-2 cron build (cross-compile via zigbuild, no test
runs) post-0.1.0 if a real consumer surfaces. README states "untested on RISC-V;
PRs welcome." No design decision blocks adding it later.

### Build configuration

- **Linux glibc pinning**: `cargo-zigbuild` for explicit version targeting.
  ```bash
  cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.28
  cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.28
  ```
  Cleaner than building on an old image; one CI runner produces all
  glibc-versioned linux artifacts.
- **macOS deployment target**: set in `.cargo/config.toml`:
  ```toml
  [env]
  MACOSX_DEPLOYMENT_TARGET = "12.0"
  ```
  Universal binary built via `lipo`:
  ```bash
  cargo build --release --target x86_64-apple-darwin
  cargo build --release --target aarch64-apple-darwin
  lipo -create -output target/hjkl \
    target/x86_64-apple-darwin/release/hjkl \
    target/aarch64-apple-darwin/release/hjkl
  ```
- **Windows**: default rust toolchain produces Win 10+ binaries. No special
  config needed.

### CI matrix

- **Per-PR / push**: `[ubuntu-latest, macos-14 (Sonoma ARM64), windows-latest]`.
  All three required to pass.
- **Cron**: `macos-13` (Ventura, Intel-era) catches Intel-only regressions. (GH
  Actions doesn't offer macOS 12 anymore; `MACOSX_DEPLOYMENT_TARGET=12.0` flag
  covers compat.)

### Release codesigning (post-publish, not CI-blocking)

- macOS: Developer ID + notarization for distribution outside App Store. Use
  `apple-codesign` Rust crate or Apple's `codesign` + `notarytool`. Defer to
  release process; not blocking for crates.io publish (libraries don't need
  notarization). Required for `apps/hjkl/` binary release.
- Windows: optional MSI signing for `apps/hjkl/`. Skip until user-distribution
  phase.
- Linux: GPG-sign release artifacts for `apps/hjkl/` deb/AppImage. Not needed
  for crates.io.

## Performance Budgets

Concrete numbers, enforced via criterion benches. CI fails if exceeded.

| Operation                              | Budget  |
| -------------------------------------- | ------- |
| FSM step (single keystroke)            | <10 μs  |
| Insert char at cursor (1MB buffer)     | <5 μs   |
| Search-next on 10k-line buffer         | <1 ms   |
| `100dd` parse + execute                | <500 μs |
| Multi-cursor edit, N=100, 1KB inserts  | <2 ms   |
| Render frame build (10k visible lines) | <100 μs |
| Cold load 10MB file into rope          | <50 ms  |
| `hjkl-engine` wasm gzipped             | <100 KB |
| FSM step allocations                   | ≤1      |
| Render frame build allocations         | ≤5      |
| Trait method count (all sub-traits)    | <40     |

Benches in `crates/hjkl-engine/benches/` and `crates/hjkl-buffer/benches/`.
Memory budget: rope must handle 1GB buffers without OOM on 16GB host; streaming
load not required for 0.0.1. Allocation count tracked via `dhat` or `cap`
allocator wrapper; matters more than wall time for wasm.

## Undo Model

Hybrid: implicit grouping by mode boundaries (vim semantics) + explicit API for
hosts. Storage is a **tree**, not stack — vim's `g-`/`g+` and `:undo {n}`
require branching history. Matches what vim, helix, and kakoune all converge on.

### Implicit grouping (vim semantics, ~95% of cases)

| Trigger                                                          | Action                                          |
| ---------------------------------------------------------------- | ----------------------------------------------- |
| Enter Insert (`i`,`a`,`o`,`I`,`A`,`O`,`s`,`S`,`c{motion}`)       | open group                                      |
| Leave Insert (`<Esc>`, `Ctrl-[`)                                 | close group                                     |
| Operator + motion completes (`d{motion}`, `y{motion}`, `>{...}`) | one group                                       |
| Single normal-mode edit (`x`, `X`, `D`, `C`, `r`, `~`)           | one group                                       |
| `.` (dot-repeat)                                                 | one group regardless of underlying complexity   |
| Paste (`p`, `P`)                                                 | one group                                       |
| Macro replay (`@{reg}`)                                          | wraps entire replay in one group (configurable) |
| `:s/.../.../` substitute                                         | one group for entire range                      |

Mental model: **one user-facing action = one undo step**.

### Explicit API (hosts + complex ops)

```rust
impl Editor {
    pub fn begin_undo_group(&mut self);
    pub fn end_undo_group(&mut self);
    pub fn with_undo_group<F: FnOnce(&mut Self) -> R, R>(&mut self, f: F) -> R;
}
```

Use cases: formatter (100 edits → 1 step), LSP rename, snippet expansion,
multi-cursor edit (engine wraps automatically).

Reentrance: nested `begin/begin/end/end` flattens to outermost via counter (not
stack). Inner begin/end no-ops if a group is already open.

### Storage: undo tree

```rust
pub struct UndoTree {
    nodes: Vec<UndoNode>,
    head: NodeId,
    seq: u64,                  // monotonic, used by g-/g+
}
pub struct UndoNode {
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    seq: u64,
    timestamp: Duration,       // from Host::now()
    edits: Vec<Edit>,
    selections_before: SelectionSet,
    selections_after: SelectionSet,
}
```

`u`/`Ctrl-r` walk the main spine (current branch). `g-`/`g+` traverse by `seq`
(time-based) across branches. `:undo {n}` jumps to absolute seq.

### Selection restoration

Undo restores selections too, not just text. Critical for multicursor — undo
with N=5 selections restores all 5. `selections_before` and `selections_after`
per node enable round-trip.

### Edge cases

- **Insert with motion**: `i hello <Left><Left> X <Esc>` produces multiple undo
  points in vim (motion breaks group). Configurable via
  `Options::undo_break_on_motion: bool` (default `true`, matches vim).
- **Empty groups**: `i<Esc>` with no edits → dropped, not recorded.
- **External edits**: `apply_external_edit()` always opens its own group. Never
  merges into a user group.
- **Macro recording**: capturing inputs does NOT produce undo entries.
- **Macro replay**: produces ONE group covering all replayed actions. Override
  via host setting if per-action grouping wanted.
- **Undo branch cleanup**: tree grows unbounded over a session. Bound via
  `Options::undo_levels` (default 1000 nodes; oldest pruned). Vim semantics.
- **Read-only buffers**: undo tree still accumulates external edits; user-action
  edits rejected before reaching undo.

### Why hybrid (not stack-only or pure-explicit)

- **Stack-only**: can't do `g-`/`g+`. Loses time-travel users.
- **Pure-explicit**: tedious for hosts; easy to forget; engine already knows
  mode boundaries — let it group.

## Pre-flight Audit (Day 0)

**Status: COMPLETE** as of 2026-04-26. Findings recorded in `AUDIT.md`. All
checks below were run; numbers replaced estimates throughout this plan.

Before touching code, catalog coupling:

- Grep every `ed.buffer_mut().X()` and `ed.buffer().X()` in `sqeel-vim/src/`.
  List unique methods. This becomes `hjkl-engine::Buffer` trait surface.
- Grep `ratatui::` and `crossterm::` usage. Tag each as: feature-gateable,
  replaceable, or removable. Note any `ratatui::Style` leakage into public APIs
  — those become engine-native `Style` callsites.
- Locate sqeel-specific bits: LSP intent in `editor.rs`, syntax fold ranges,
  anything calling `sqeel-core` types. List for relocation.
- Confirm test count baseline: `cargo test -p sqeel-vim 2>&1 | tail -3`. Record
  number; don't regress.
- **Bench baseline**: `cargo bench -p sqeel-vim` (or add criterion harness if
  none exists) for: rope insert at cursor, 1k-line scroll, FSM step on `100dd`,
  search-next on 10k-line buffer. Record numbers; don't regress >5% after trait
  extraction.
- **Clipboard usage audit**: grep clipboard calls in sqeel. Map each to write
  (fire-and-forget) or read (cached) path. Note any callsite that currently
  awaits a clipboard result — those need refactoring to use the cache or trigger
  a host refresh first.
- **`sqeel-buffer` SQL extension audit**: enumerate concrete additions over a
  generic rope (token-aware cursor? autocomplete hooks? SQL-aware
  word-boundary?). Decide: extension trait `SqlBuffer: Buffer + …` in sqeel, or
  relocate as plain helpers. Drives Phase 8 sqeel-buffer disposition.
- **Dot-repeat capture audit**: confirm sqeel's current capture is parsed-action
  (not raw input). If raw, plan refactor — multicursor replay needs parsed form
  to fan out across selections.
- **`Editor` constructor ergonomics**: list every existing call site
  constructing `Editor`. Define new `Editor::new(buffer, host, options)` +
  builder. sqeel migration changes one call site instead of many.
- **crates.io name availability**:
  `cargo search hjkl-engine hjkl-buffer hjkl-editor hjkl-ratatui`. Squat any
  free names now (publish 0.0.0 placeholder if needed).
- **License audit**:
  `grep license sqeel-vim/Cargo.toml sqeel-buffer/Cargo.toml`. Confirm MIT or
  MIT-compatible. Code transfer needs clean license. If sqeel is not MIT, either
  dual-license sqeel or pick matching license for hjkl.
- **Tag pre-extraction sha**: in sqeel main, tag `pre-hjkl-extraction` before
  phase 3 deletions. Rollback path if 0.0.x ships broken.
- **`editor.rs` size audit**: `wc -l sqeel-vim/src/editor.rs`. If >2000 lines,
  plan pre-split refactor inside phase 3 to land smaller modules.
- **buffr current state audit**: read `buffr/PLAN.md` and inventory
  `buffr-modal` crate's current modal logic. If buffr ships its own modal layer
  today, phase 10 dep swap is a behavior change for buffr users. Document
  delta + migration in changelog.
- **sqeel-tui / sqeel-gui coordination**: confirm both live in same workspace +
  git history. If split, phase 8 needs two coordinated PRs: tui first, gui
  second, both merged before any `sqeel-vim` deletion.
- **Test migration recipe**: count test files using `Editor::*` constructors
  (`grep -rn "Editor::new" sqeel-vim/`). Document concrete codemod:
  - Old: `let mut ed = Editor::new(rope);`
  - New:
    `let mut ed = Editor::new(TestBuffer::from(rope), NoopHost, Options::default());`
  - Ship `hjkl-editor::testing::{TestBuffer, NoopHost}` behind
    `feature = "testing"`.
  - Estimate: N test files × 5 min = migration time. Without recipe, phase 5
    estimate doubles.
- **Vim compat oracle decision**: pick headless-vim diff (preferred),
  hand-curated `:help` tests, or trust-existing-tests. Headless-vim diff
  recommended: run `vim --clean -e -c '...'` on ~50 canonical cases in cron CI;
  hand-curated for the rest. Sets the strength of the "vim-compatible" claim.

## Phase 1 — Bootstrap hjkl Repo

- [ ] `gh repo create kryptic-sh/hjkl --public`.
- [ ] Workspace `Cargo.toml`: 4 crate members, shared `[workspace.package]`
      (version 0.0.1, edition 2021, `rust-version` = current stable (~1.95),
      license MIT, repository, authors).
- [ ] Shared `[workspace.dependencies]`: `regex`, `serde`, `thiserror`,
      `tracing`, `bitflags` (for `Style`), `unicode-segmentation` (graphemes).
- [ ] `CONTRIBUTING.md` documents MSRV policy: tracks stable, bump freely, log
      in CHANGELOG.
- [ ] `rustfmt.toml`, `.gitignore`, `LICENSE` (MIT), `README.md`,
      `CHANGELOG.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`.
- [ ] `.github/`: issue templates (bug, feature, design proposal), PR template,
      `CODEOWNERS`.
- [ ] `#![forbid(unsafe_code)]` in `hjkl-engine`, `hjkl-editor`, `hjkl-ratatui`
      crate roots. `hjkl-buffer` opts out (rope perf); each `unsafe` block
      requires safety comment + miri test.
- [ ] CI: GitHub Actions —
  - **Per-PR (fast path)**:
    - Test matrix: `[ubuntu-latest, macos-latest, windows-latest]`. All three on
      every PR + push. sqeel/buffr ship on macOS + linux; Windows covered for
      crates.io consumers.
    - `cargo fmt --check`
    - `cargo clippy --all-targets --all-features -D warnings`
    - `cargo test --all-features` (stable only)
    - `cargo test --doc --all-features`
    - `cargo build --examples --all-features` (catch stale examples).
    - `cargo build -p hjkl-engine --no-default-features` (no_std smoke)
    - `cargo build -p hjkl-engine --no-default-features --target wasm32-unknown-unknown`
    - `cargo bench --bench budgets -- --save-baseline pr` (asserts perf budgets;
      fails if exceeded).
  - **Cron / nightly**:
    - Beta toolchain build + test.
    - **Nightly toolchain**: `cargo fuzz run` for 10 minutes per harness
      (cargo-fuzz requires nightly — isolated to this job only).
    - Full criterion bench suite vs `main` baseline; post diff to commit status.
      Baseline artifact uploaded per commit on `main`.
    - `cargo miri test -p hjkl-buffer` (rope unsafe blocks).
    - `cargo deny check` (licenses + RUSTSEC advisories).
    - Headless vim diff: ~50 canonical cases run against real vim, compare
      buffer state.
    - wasm bundle size: `wasm-opt -Os` then `wc -c`; fail if `hjkl-engine`
      gzipped wasm exceeds 100 KB.
    - Feature combination matrix (valid combos from features doc).
  - **Post-0.1.0**:
    - `cargo semver-checks` gate (warn-only pre-0.1.0).
- [ ] `release-plz` GitHub Action configured for lockstep version bumps +
      changelog + crates.io publish. Manual approval gate on release PR.
- [ ] `dependabot.yml` for automated dep updates (weekly cargo + actions).
- [ ] `cargo deny` config: license allowlist (MIT, Apache-2.0, BSD-3, MPL-2.0),
      advisory check via RUSTSEC. Cron job.
- [ ] `[package.metadata.docs.rs]` per crate: `all-features = true`,
      `targets = ["x86_64-unknown-linux-gnu"]` (skip wasm to avoid build
      failures on docs.rs).
- [ ] `rust-toolchain.toml` pins local dev toolchain to current stable.
- [ ] `Cargo.lock` committed (workspace has binary `apps/hjkl/`).
- [ ] `[profile.bench]`: `lto = "fat"`, `codegen-units = 1`, `opt-level = 3` for
      reproducible budget enforcement.
- [ ] Cargo features matrix documented: valid combinations table in
      `hjkl-engine/README.md`. CI tests each valid combo (cron):
  - `default` (no features)
  - `crossterm`
  - `serde` (snapshot serialization)
  - `testing` (test fixtures)
  - All-of-above for std-only consumers.
- [ ] `proptest-regressions/` directories committed (failing seeds replayed in
      CI).
- [ ] CHANGELOG follows [Keep a Changelog](https://keepachangelog.com) format.
      `release-plz` configured to match.
- [ ] Golden snapshot tests use `insta` crate. `cargo insta review` for
      interactive regen; `INSTA_UPDATE=always cargo test` for batch.
- [ ] Allocation tracking: `dhat-rs` benches for FSM step + render frame
      allocation count. Fails CI if budget exceeded.
- [ ] Trait surface size check: `cargo public-api` (warn-only pre-0.1.0) tracks
      count; fails if total trait methods >40.
- [ ] Workspace docs: top-level README links to each crate's rustdoc on docs.rs.
      `cargo doc --workspace --no-deps --open` for local dev.
- [ ] `Buffer` trait implementer guide in `hjkl-engine/IMPLEMENTERS.md`:
      enumerates invariants every `Buffer` impl must hold (`len()` =
      sum-of-lines + newlines, grapheme bounds, etc.).
- [ ] Initial empty crate skeletons compile.

## Phase 2 — Move `sqeel-buffer` → `hjkl-buffer`

- [ ] Use `git subtree split --prefix=sqeel-buffer -b extract-buffer` in sqeel
      repo to preserve history.
- [ ] Pull that branch into `hjkl/crates/hjkl-buffer/`.
- [ ] Rename package: `sqeel-buffer` → `hjkl-buffer`. Adjust `Cargo.toml` paths.
- [ ] Audit deps. Strip ratatui-isms if any. Tests pass standalone.
- [ ] `cargo test -p hjkl-buffer` green.
- [ ] Verify `git log --follow crates/hjkl-buffer/src/lib.rs` shows full
      history.

## Phase 3 — Pre-Split in sqeel, Then Subtree-Split Per Crate

History preservation requires the source layout match the destination shape
**before** subtree-split. Do the file reorg in sqeel first, in a single commit,
then split each subdir.

- [ ] In sqeel repo, branch `vim-presplit`. Move files:
  - `sqeel-vim/src/{vim.rs, input.rs, registers.rs, style.rs}` → new crate
    `sqeel-vim-engine/`.
  - `sqeel-vim/src/{editor.rs, ex.rs, lib.rs glue}` → `sqeel-vim-editor/`.
  - Update sqeel workspace + paths. Tests still pass on this branch.
- [ ] Commit reorg as one commit. Message:
      `refactor: pre-split vim crate for hjkl extraction`.
- [ ] `git subtree split --prefix=sqeel-vim-engine -b extract-engine`.
- [ ] `git subtree split --prefix=sqeel-vim-editor -b extract-editor`.
- [ ] Pull each branch into `hjkl/crates/hjkl-engine/` and
      `hjkl/crates/hjkl-editor/` respectively.
- [ ] Rename packages. `hjkl-editor` depends on `hjkl-engine` + `hjkl-buffer`.
- [ ] Run baseline: tests + benches match.

## Phase 4 — Decouple `ratatui` / `crossterm`

- [ ] Replace any `ratatui::style::Style` in engine/editor public API with
      `hjkl_engine::Style` (plain struct, no ratatui dep).
- [ ] `From<crossterm::event::KeyEvent> for Input` lives in `hjkl-engine` under
      `#[cfg(feature = "crossterm")]`.
- [ ] All ratatui adapters move to new `hjkl-ratatui` crate. It depends on
      `hjkl-engine` and re-exports conversion impls.
- [ ] Default features = `[]`. sqeel turns on `crossterm` + uses `hjkl-ratatui`.
      buffr turns on neither.
- [ ] CI verifies: `cargo build -p hjkl-engine --no-default-features` green,
      wasm32 build green.

## Phase 5 — Extract Trait Hierarchy in `hjkl-engine`

- [ ] Core types:
  - `pub struct Pos { line: u32, col: u32 }` (graphemes, not bytes).
  - `pub struct Selection { anchor: Pos, head: Pos }`. Empty = `anchor==head`.
  - `pub struct SelectionSet { items: Vec<Selection>, primary: usize }`.
- [ ] Sub-traits operate on positions; `Editor` orchestrates selection-set
      fan-out:
  - `pub trait Cursor: Send`: pos validation, line/column conversion, grapheme
    byte-offset.
  - `pub trait Query: Send`: line_count, line_text, char_at, len, slice.
  - `pub trait Edit: Send`: `insert_at(pos, &str)`, `delete_range(Range)`,
    `replace_range(Range, &str)`. Engine fans out edits across selections in
    reverse byte order to keep offsets valid.
  - `pub trait Search: Send`: find_next, find_prev, regex_match (uses `regex`
    crate; smartcase honored at editor layer via `Options`).
  - `pub trait Buffer: Cursor + Query + Edit + Search + sealed::Sealed + Send {}`.
- [ ] Define `pub trait Host: Send`:
  - `type Intent;` (default `()`)
  - `fn write_clipboard(&mut self, text: String)` — fire-and-forget; host
    queues + flushes async.
  - `fn read_clipboard(&mut self) -> Option<String>` — returns cached value;
    host refreshes async out-of-band.
  - `fn prompt_search(&mut self) -> Option<String>`
  - `fn emit_intent(&mut self, intent: Self::Intent)`
  - `fn emit_cursor_shape(&mut self, shape: CursorShape)` — separate from custom
    intent; always present.
- [ ] `Editor<B: Buffer, H: Host>` owns `SelectionSet`, `Mode`, `Options`,
      `Keymap`, `Marks`, `JumpList`, `ChangeList`, `Registers`, `MacroRecorder`,
      `DotRepeat`. Multicursor primitive: every operator + motion fans out.
- [ ] Conflict resolution: overlapping selections merged before edit. Adjacent
      selections after edit configurable (merge default).
- [ ] Undo records full `SelectionSet` per step (restored on undo).
- [ ] `hjkl-buffer::Rope` implements all four sub-traits + `Buffer`.
- [ ] Existing tests: provide a `TestBuffer` + `NoopHost` in dev-dependencies of
      `hjkl-editor`. Migrate test helpers.
- [ ] **Property tests** with `proptest`: FSM step preserves invariants
      (selections in bounds, no overlaps after merge, mode transitions valid,
      dot-repeat reproduces, multi-selection edits commute under reverse
      ordering).
- [ ] **Fuzz target**: `cargo fuzz` harness for FSM input stream — no panics on
      arbitrary keystroke sequences with N selections.
- [ ] Re-run benches; flag any regression >5% (dynamic dispatch is the usual
      culprit — switch to monomorphization if so). Add benches for N=10 and
      N=100 selection edits.
- [ ] All baseline tests pass.

## Phase 6 — Strip sqeel-specific bits

- [ ] LSP intent: removed from engine. sqeel sets
      `Host::Intent =     SqeelIntent` and routes to its LSP layer.
- [ ] Syntax fold ranges: move out of `hjkl-editor` into a sqeel adapter. Engine
      exposes a `FoldProvider` trait method on `Host` that returns
      `Vec<Range<usize>>`.
- [ ] No `sqeel-core` or `sqeel-buffer` mention left in `hjkl-*`. Grep to
      confirm.

## Phase 7 — Docs + Stability Contract

- [ ] Rustdoc on every public item in `hjkl-engine`. `#![deny(missing_docs)]`.
- [ ] Each crate README includes a minimal working example (compiled via
      `doc-tests`).
- [ ] `hjkl-engine/README.md` documents the trait surface as the **stability
      contract** — what `Buffer` and `Host` impls must guarantee.
- [ ] `CHANGELOG.md` documents extraction provenance and 0.0.x churn policy.

## Phase 8 — Pre-Publish Migration via Path Deps

Publishing before migrating leaves sqeel depending on unreleased crates. Migrate
sqeel against path deps first, then publish, then swap.

- [ ] sqeel root `Cargo.toml` patch table:

  ```toml
  [patch.crates-io]
  hjkl-engine = { path = "../hjkl/crates/hjkl-engine" }
  hjkl-buffer = { path = "../hjkl/crates/hjkl-buffer" }
  hjkl-editor = { path = "../hjkl/crates/hjkl-editor" }
  ```

- [ ] sqeel-tui / sqeel-gui swap `sqeel-vim` dep for `hjkl-editor`.
- [ ] sqeel impls `hjkl_engine::Host` (Intent = SqeelIntent) for LSP +
      clipboard + fold plumbing.
- [ ] sqeel-buffer: if SQL-specific extensions exist, keep as thin wrapper
      around `hjkl-buffer`. Otherwise delete and use `hjkl-buffer` directly.
- [ ] sqeel test suite green. Manual smoke: vim modes, ex commands, dot-repeat,
      clipboard, LSP intent.
- [ ] Bench parity check against pre-extraction baseline.

## Phase 9 — Publish 0.0.1 + Version Swap

- [ ] `cargo publish --dry-run` for each crate. Fix metadata gaps.
- [ ] Publish in dep order: `hjkl-buffer` → `hjkl-engine` → `hjkl-editor` →
      `hjkl-ratatui`.
- [ ] Tag `v0.0.1` on hjkl repo.
- [ ] In sqeel: drop `[patch.crates-io]` block, pin to `=0.0.1` (exact match —
      `^0.0.1` won't accept `0.0.2`).
- [ ] sqeel CI green against published crates.
- [ ] Delete `sqeel-vim-engine` / `sqeel-vim-editor` directories from sqeel.
      Commit.

## Phase 10 — buffr Consumes

- [ ] buffr `Cargo.toml`: `hjkl-engine = "=0.0.1"`, `hjkl-buffer = "=0.0.1"`, no
      default features (no crossterm/ratatui).
- [ ] buffr's `buffr-modal` crate becomes thin: page-mode action enum +
      dispatcher only. Edit-mode delegates to `hjkl-editor` via mirrored buffer.
- [ ] Verify wasm32 build of buffr still green.
- [ ] Update buffr `PLAN.md` to reflect this.

## Phase 11 — hjkl Binary (Future, Not Blocking)

- [ ] `apps/hjkl/`: TUI vim clone. Consumes `hjkl-editor` + `hjkl-ratatui` with
      `crossterm` feature.
- [ ] Out of scope for extraction; track separately.

## Promotion to 0.1.0

After ≥1 month of churn-free use across sqeel + buffr:

- [ ] No breaking changes for 4+ weeks.
- [ ] Trait surface reviewed and documented.
- [ ] `cargo public-api` baseline taken at 0.1.0 (NOT earlier — pre-0.1.0 churn
      would invalidate baseline every PR).
- [ ] Snapshot `version` bumped if format changed during 0.0.x.
- [ ] Bump all crates to `0.1.0`. From here, semver applies normally.
- [ ] Document yank procedure in `CONTRIBUTING.md`:
      `cargo yank --version     X.Y.Z` for broken releases (yank ≠ delete;
      consumers on `=X.Y.Z` still resolve).

## Risks & Mitigations

| Risk                                              | Mitigation                                                         |
| ------------------------------------------------- | ------------------------------------------------------------------ |
| sqeel-buffer leaks SQL semantics into trait       | Audit Phase 2; relocate SQL bits to sqeel side                     |
| ratatui/crossterm removal breaks compilation      | Feature-gate gradually; CI builds no-default + wasm                |
| Trait extraction surfaces design flaws            | Sub-traits + sealed; 0.0.x lockstep churn                          |
| sqeel test regressions                            | Baseline count; run after every phase                              |
| Bench regression from dynamic dispatch            | Bench baseline phase 0; monomorphize if >5% regression             |
| Pre-split history loss                            | Do file reorg as single commit before subtree-split                |
| `^0.1` semver traps callers on breaking changes   | Use 0.0.x lockstep until trait stable; pin with `=` in callers     |
| Clipboard write blocks main thread                | Fire-and-forget write API; host queues + flushes async             |
| Clipboard read returns stale value                | Host refreshes cache on focus/OSC52 reply; matches neovim/helix    |
| no_std/wasm regression slips in                   | CI builds wasm32 + no-default-features every PR                    |
| Publish-before-migrate strands sqeel              | Path-dep migration in phase 8, publish in phase 9                  |
| Multicursor adds complexity engine doesn't need   | Vim mode hides it (primary only); selection set is internal        |
| Edit fan-out corrupts offsets across selections   | Always iterate selections in reverse byte order; tested in fuzz    |
| `Send` bounds added late break consumers          | Bake `Send` into traits day one; CI builds with `Send` impls       |
| MSRV drift breaks consumers                       | MSRV tracks stable; in-house consumers bump together via lockstep  |
| Accidental breaking change post-0.1.0             | `cargo semver-checks` in CI from 0.1.0 onward                      |
| crates.io name squatted before publish            | Phase 0 audit + 0.0.0 placeholder publish if names available       |
| Plugin requests pile up post-publish              | README states "no plugins in 0.x"; surface is `Host` + `Intent`    |
| Determinism drift breaks fuzz / replay            | No clock reads except `Host::now()`; fuzz asserts pure-fn behavior |
| Tree-sitter dep creeps into engine                | Syntax via `Host::syntax_highlights`; engine only merges ranges    |
| Performance regresses silently                    | Concrete budget table; criterion fails CI on regression            |
| CI minutes blow up                                | Per-PR fast path only; fuzz/bench/miri on cron                     |
| 0.0.x ships broken, sqeel cannot revert           | Tag `pre-hjkl-extraction` on sqeel main before phase 3 deletions   |
| License incompatibility blocks code transfer      | Phase 0 license audit; resolve before phase 2                      |
| Multi-key timeout retrofitted late                | `Host::now()` in trait day one; engine tracks pending sequence     |
| Mouse semantics added late break clients          | `Input::Mouse` variant in enum from 0.0.1                          |
| Push callbacks tangle host re-entrancy            | Pull model: `Editor::take_changes()` per render frame              |
| External edits (LSP rename) desync cursor         | `Editor::apply_external_edit(Edit)` remaps state                   |
| Undo group boundaries inconsistent across modes   | Implicit grouping by mode boundaries + explicit begin/end API      |
| Inc-search recompiles regex per keystroke         | LRU cache (size 32) of compiled `Regex` keyed by pattern string    |
| Long ops un-cancellable                           | `Host::should_cancel()` reserved in 0.0.1; polled in inner loops   |
| Snapshot format breaks across versions            | `version: u32` field in `EditorSnapshot`; bump on incompat change  |
| Default keymap divergence from vim                | Ship `Keymap::vim_defaults()` + golden test suite vs vim behavior  |
| Visual line/block deletes wrong amount            | `SelectionKind::{Char,Line,Block}` from day one                    |
| Wrap-aware `gj`/`gk` broken                       | `Host::display_line_for(pos)` query with logical-default fallback  |
| Paste mangled by insert mappings                  | `Input::Paste` bypasses mappings + abbrev + autoindent             |
| Macro replay non-deterministic with timeouts      | Replay disables time-based mapping resolution                      |
| Allocation count regresses silently in wasm       | dhat benches; ≤1 alloc per FSM step, ≤5 per render frame           |
| Trait surface bloats past sustainable             | Hard cap 40 methods; CI tracks via `cargo public-api`              |
| `Buffer` impls subtly violate invariants          | `IMPLEMENTERS.md` documents every invariant; miri tests verify     |
| Platform-specific bug ships unnoticed             | linux + macOS + Windows all in per-PR matrix                       |
| `Box<dyn Buffer>` request from users post-publish | Generic-only documented up front; sealed prevents dyn anyway       |
| Edits applied in wrong order across selections    | `Edit` ordered reverse byte offset; tested in fuzz                 |
| Stale `examples/` rot in repo                     | `cargo build --examples` per-PR                                    |
| Operator FSM splits per-operator, drifts          | Single grammar module documented as the rule                       |
| Test migration estimate explodes in phase 5       | Codemod recipe in phase 0; `testing` feature ships fixtures        |
| "vim-compatible" claim unfalsifiable              | Headless vim diff in cron CI on ~50 canonical cases                |
| `cargo fuzz` blocked by stable-only policy        | Isolate fuzz to nightly cron job; per-PR stays stable + beta       |
| buffr behavior changes silently on dep swap       | Phase 0 audit + changelog entry documenting modal-layer transition |
| Re-export omission forces 3-dep adoption          | `hjkl-editor` re-exports engine public API                         |
| Feature combo breaks post-publish                 | Document valid feature matrix; cron CI tests each combo            |
| wasm bundle bloats unnoticed                      | Bundle size budget; CI tracks gzipped output                       |
| Bench regression undetectable across CI runs      | Upload baseline artifact on `main`; cron diff vs current PR        |
| Lockfile decision flip-flops                      | Commit `Cargo.lock` from day one (binary in workspace)             |

## Versioning Strategy

- **0.0.x churn phase**: lockstep workspace version. All crates bump together.
  Breaking changes allowed in patch bumps. Callers pin with `=0.0.x`.
- **0.1.0 promotion**: after stability window. From here, breaking changes only
  on minor bumps, semver applies normally.
- **Stability contract**: `hjkl-engine/README.md` documents the trait surface as
  the API. Sealed traits prevent downstream impls until 1.0.

## Definition of Done

- [ ] `hjkl` repo public, CI green (incl. wasm + no-default), 0.0.1 on
      crates.io.
- [ ] sqeel runs entirely on `hjkl-editor`. `sqeel-vim*` directories deleted.
- [ ] sqeel test count matches pre-extraction baseline.
- [ ] sqeel benches within 5% of pre-extraction baseline.
- [ ] buffr `PLAN.md` updated to consume hjkl crates; wasm build green.
- [ ] Rustdoc complete on `hjkl-engine` public surface; `deny(missing_docs)`.
- [ ] `proptest` + `cargo fuzz` harnesses in `hjkl-engine` running in CI.
- [ ] CHANGELOG.md in hjkl documents extraction provenance.

## Estimated Effort

- Phase 0 (audit, benches, Spec Lock, license, name squat, tag, buffr audit,
  test migration recipe, vim-compat oracle decision): 2 days.
- Phase 1–2 (governance files, CI matrix, budget harness, dependabot,
  cargo-deny, features matrix, headless-vim oracle setup): 1.5 days.
- Phase 3–4: 1.5 days.
- Phase 5 (multicursor, sub-traits, Input enum, render frame, Host::now,
  SelectionKind, autoindent, paste mode, fold intents, numbered registers,
  proptest, fuzz, budget benches, alloc benches, test fixture migration,
  IMPLEMENTERS.md): 6 days.
- Phase 6–7: 1 day.
- Phase 8 (sqeel migration: Host impl, multicursor callsites, smoke, bench
  parity, sqeel-tui + sqeel-gui coordination): 2 days.
- Phase 9: 0.5 day.
- Phase 10 (buffr migration with modal-layer transition): 1 day.
- **Total: ~15.5 days focused work.**
