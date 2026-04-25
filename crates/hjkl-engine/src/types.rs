//! Core types for the planned 0.1.0 trait surface (per `SPEC.md`).
//!
//! These are introduced alongside the legacy sqeel-vim public API. The
//! trait extraction (phase 5) progressively rewires the existing FSM and
//! Editor to operate on `Selection` / `SelectionSet` / `Edit` / `Pos`.
//! Until that work lands, the legacy types in [`crate::editor`] and
//! [`crate::vim`] remain authoritative.

use std::ops::Range;

/// Grapheme-indexed position. `line` is zero-based row; `col` is zero-based
/// grapheme column within that line.
///
/// Note that `col` counts graphemes, not bytes or chars. Motions and
/// rendering both honor grapheme boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

impl Pos {
    pub const ORIGIN: Pos = Pos { line: 0, col: 0 };

    pub const fn new(line: u32, col: u32) -> Self {
        Pos { line, col }
    }
}

/// What kind of region a [`Selection`] covers.
///
/// - `Char`: classic vim `v` selection — closed range on the inline character
///   axis.
/// - `Line`: linewise (`V`) — anchor/head columns ignored, full lines covered
///   between `min(anchor.line, head.line)` and `max(...)`.
/// - `Block`: blockwise (`Ctrl-V`) — rectangle from `min(col)` to `max(col)`,
///   each line a sub-range. Falls out of multi-cursor model: implementations
///   may expand a `Block` selection into N sub-selections during edit
///   dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SelectionKind {
    #[default]
    Char,
    Line,
    Block,
}

/// A single anchored selection. Empty (caret-only) when `anchor == head`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Selection {
    pub anchor: Pos,
    pub head: Pos,
    pub kind: SelectionKind,
}

impl Selection {
    /// Caret at `pos` with no extent.
    pub const fn caret(pos: Pos) -> Self {
        Selection {
            anchor: pos,
            head: pos,
            kind: SelectionKind::Char,
        }
    }

    /// Inclusive range `[anchor, head]` (or reversed) as a `Char` selection.
    pub const fn char_range(anchor: Pos, head: Pos) -> Self {
        Selection {
            anchor,
            head,
            kind: SelectionKind::Char,
        }
    }

    /// True if `anchor == head`.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// Ordered set of selections. Always non-empty in valid states; `primary`
/// indexes the cursor visible to vim mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionSet {
    pub items: Vec<Selection>,
    pub primary: usize,
}

impl SelectionSet {
    /// Single caret at `pos`.
    pub fn caret(pos: Pos) -> Self {
        SelectionSet {
            items: vec![Selection::caret(pos)],
            primary: 0,
        }
    }

    /// Returns the primary selection, or the first if `primary` is out of
    /// bounds.
    pub fn primary(&self) -> &Selection {
        self.items
            .get(self.primary)
            .or_else(|| self.items.first())
            .expect("SelectionSet must contain at least one selection")
    }
}

impl Default for SelectionSet {
    fn default() -> Self {
        SelectionSet::caret(Pos::ORIGIN)
    }
}

/// A pending or applied edit. Multi-cursor edits fan out to `Vec<Edit>`
/// ordered in **reverse byte offset** so each entry's positions remain valid
/// after the prior entry applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<Pos>,
    pub replacement: String,
}

impl Edit {
    pub fn insert(at: Pos, text: impl Into<String>) -> Self {
        Edit {
            range: at..at,
            replacement: text.into(),
        }
    }

    pub fn delete(range: Range<Pos>) -> Self {
        Edit {
            range,
            replacement: String::new(),
        }
    }

    pub fn replace(range: Range<Pos>, text: impl Into<String>) -> Self {
        Edit {
            range,
            replacement: text.into(),
        }
    }
}

/// Vim editor mode. Distinct from the legacy [`crate::VimMode`] — that one
/// is the host-facing status-line summary; this is the engine's internal
/// state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Visual,
    Replace,
    Command,
    OperatorPending,
}

/// Cursor shape intent emitted on mode transitions. Hosts honor it via
/// `Host::emit_cursor_shape` once the trait extraction lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Bar,
    Underline,
}

/// Engine-native style. Replaces direct ratatui `Style` use in the public
/// API once phase 5 trait extraction completes; until then both coexist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub attrs: Attrs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Color(pub u8, pub u8, pub u8);

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
    pub struct Attrs: u8 {
        const BOLD       = 1 << 0;
        const ITALIC     = 1 << 1;
        const UNDERLINE  = 1 << 2;
        const REVERSE    = 1 << 3;
        const DIM        = 1 << 4;
        const STRIKE     = 1 << 5;
    }
}

/// Highlight kind emitted by the engine's render pass. The host's style
/// resolver picks colors for `Selection`/`SearchMatch`/etc.; `Syntax(id)`
/// carries an opaque host-supplied id whose styling lives in the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    Selection,
    SearchMatch,
    IncSearch,
    MatchParen,
    Syntax(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Highlight {
    pub range: Range<Pos>,
    pub kind: HighlightKind,
}

/// Editor settings surfaced via `:set`. Per SPEC. Consumed once trait
/// extraction lands; today's legacy `Settings` (in [`crate::editor`])
/// continues to drive runtime behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Display width of `\t` for column math + render. Default 8.
    pub tabstop: u32,
    /// Spaces per shift step (`>>`, `<<`, `Ctrl-T`, `Ctrl-D`).
    pub shiftwidth: u32,
    /// Insert spaces (`true`) or literal `\t` (`false`) for the Tab key.
    pub expandtab: bool,
    /// Characters considered part of a "word" for `w`/`b`/`*`/`#`.
    /// Default `"@,48-57,_,192-255"` (ASCII letters, digits, `_`, plus
    /// extended Latin); host may override per language.
    pub iskeyword: String,
    /// Default `false`: search is case-sensitive.
    pub ignorecase: bool,
    /// When `true` and `ignorecase` is `true`, an uppercase letter in the
    /// pattern flips back to case-sensitive for that search.
    pub smartcase: bool,
    /// Highlight all matches of the last search.
    pub hlsearch: bool,
    /// Incrementally highlight matches while typing the search pattern.
    pub incsearch: bool,
    /// Wrap searches around the buffer ends.
    pub wrapscan: bool,
    /// Copy previous line's leading whitespace on Enter in insert mode.
    pub autoindent: bool,
    /// Multi-key sequence timeout (e.g., `<C-w>v`). Vim's `timeoutlen`.
    pub timeout_len: core::time::Duration,
    /// Maximum undo-tree depth. Older entries pruned.
    pub undo_levels: u32,
    /// Break the current undo group on cursor motion in insert mode.
    /// Matches vim default; turn off to merge multi-segment edits.
    pub undo_break_on_motion: bool,
    /// Reject every edit. `:set ro` sets this; `:w!` clears it.
    pub readonly: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            tabstop: 8,
            shiftwidth: 8,
            expandtab: false,
            iskeyword: "@,48-57,_,192-255".to_string(),
            ignorecase: false,
            smartcase: false,
            hlsearch: true,
            incsearch: true,
            wrapscan: true,
            autoindent: true,
            timeout_len: core::time::Duration::from_millis(1000),
            undo_levels: 1000,
            undo_break_on_motion: true,
            readonly: false,
        }
    }
}

/// Visible region of a buffer. The host writes `top_line` and `height`
/// per render frame; the engine reads to decide where the cursor must
/// land for visibility (cf. `scroll_off`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Viewport {
    pub top_line: u32,
    pub height: u32,
    pub scroll_off: u32,
}

/// Opaque buffer identifier owned by the host. Engine echoes it back
/// in [`Host::Intent`] variants for buffer-list operations
/// (`SwitchBuffer`, etc.). Generation is the host's responsibility.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferId(pub u64);

/// Modifier bits accompanying every keystroke.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_: bool,
}

/// Special key codes — anything that isn't a printable character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SpecialKey {
    Esc,
    Enter,
    Backspace,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseKind {
    Press,
    Release,
    Drag,
    ScrollUp,
    ScrollDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub pos: Pos,
    pub mods: Modifiers,
}

/// Single input event handed to the engine.
///
/// `Paste` content bypasses insert-mode mappings, abbreviations, and
/// autoindent; the engine inserts the bracketed-paste payload as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Input {
    Char(char, Modifiers),
    Key(SpecialKey, Modifiers),
    Mouse(MouseEvent),
    Paste(String),
    FocusGained,
    FocusLost,
    Resize(u16, u16),
}

/// Host adapter consumed by the engine. Lives behind the planned
/// `Editor<B: Buffer, H: Host>` generic; today it's the contract that
/// `buffr-modal::BuffrHost` and the (future) `sqeel-tui` Host impl
/// align against.
///
/// Methods with default impls return safe no-ops so hosts that don't
/// need a feature (cancellation, wrap-aware motion, syntax highlights)
/// can ignore them.
pub trait Host: Send {
    /// Custom intent type. Hosts that don't fan out actions back to
    /// themselves can use the unit type via the default impl approach
    /// (set associated type explicitly).
    type Intent;

    // ── Clipboard (hybrid: write fire-and-forget, read cached) ──

    /// Fire-and-forget clipboard write. Engine never blocks; the host
    /// queues internally and flushes on its own task (OSC52, `wl-copy`,
    /// `pbcopy`, …).
    fn write_clipboard(&mut self, text: String);

    /// Returns the last-known cached clipboard value. May be stale —
    /// matches the OSC52/wl-paste model neovim and helix both ship.
    fn read_clipboard(&mut self) -> Option<String>;

    // ── Time + cancellation ──

    /// Monotonic time. Multi-key timeout (`timeoutlen`) resolution
    /// reads this; engine never reads `Instant::now()` directly so
    /// macro replay stays deterministic.
    fn now(&self) -> core::time::Duration;

    /// Cooperative cancellation. Engine polls during long search /
    /// regex / multi-cursor edit loops. Default returns `false`.
    fn should_cancel(&self) -> bool {
        false
    }

    // ── Search prompt ──

    /// Synchronously prompt the user for a search pattern. Returning
    /// `None` aborts the search.
    fn prompt_search(&mut self) -> Option<String>;

    // ── Wrap-aware motion (default: wrap is identity) ──

    /// Map a logical position to its display line for `gj`/`gk`. Hosts
    /// without wrapping may use the default identity impl.
    fn display_line_for(&self, pos: Pos) -> u32 {
        pos.line
    }

    /// Inverse of [`display_line_for`]. Default identity.
    fn pos_for_display(&self, line: u32, col: u32) -> Pos {
        Pos { line, col }
    }

    // ── Syntax highlights (default: none) ──

    /// Host-supplied syntax highlights for `range`. Empty by default;
    /// hosts wire tree-sitter or LSP semantic tokens here.
    fn syntax_highlights(&self, range: Range<Pos>) -> Vec<Highlight> {
        let _ = range;
        Vec::new()
    }

    // ── Cursor shape ──

    /// Engine emits this on every mode transition. Hosts repaint the
    /// cursor in the requested shape.
    fn emit_cursor_shape(&mut self, shape: CursorShape);

    // ── Custom intent fan-out ──

    /// Host-defined event the engine raises (LSP request, fold op,
    /// buffer switch, …).
    fn emit_intent(&mut self, intent: Self::Intent);
}

/// Errors surfaced from the engine to the host. Intentionally narrow —
/// callsites that fail in user-facing ways return `Result<_,
/// EngineError>`; internal invariant breaks use `debug_assert!`.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// `:s/pat/.../` couldn't compile the pattern. Host displays the
    /// regex error in the status line.
    #[error("regex compile error: {0}")]
    Regex(#[from] regex::Error),

    /// `:[range]` parse failed.
    #[error("invalid range: {0}")]
    InvalidRange(String),

    /// Ex command parse failed (unknown command, malformed args).
    #[error("ex parse: {0}")]
    Ex(String),

    /// Edit attempted on a read-only buffer.
    #[error("buffer is read-only")]
    ReadOnly,

    /// Position passed by the caller pointed outside the buffer.
    #[error("position out of bounds: {0:?}")]
    OutOfBounds(Pos),

    /// Snapshot version mismatch. Host should treat as "abandon
    /// snapshot" rather than attempt migration.
    #[error("snapshot version mismatch: file={0}, expected={1}")]
    SnapshotVersion(u32, u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_is_empty() {
        let sel = Selection::caret(Pos::new(2, 4));
        assert!(sel.is_empty());
        assert_eq!(sel.anchor, sel.head);
    }

    #[test]
    fn selection_set_default_has_one_caret() {
        let set = SelectionSet::default();
        assert_eq!(set.items.len(), 1);
        assert_eq!(set.primary, 0);
        assert_eq!(set.primary().anchor, Pos::ORIGIN);
    }

    #[test]
    fn edit_constructors() {
        let p = Pos::new(0, 5);
        assert_eq!(Edit::insert(p, "x").range, p..p);
        assert!(Edit::insert(p, "x").replacement == "x");
        assert!(Edit::delete(p..p).replacement.is_empty());
    }

    #[test]
    fn attrs_flags() {
        let a = Attrs::BOLD | Attrs::UNDERLINE;
        assert!(a.contains(Attrs::BOLD));
        assert!(!a.contains(Attrs::ITALIC));
    }

    #[test]
    fn options_default_matches_vim() {
        let o = Options::default();
        assert_eq!(o.tabstop, 8);
        assert!(!o.expandtab);
        assert!(o.hlsearch);
        assert!(o.wrapscan);
        assert_eq!(o.timeout_len, core::time::Duration::from_millis(1000));
    }

    #[test]
    fn engine_error_display() {
        let e = EngineError::ReadOnly;
        assert_eq!(e.to_string(), "buffer is read-only");
        let e = EngineError::OutOfBounds(Pos::new(3, 7));
        assert!(e.to_string().contains("out of bounds"));
    }
}
