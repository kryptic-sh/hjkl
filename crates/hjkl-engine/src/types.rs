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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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
    /// Soft-wrap behavior for lines that exceed the viewport width.
    /// Maps directly to `:set wrap` / `:set linebreak` / `:set nowrap`.
    pub wrap: WrapMode,
    /// Wrap column for `gq{motion}` text reflow. Vim's default is 79.
    pub textwidth: u32,
}

/// Soft-wrap mode for the renderer + scroll math + `gj` / `gk`.
/// Engine-native equivalent of [`hjkl_buffer::Wrap`]; the engine
/// converts at the boundary to the buffer's runtime wrap setting.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum WrapMode {
    /// Long lines extend past the right edge; `top_col` clips the
    /// left side. Matches vim's `:set nowrap`.
    #[default]
    None,
    /// Break at the cell boundary regardless of word edges. Matches
    /// `:set wrap`.
    Char,
    /// Break at the last whitespace inside the visible width when
    /// possible; falls back to a char break for runs longer than the
    /// width. Matches `:set linebreak`.
    Word,
}

/// Typed value for [`Options::set_by_name`] / [`Options::get_by_name`].
///
/// `:set tabstop=4` parses as `OptionValue::Int(4)`;
/// `:set noexpandtab` parses as `OptionValue::Bool(false)`;
/// `:set iskeyword=...` as `OptionValue::String(...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionValue {
    Bool(bool),
    Int(i64),
    String(String),
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
            wrap: WrapMode::None,
            textwidth: 79,
        }
    }
}

impl Options {
    /// Set an option by name. Vim-flavored option naming. Returns
    /// [`EngineError::Ex`] for unknown names or type-mismatched values.
    ///
    /// Booleans accept `OptionValue::Bool(_)` directly or
    /// `OptionValue::Int(0)`/`Int(non_zero)`. Integers accept only
    /// `Int(_)`. Strings accept only `String(_)`.
    pub fn set_by_name(&mut self, name: &str, val: OptionValue) -> Result<(), EngineError> {
        macro_rules! set_bool {
            ($field:ident) => {{
                self.$field = match val {
                    OptionValue::Bool(b) => b,
                    OptionValue::Int(n) => n != 0,
                    other => {
                        return Err(EngineError::Ex(format!(
                            "option `{name}` expects bool, got {other:?}"
                        )));
                    }
                };
                Ok(())
            }};
        }
        macro_rules! set_u32 {
            ($field:ident) => {{
                self.$field = match val {
                    OptionValue::Int(n) if n >= 0 && n <= u32::MAX as i64 => n as u32,
                    OptionValue::Int(n) => {
                        return Err(EngineError::Ex(format!(
                            "option `{name}` out of u32 range: {n}"
                        )));
                    }
                    other => {
                        return Err(EngineError::Ex(format!(
                            "option `{name}` expects int, got {other:?}"
                        )));
                    }
                };
                Ok(())
            }};
        }
        macro_rules! set_string {
            ($field:ident) => {{
                self.$field = match val {
                    OptionValue::String(s) => s,
                    other => {
                        return Err(EngineError::Ex(format!(
                            "option `{name}` expects string, got {other:?}"
                        )));
                    }
                };
                Ok(())
            }};
        }
        match name {
            "tabstop" | "ts" => set_u32!(tabstop),
            "shiftwidth" | "sw" => set_u32!(shiftwidth),
            "textwidth" | "tw" => set_u32!(textwidth),
            "expandtab" | "et" => set_bool!(expandtab),
            "iskeyword" | "isk" => set_string!(iskeyword),
            "ignorecase" | "ic" => set_bool!(ignorecase),
            "smartcase" | "scs" => set_bool!(smartcase),
            "hlsearch" | "hls" => set_bool!(hlsearch),
            "incsearch" | "is" => set_bool!(incsearch),
            "wrapscan" | "ws" => set_bool!(wrapscan),
            "autoindent" | "ai" => set_bool!(autoindent),
            "timeoutlen" | "tm" => {
                self.timeout_len = match val {
                    OptionValue::Int(n) if n >= 0 => core::time::Duration::from_millis(n as u64),
                    other => {
                        return Err(EngineError::Ex(format!(
                            "option `{name}` expects non-negative int (millis), got {other:?}"
                        )));
                    }
                };
                Ok(())
            }
            "undolevels" | "ul" => set_u32!(undo_levels),
            "undobreak" => set_bool!(undo_break_on_motion),
            "readonly" | "ro" => set_bool!(readonly),
            "wrap" => {
                let on = match val {
                    OptionValue::Bool(b) => b,
                    OptionValue::Int(n) => n != 0,
                    other => {
                        return Err(EngineError::Ex(format!(
                            "option `{name}` expects bool, got {other:?}"
                        )));
                    }
                };
                self.wrap = match (on, self.wrap) {
                    (false, _) => WrapMode::None,
                    (true, WrapMode::Word) => WrapMode::Word,
                    (true, _) => WrapMode::Char,
                };
                Ok(())
            }
            "linebreak" | "lbr" => {
                let on = match val {
                    OptionValue::Bool(b) => b,
                    OptionValue::Int(n) => n != 0,
                    other => {
                        return Err(EngineError::Ex(format!(
                            "option `{name}` expects bool, got {other:?}"
                        )));
                    }
                };
                self.wrap = match (on, self.wrap) {
                    (true, _) => WrapMode::Word,
                    (false, WrapMode::Word) => WrapMode::Char,
                    (false, other) => other,
                };
                Ok(())
            }
            other => Err(EngineError::Ex(format!("unknown option `{other}`"))),
        }
    }

    /// Read an option by name. `None` for unknown names.
    pub fn get_by_name(&self, name: &str) -> Option<OptionValue> {
        Some(match name {
            "tabstop" | "ts" => OptionValue::Int(self.tabstop as i64),
            "shiftwidth" | "sw" => OptionValue::Int(self.shiftwidth as i64),
            "textwidth" | "tw" => OptionValue::Int(self.textwidth as i64),
            "expandtab" | "et" => OptionValue::Bool(self.expandtab),
            "iskeyword" | "isk" => OptionValue::String(self.iskeyword.clone()),
            "ignorecase" | "ic" => OptionValue::Bool(self.ignorecase),
            "smartcase" | "scs" => OptionValue::Bool(self.smartcase),
            "hlsearch" | "hls" => OptionValue::Bool(self.hlsearch),
            "incsearch" | "is" => OptionValue::Bool(self.incsearch),
            "wrapscan" | "ws" => OptionValue::Bool(self.wrapscan),
            "autoindent" | "ai" => OptionValue::Bool(self.autoindent),
            "timeoutlen" | "tm" => OptionValue::Int(self.timeout_len.as_millis() as i64),
            "undolevels" | "ul" => OptionValue::Int(self.undo_levels as i64),
            "undobreak" => OptionValue::Bool(self.undo_break_on_motion),
            "readonly" | "ro" => OptionValue::Bool(self.readonly),
            "wrap" => OptionValue::Bool(!matches!(self.wrap, WrapMode::None)),
            "linebreak" | "lbr" => OptionValue::Bool(matches!(self.wrap, WrapMode::Word)),
            _ => return None,
        })
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

/// Engine render frame consumed by the host once per redraw.
///
/// Borrow-style — the engine builds it on demand from its internal
/// state without allocating clones of large fields. Hosts diff across
/// frames to decide what to repaint.
///
/// Coarse today: covers mode, cursor, cursor shape, viewport top, and
/// a snapshot of the current line count (to size the gutter). The
/// SPEC-target fields (`selections`, `highlights`, `command_line`,
/// `search_prompt`, `status_line`) land once trait extraction wires
/// the FSM through `SelectionSet` and the highlight pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderFrame {
    pub mode: SnapshotMode,
    pub cursor_row: u32,
    pub cursor_col: u32,
    pub cursor_shape: CursorShape,
    pub viewport_top: u32,
    pub line_count: u32,
}

/// Coarse editor snapshot suitable for serde round-tripping.
///
/// Today's shape is intentionally minimal — it carries only the bits
/// the runtime [`crate::Editor`] knows how to round-trip without the
/// trait extraction (mode, cursor, lines, viewport top, settings).
/// Once `Editor<B: Buffer, H: Host>` ships under phase 5, this struct
/// grows to cover full SPEC state: registers, marks, jump list, change
/// list, undo tree, full options.
///
/// Hosts that persist editor state between sessions should:
///
/// - Treat the snapshot as opaque. Don't manually mutate fields.
/// - Always check `version` after deserialization; reject on
///   mismatch rather than attempt migration.
///
/// # Wire-format stability
///
/// - **0.0.x:** [`Self::VERSION`] bumps with every structural change to
///   the snapshot. Hosts must reject mismatched persisted state — no
///   migration path is offered.
/// - **0.1.0:** [`Self::VERSION`] freezes. Hosts persisting editor state
///   between sessions can rely on the wire format being stable for the
///   entire 0.1.x line.
/// - **0.2.0+:** any further structural change to this struct requires a
///   `VERSION++` bump and is gated behind a major version bump of the
///   crate.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EditorSnapshot {
    /// Format version. See [`Self::VERSION`] for the lock policy.
    /// Hosts use this to detect mismatched persisted state.
    pub version: u32,
    /// Mode at snapshot time (status-line granularity).
    pub mode: SnapshotMode,
    /// Cursor `(row, col)` in byte indexing.
    pub cursor: (u32, u32),
    /// Buffer lines. Trailing `\n` not included.
    pub lines: Vec<String>,
    /// Viewport top line at snapshot time.
    pub viewport_top: u32,
    /// Register bank. Vim's `""`, `"0`–`"9`, `"a`–`"z`, `"+`/`"*`.
    /// Skipped for `Eq`/`PartialEq` because [`crate::Registers`]
    /// doesn't derive them today.
    pub registers: crate::Registers,
    /// Uppercase / "file" marks (`'A`–`'Z`). Survive `set_content`
    /// calls so they round-trip across tab swaps in the host.
    /// Lowercase marks are buffer-local and live on the `VimState`.
    pub file_marks: std::collections::HashMap<char, (u32, u32)>,
}

/// Status-line mode summary. Bridges to the legacy
/// [`crate::VimMode`] without leaking the full FSM type into the
/// snapshot wire format.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SnapshotMode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
    VisualBlock,
}

impl EditorSnapshot {
    /// Current snapshot format version.
    ///
    /// Bumped to 2 in v0.0.8: registers added.
    /// Bumped to 3 in v0.0.9: file_marks added.
    ///
    /// # Lock policy
    ///
    /// - **0.0.x (today):** `VERSION` bumps freely with each structural
    ///   change to [`EditorSnapshot`]. Persisted state from an older
    ///   patch release will not round-trip; hosts must reject the
    ///   snapshot rather than attempt a field-by-field migration.
    /// - **0.1.0:** `VERSION` freezes. Hosts persisting editor state
    ///   between sessions can rely on the wire format being stable for
    ///   the entire 0.1.x line.
    /// - **0.2.0+:** any further structural change requires `VERSION++`
    ///   together with a major-version bump of `hjkl-engine`.
    pub const VERSION: u32 = 3;
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
    fn options_set_get_roundtrip() {
        let mut o = Options::default();
        o.set_by_name("tabstop", OptionValue::Int(4)).unwrap();
        assert!(matches!(o.get_by_name("ts"), Some(OptionValue::Int(4))));
        o.set_by_name("expandtab", OptionValue::Bool(true)).unwrap();
        assert!(matches!(o.get_by_name("et"), Some(OptionValue::Bool(true))));
        o.set_by_name("iskeyword", OptionValue::String("a-z".into()))
            .unwrap();
        match o.get_by_name("iskeyword") {
            Some(OptionValue::String(s)) => assert_eq!(s, "a-z"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn options_unknown_name_errors_on_set() {
        let mut o = Options::default();
        assert!(matches!(
            o.set_by_name("frobnicate", OptionValue::Int(1)),
            Err(EngineError::Ex(_))
        ));
        assert!(o.get_by_name("frobnicate").is_none());
    }

    #[test]
    fn options_type_mismatch_errors() {
        let mut o = Options::default();
        assert!(matches!(
            o.set_by_name("tabstop", OptionValue::String("nope".into())),
            Err(EngineError::Ex(_))
        ));
        assert!(matches!(
            o.set_by_name("iskeyword", OptionValue::Int(7)),
            Err(EngineError::Ex(_))
        ));
    }

    #[test]
    fn options_int_to_bool_coercion() {
        // `:set ic=0` reads as boolean false; `:set ic=1` as true.
        // Common vim spelling.
        let mut o = Options::default();
        o.set_by_name("ignorecase", OptionValue::Int(1)).unwrap();
        assert!(matches!(o.get_by_name("ic"), Some(OptionValue::Bool(true))));
        o.set_by_name("ignorecase", OptionValue::Int(0)).unwrap();
        assert!(matches!(
            o.get_by_name("ic"),
            Some(OptionValue::Bool(false))
        ));
    }

    #[test]
    fn options_wrap_linebreak_roundtrip() {
        let mut o = Options::default();
        assert_eq!(o.wrap, WrapMode::None);
        o.set_by_name("wrap", OptionValue::Bool(true)).unwrap();
        assert_eq!(o.wrap, WrapMode::Char);
        o.set_by_name("linebreak", OptionValue::Bool(true)).unwrap();
        assert_eq!(o.wrap, WrapMode::Word);
        assert!(matches!(
            o.get_by_name("wrap"),
            Some(OptionValue::Bool(true))
        ));
        assert!(matches!(
            o.get_by_name("lbr"),
            Some(OptionValue::Bool(true))
        ));
        o.set_by_name("linebreak", OptionValue::Bool(false))
            .unwrap();
        assert_eq!(o.wrap, WrapMode::Char);
        o.set_by_name("wrap", OptionValue::Bool(false)).unwrap();
        assert_eq!(o.wrap, WrapMode::None);
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
    fn editor_snapshot_version_const() {
        assert_eq!(EditorSnapshot::VERSION, 3);
    }

    #[test]
    fn editor_snapshot_default_shape() {
        let s = EditorSnapshot {
            version: EditorSnapshot::VERSION,
            mode: SnapshotMode::Normal,
            cursor: (0, 0),
            lines: vec!["hello".to_string()],
            viewport_top: 0,
            registers: crate::Registers::default(),
            file_marks: Default::default(),
        };
        assert_eq!(s.cursor, (0, 0));
        assert_eq!(s.lines.len(), 1);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn editor_snapshot_roundtrip() {
        let mut file_marks = std::collections::HashMap::new();
        file_marks.insert('A', (5u32, 2u32));
        let s = EditorSnapshot {
            version: EditorSnapshot::VERSION,
            mode: SnapshotMode::Insert,
            cursor: (3, 7),
            lines: vec!["alpha".into(), "beta".into()],
            viewport_top: 2,
            registers: crate::Registers::default(),
            file_marks,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: EditorSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(s.cursor, back.cursor);
        assert_eq!(s.lines, back.lines);
        assert_eq!(s.viewport_top, back.viewport_top);
    }

    #[test]
    fn engine_error_display() {
        let e = EngineError::ReadOnly;
        assert_eq!(e.to_string(), "buffer is read-only");
        let e = EngineError::OutOfBounds(Pos::new(3, 7));
        assert!(e.to_string().contains("out of bounds"));
    }
}
