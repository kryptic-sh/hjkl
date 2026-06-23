//! Renderer-agnostic quickfix / location-list data model (#184).
//!
//! A [`QfList`] is an ordered list of [`QfEntry`] locations with a cursor
//! pointer and vim-style navigation. The same type backs the global quickfix
//! list and (later) per-window location lists. Population (`:grep`, `:make`,
//! LSP references) and rendering are the host's responsibility — this crate is
//! pure `std`.

mod errorformat;
pub use errorformat::{parse_errorformat, parse_make_output};

use std::path::PathBuf;

/// Classification of a quickfix entry — drives the display marker / color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QfKind {
    Error,
    Warning,
    Info,
    Note,
    /// A `:grep` / search hit (no severity).
    Grep,
}

/// A single location: file + 0-based row/col + a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QfEntry {
    pub path: PathBuf,
    /// 0-based row.
    pub row: usize,
    /// 0-based column.
    pub col: usize,
    pub kind: QfKind,
    pub message: String,
}

/// An ordered list of locations with a cursor pointer. Navigation saturates at
/// the ends (vim errors past the end; saturating is fine for v1).
#[derive(Debug, Clone, Default)]
pub struct QfList {
    entries: Vec<QfEntry>,
    cursor: usize,
}

impl QfList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace all entries and reset the cursor to the first entry.
    pub fn set(&mut self, entries: Vec<QfEntry>) {
        self.entries = entries;
        self.cursor = 0;
    }

    /// Drop all entries and reset the cursor.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.cursor = 0;
    }

    /// Append entries to the list without changing the cursor.
    pub fn extend(&mut self, entries: Vec<QfEntry>) {
        self.entries.extend(entries);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn entries(&self) -> &[QfEntry] {
        &self.entries
    }

    /// Index of the current entry (0-based). Meaningless when empty (returns 0).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn current(&self) -> Option<&QfEntry> {
        self.entries.get(self.cursor)
    }

    /// `:cnext` — advance to the next entry, saturating at the last.
    /// (Named for the vim command; not an `Iterator`.)
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<&QfEntry> {
        if self.cursor + 1 < self.entries.len() {
            self.cursor += 1;
        }
        self.current()
    }

    /// `:cprev` — step back, saturating at the first.
    pub fn prev(&mut self) -> Option<&QfEntry> {
        self.cursor = self.cursor.saturating_sub(1);
        self.current()
    }

    /// `:cfirst` — jump to the first entry.
    pub fn first(&mut self) -> Option<&QfEntry> {
        self.cursor = 0;
        self.current()
    }

    /// `:clast` — jump to the last entry.
    pub fn last(&mut self) -> Option<&QfEntry> {
        if !self.entries.is_empty() {
            self.cursor = self.entries.len() - 1;
        }
        self.current()
    }

    /// `:cc N` — jump to the 1-based entry `n`, clamped into range.
    pub fn nth(&mut self, n_one_based: usize) -> Option<&QfEntry> {
        if !self.entries.is_empty() {
            self.cursor = n_one_based.saturating_sub(1).min(self.entries.len() - 1);
        }
        self.current()
    }

    /// Set the cursor directly (e.g. from a popup selection). No-op out of range.
    pub fn set_cursor(&mut self, i: usize) {
        if i < self.entries.len() {
            self.cursor = i;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(row: usize) -> QfEntry {
        QfEntry {
            path: PathBuf::from("a.rs"),
            row,
            col: 0,
            kind: QfKind::Grep,
            message: format!("line {row}"),
        }
    }

    fn list3() -> QfList {
        let mut l = QfList::new();
        l.set(vec![e(0), e(1), e(2)]);
        l
    }

    #[test]
    fn empty_nav_returns_none() {
        let mut l = QfList::new();
        assert!(l.is_empty());
        assert_eq!(l.current(), None);
        assert_eq!(l.next(), None);
        assert_eq!(l.prev(), None);
        assert_eq!(l.first(), None);
        assert_eq!(l.last(), None);
        assert_eq!(l.nth(1), None);
    }

    #[test]
    fn set_resets_cursor_to_first() {
        let mut l = list3();
        l.last();
        assert_eq!(l.cursor(), 2);
        l.set(vec![e(10), e(11)]);
        assert_eq!(l.cursor(), 0);
        assert_eq!(l.current().unwrap().row, 10);
    }

    #[test]
    fn next_saturates_at_last() {
        let mut l = list3();
        assert_eq!(l.next().unwrap().row, 1);
        assert_eq!(l.next().unwrap().row, 2);
        assert_eq!(l.next().unwrap().row, 2, "next at end stays");
        assert_eq!(l.cursor(), 2);
    }

    #[test]
    fn prev_saturates_at_first() {
        let mut l = list3();
        l.last();
        assert_eq!(l.prev().unwrap().row, 1);
        assert_eq!(l.prev().unwrap().row, 0);
        assert_eq!(l.prev().unwrap().row, 0, "prev at 0 stays");
        assert_eq!(l.cursor(), 0);
    }

    #[test]
    fn first_last() {
        let mut l = list3();
        l.next();
        assert_eq!(l.first().unwrap().row, 0);
        assert_eq!(l.last().unwrap().row, 2);
    }

    #[test]
    fn nth_is_one_based_and_clamps() {
        let mut l = list3();
        assert_eq!(l.nth(1).unwrap().row, 0, "nth is 1-based");
        assert_eq!(l.nth(2).unwrap().row, 1);
        assert_eq!(l.nth(99).unwrap().row, 2, "nth clamps to last");
        assert_eq!(l.nth(0).unwrap().row, 0, "nth(0) clamps to first");
    }

    #[test]
    fn set_cursor_out_of_range_noop() {
        let mut l = list3();
        l.set_cursor(1);
        assert_eq!(l.cursor(), 1);
        l.set_cursor(99);
        assert_eq!(l.cursor(), 1, "out-of-range set_cursor is a no-op");
    }
}
