# hjkl-engine — SPEC

**Status: 0.1.0 (frozen 2026-04-27).** This document is the **stability
contract** that the public trait surface guarantees. From 0.1.0 onward, breaking
changes to anything described here require a minor-version bump (per [SemVer]);
patch bumps preserve every signature.

Source: phase 0 audit (`../../AUDIT.md`) + `MIGRATION.md` Spec Lock + the 0.0.33
— 0.0.42 trait-extraction series (Patches C-α through C-δ.7).

[SemVer]: https://semver.org

## What is frozen at 0.1.0

- The `Buffer` super-trait surface — 14 methods across the four sub-traits
  `Cursor` / `Query` / `BufferEdit` / `Search`, sealed via private
  `mod sealed { pub trait Sealed; }`. Pre-0.1.0 churn is over; new methods on
  these traits require a minor bump from 0.1.0 onward.
- The `Host` trait surface — clipboard / time / cancellation / search-prompt /
  display-line bridge / syntax highlights / cursor-shape emit / viewport
  ownership / `Intent` fan-out. `EngineHost` (the pre-0.1.0 dyn-shim) is
  removed; hosts implement `Host` directly and the `Editor<B, H>` generic
  carries the typed slot.
- The `Editor::new(buffer, host, options)` constructor — the legacy
  `Editor::new(KeybindingMode)` / `Editor::with_host` / `Editor::with_options`
  triad is removed.
- The `EditorSnapshot` wire format — `EditorSnapshot::VERSION` (`4`) is locked
  for the entire 0.1.x line.
- The `Options` `:set` surface — vim-faithful defaults (shiftwidth=8 / tabstop=8
  / `iskeyword="@,48-57,_,192-255"` / etc.).
- `FoldOp` / `FoldProvider` (the engine-canonical fold-mutation channel
  introduced in 0.0.38) — both the enum variants and the trait surface.

## Crate boundaries

- `hjkl-engine` (this crate): vim FSM, motion grammar, operator parser,
  registers, undo tree, keymap, options, render frame, traits. `no_std + alloc`.
- `hjkl-buffer`: `Rope` impl of `Buffer` + sub-traits. `std`.
- `hjkl-editor`: glue — `Editor<B: Buffer, H: Host>`. `std`.
- `hjkl-ratatui`: `From<Style>` and `From<KeyEvent>` adapters. `std`.

## Core types

```rust
pub struct Pos { pub line: u32, pub col: u32 }   // graphemes, not bytes

pub enum SelectionKind { Char, Line, Block }
pub struct Selection {
    pub anchor: Pos,
    pub head: Pos,
    pub kind: SelectionKind,
}
pub struct SelectionSet {
    pub items: Vec<Selection>,
    pub primary: usize,
}

pub struct Edit {
    pub range: core::ops::Range<Pos>,
    pub replacement: String,
}

pub enum Mode { Normal, Insert, Visual, Replace, Command, OperatorPending }

pub enum CursorShape { Block, Bar, Underline }
```

## `Buffer` trait surface

Audit found 45 raw methods on sqeel's buffer. After relocating folds (8) and
viewport (3) to `Host`, and moving motion logic (24) to engine FSM functions
(motions don't belong on `Buffer` — they're computed over the buffer, not
delegated to it), the real surface is:

```rust
pub trait Cursor: Send {
    fn cursor(&self) -> Pos;
    fn set_cursor(&mut self, pos: Pos);
    fn byte_offset(&self, pos: Pos) -> usize;
    fn pos_at_byte(&self, byte: usize) -> Pos;
}

pub trait Query: Send {
    fn line_count(&self) -> u32;
    fn line(&self, idx: u32) -> &str;
    fn len_bytes(&self) -> usize;
    fn slice(&self, range: core::ops::Range<Pos>) -> alloc::borrow::Cow<'_, str>;
    fn dirty_gen(&self) -> u64;     // monotonic mutation counter (added 0.0.39)
}

pub trait Edit: Send {
    fn insert_at(&mut self, pos: Pos, text: &str);
    fn delete_range(&mut self, range: core::ops::Range<Pos>);
    fn replace_range(&mut self, range: core::ops::Range<Pos>, replacement: &str);
}

pub trait Search: Send {
    fn find_next(&self, from: Pos, pat: &Regex) -> Option<core::ops::Range<Pos>>;
    fn find_prev(&self, from: Pos, pat: &Regex) -> Option<core::ops::Range<Pos>>;
}

pub trait Buffer:
    Cursor + Query + Edit + Search + sealed::Sealed + Send
{}
```

Total: **14 methods** (13 from sub-traits + `Query::dirty_gen`). Under <40 cap
with room.

Sealed via private `mod sealed { pub trait Sealed {} }`. Pre-1.0, downstream
cannot impl `Buffer` directly. `hjkl-buffer::Rope` implements `Sealed` from
inside the family (allowed via re-export pattern).

## `Host` trait

```rust
pub trait Host: Send {
    type Intent;

    // Clipboard (hybrid: write fire-and-forget, read cached)
    fn write_clipboard(&mut self, text: String);
    fn read_clipboard(&mut self) -> Option<String>;

    // Time (multi-key timeout, no clock reads inside engine)
    fn now(&self) -> core::time::Duration;

    // Cancellation hook (cooperative; engine polls in long loops)
    fn should_cancel(&self) -> bool { false }

    // Search prompt
    fn prompt_search(&mut self) -> Option<String>;

    // Display line ↔ logical line (wrap-aware; default = identity)
    fn display_line_for(&self, pos: Pos) -> u32 { pos.line }
    fn pos_for_display(&self, line: u32, col: u32) -> Pos {
        Pos { line, col }
    }

    // Syntax highlights from host (tree-sitter etc.)
    fn syntax_highlights(&self, range: core::ops::Range<Pos>) -> Vec<Highlight>;

    // Cursor shape change emitted on mode transitions
    fn emit_cursor_shape(&mut self, shape: CursorShape);

    // Custom intent fan-out (LSP, fold ops, buffer switch, etc.)
    fn emit_intent(&mut self, intent: Self::Intent);
}
```

Hosts that don't need an intent type: `type Intent = ();`. sqeel sets
`type Intent = SqeelIntent;` covering LSP variants (`Hover(Pos)`,
`Complete(Pos, char)`, `GotoDef(Pos)`, `Rename(Pos, String)`, `Diagnostic(u32)`,
`FormatRange(Range)`) plus fold ops (`FoldOp::{Open, Close, ToggleAt(line)}`)
plus buffer-list ops (`SwitchBuffer(BufferId)`, `ListBuffers`).

## `Input` enum

```rust
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_: bool,
}

pub enum Input {
    Char(char, Modifiers),
    Key(SpecialKey, Modifiers),
    Mouse(MouseEvent),
    Paste(String),       // bracketed paste; bypasses mappings + abbrev + autoindent
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

pub struct MouseEvent {
    pub kind: MouseKind,
    pub pos: Pos,
    pub mods: Modifiers,
}
pub enum MouseKind { Press, Release, Drag, ScrollUp, ScrollDown }
```

## `Style` (engine-native)

```rust
bitflags::bitflags! {
    pub struct Attrs: u8 {
        const BOLD       = 1 << 0;
        const ITALIC     = 1 << 1;
        const UNDERLINE  = 1 << 2;
        const REVERSE    = 1 << 3;
        const DIM        = 1 << 4;
        const STRIKE     = 1 << 5;
    }
}

pub struct Color(pub u8, pub u8, pub u8);

pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub attrs: Attrs,
}
```

`hjkl-ratatui` provides `From<Style> for ratatui::style::Style` and the inverse.
Engine never imports ratatui.

## `Highlight`

```rust
pub enum HighlightKind {
    Selection,
    SearchMatch,
    IncSearch,
    MatchParen,
    Syntax(u32),     // opaque host-supplied id; styled by host's style table
}

pub struct Highlight {
    pub range: core::ops::Range<Pos>,
    pub kind: HighlightKind,
}
```

## `RenderFrame`

```rust
pub struct RenderFrame<'a> {
    pub mode: Mode,
    pub selections: &'a SelectionSet,
    pub highlights: Vec<Highlight>,    // ≤ 5 allocs per build (budget)
    pub cursor_shape: CursorShape,
    pub mode_indicator: &'a str,
    pub command_line: Option<&'a str>,
    pub search_prompt: Option<&'a str>,
    pub status_line: &'a str,
    pub viewport: Viewport,
}

pub struct Viewport {
    pub top_line: u32,
    pub height: u32,
    pub scroll_off: u32,
}
```

Built per `Editor::render() -> RenderFrame<'_>`. Host diffs frame to frame. No
partial updates from engine.

## `Options`

```rust
pub struct Options {
    pub tabstop: u32,
    pub shiftwidth: u32,
    pub expandtab: bool,
    pub iskeyword: String,
    pub ignorecase: bool,
    pub smartcase: bool,
    pub hlsearch: bool,
    pub incsearch: bool,
    pub wrapscan: bool,
    pub autoindent: bool,
    pub timeout_len: core::time::Duration,
    pub undo_levels: u32,
    pub undo_break_on_motion: bool,
    pub readonly: bool,
}
```

`:set name=value` parser dispatches via
`Options::set_by_name(&str, OptionValue)`.

## Editor surface

```rust
pub struct Editor<B: Buffer = hjkl_buffer::Buffer, H: Host = DefaultHost> {
    /* private */
}

impl<H: Host> Editor<hjkl_buffer::Buffer, H> {
    pub fn new(buffer: hjkl_buffer::Buffer, host: H, options: Options) -> Self;

    // Input dispatch
    pub fn input(&mut self, input: Input) -> Result<(), EngineError>;
    pub fn execute_ex(&mut self, cmd: &str) -> Result<(), EngineError>;

    // Render + change observation
    pub fn render(&self) -> RenderFrame<'_>;
    pub fn take_changes(&mut self) -> Vec<Edit>;

    // External edits (LSP rename, formatter)
    pub fn apply_external_edit(&mut self, edit: Edit);

    // Undo grouping (host-driven)
    pub fn begin_undo_group(&mut self);
    pub fn end_undo_group(&mut self);
    pub fn with_undo_group<F, R>(&mut self, f: F) -> R
    where F: FnOnce(&mut Self) -> R;

    // Snapshot / restore
    pub fn snapshot(&self) -> EditorSnapshot;
    pub fn restore(&mut self, snap: EditorSnapshot) -> Result<(), EngineError>;

    // Accessors
    pub fn buffer(&self) -> &B;
    pub fn buffer_mut(&mut self) -> &mut B;
    pub fn options(&self) -> &Options;
    pub fn options_mut(&mut self) -> &mut Options;
    pub fn selections(&self) -> &SelectionSet;
    pub fn mode(&self) -> Mode;
}
```

## Stability commitments

Pre-0.1.0 (0.0.x): trait surface and method signatures **may change in patch
bumps**. Lockstep workspace version. Callers pin with `=0.0.x`.

0.1.0+: traits sealed, semver applies. Breaking changes require minor bump.
`cargo public-api` baseline taken at 0.1.0 release; CI gates prevent accidental
breakage from 0.1.0 onward.

### Snapshot wire format

`EditorSnapshot::VERSION` (currently `4` — frozen at 0.1.0) tags the serde
payload produced by `Editor::snapshot` / consumed by `Editor::restore`.

- **0.0.x:** `VERSION` bumps with every structural change. Persisted state from
  an older patch release will not round-trip; hosts must reject mismatched
  payloads rather than attempt field-by-field migration.
- **0.1.0:** `VERSION` freezes. Hosts persisting editor state between sessions
  can rely on the wire format being stable for the entire 0.1.x line.
- **0.2.0+:** any further structural change to `EditorSnapshot` requires
  `VERSION++` and a major-version bump of `hjkl-engine`.

## Out of scope at 0.1.0

Explicit non-goals for the 0.1.x line. These remain post-0.1.0 work and **are
not part of the frozen surface** — implementations and call sites are free to
evolve in patch bumps.

### Vim FSM is concrete on `hjkl_buffer::Buffer`

`Editor` is generic over `B: Buffer = hjkl_buffer::Buffer`, but the constructor
(`Editor::new`) and the entire vim FSM (`crate::vim` free functions,
`Editor::mutate_edit`, change-log emission, undo machinery) are bound to
`B = hjkl_buffer::Buffer`:

```rust
impl<H: Host> Editor<hjkl_buffer::Buffer, H> { /* most methods */ }
```

The `<B: Buffer, H: Host>` impl block exposes only universal accessors
(`buffer()` / `buffer_mut()` / `host()` / `host_mut()`). Custom buffer backends
compile against the trait but cannot run the vim FSM at 0.1.0.

The blocker is `Editor::mutate_edit`, which consumes the rich
`hjkl_buffer::Edit` enum (8 variants — `InsertChar`, `InsertStr`, `DeleteRange`,
`JoinLines`, `SplitLines`, `Replace`, `InsertBlock`, `DeleteBlockChunks`) with
~700 LOC of `do_*` machinery. Lifting that onto `BufferEdit` requires an
associated `type Edit;` (forces every backend to design its own rich-edit enum
just to compile) — that's post-0.1.0 work tracked under `BufferEdit::Op`.

The seam between the engine and `hjkl_buffer::Buffer` for the mutate-edit
channel lives at
`crate::buf_helpers::apply_buffer_edit(&mut hjkl_buffer::Buffer, hjkl_buffer::Edit) -> hjkl_buffer::Edit`
— a single concrete reach that 0.2.0 will lift onto a trait associated type
without touching call sites.

### Viewport math on `Buffer`

The viewport-math fns (`ensure_cursor_visible`, `cursor_screen_row`,
`max_top_for_height`) are engine free functions over `B: Query [+ Cursor]` +
`&dyn FoldProvider` + `&Viewport`, lifted out of `hjkl_buffer::Buffer` in
0.0.42. The buffer-side inherent copies survive 0.1.0 only as a compatibility
scaffold; 0.2.0 deletes them once every external consumer migrates to
`crate::viewport_math`.

### `Editor::apply_external_edit` / `Editor::take_changes`

The SPEC §"Editor surface" gestures at `apply_external_edit(edit: Edit)` and
`take_changes() -> Vec<Edit>`. The change-log emitter (drained via
`Editor::take_content_change()`) covers the read-side; the `apply_external_edit`
write-side is wired through `mutate_edit` and is concrete on
`hjkl_buffer::Buffer` for the same associated-type reason above.

See `MIGRATION.md` "Out of Scope" table for the broader "engine never owns" list
(file I/O, terminal I/O, LSP transport, tree-sitter, syntax themes, …).

## Open issues

- `Edit::range` uses `Range<Pos>`. Some operations naturally express in bytes
  (regex matches). Provide `Pos`-byte conversion via `Cursor::byte_offset` /
  `pos_at_byte` — no separate byte-range API.
- `find_next` / `find_prev` take `&Regex` — caller responsible for smartcase
  compilation. `Editor` owns the LRU cache; `Buffer` impls do not cache.
- `Highlight::Syntax(u32)` opaque id requires host-supplied style table.
  Document host responsibility.
