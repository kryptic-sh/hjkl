# hjkl-engine — SPEC

Draft, 2026-04-26. Source: phase 0 audit (`../../AUDIT.md`) + `MIGRATION.md`
Spec Lock. This document is the **stability contract** that the public trait
surface guarantees. Bumps in lockstep with crate version.

Status: **0.0.0 placeholder**. SPEC describes the planned 0.0.1 surface.

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

Total: **13 methods**. Under <40 cap with room.

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
pub struct Editor<B: Buffer, H: Host> { /* private */ }

impl<B: Buffer, H: Host> Editor<B, H> {
    pub fn new(buffer: B, host: H, options: Options) -> Self;

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

## Out of scope (engine never owns)

See `MIGRATION.md` "Out of Scope" table.

## Open issues

- `Edit::range` uses `Range<Pos>`. Some operations naturally express in bytes
  (regex matches). Provide `Pos`-byte conversion via `Cursor::byte_offset` /
  `pos_at_byte` — no separate byte-range API.
- `find_next` / `find_prev` take `&Regex` — caller responsible for smartcase
  compilation. `Editor` owns the LRU cache; `Buffer` impls do not cache.
- `Highlight::Syntax(u32)` opaque id requires host-supplied style table.
  Document host responsibility.
