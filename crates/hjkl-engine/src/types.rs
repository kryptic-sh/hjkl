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
}
