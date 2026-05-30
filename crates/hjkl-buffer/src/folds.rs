//! Manual folds: contiguous row ranges that the host can collapse
//! to a single visible "fold marker" line.
//!
//! Phase 9 of the migration plan unlocks this — vim users get
//! `zo`/`zc`/`za`/`zR`/`zM` over the same buffer the editor is
//! mutating, no separate fold tracker required.
//!
//! ## Fold semantics
//!
//! Folds are **row-range** spans, not byte spans. [`Fold`] covers
//! `[start_row, end_row]` inclusive. The host renders folds as collapsed
//! single-line stubs; the buffer never elides them on its own —
//! [`crate::Buffer::lines`] always returns the underlying logical text.
//!
//! Add / remove / toggle goes through
//! [`crate::Buffer::add_fold`] / [`crate::Buffer::remove_fold_at`] /
//! [`crate::Buffer::toggle_fold_at`]. Open-all / close-all (`zR` / `zM`)
//! go through [`crate::Buffer::open_all_folds`] /
//! [`crate::Buffer::close_all_folds`]; folds keep their definitions across
//! open/close cycles.

/// A contiguous range of rows that the host can collapse to a single
/// fold-marker line.
///
/// Folds are row-range spans: `[start_row, end_row]` inclusive. The buffer
/// never elides content — [`crate::Buffer::lines`] always returns the full
/// logical text regardless of fold state. It is the host's render path that
/// skips hidden rows and replaces them with a stub.
///
/// See the `folds` module documentation for the full invariant description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    /// First row of the folded range (visible when closed).
    pub start_row: usize,
    /// Last row of the folded range, inclusive.
    pub end_row: usize,
    /// `true` = collapsed (rows after `start_row` are hidden).
    pub closed: bool,
}

impl Fold {
    pub fn contains(&self, row: usize) -> bool {
        row >= self.start_row && row <= self.end_row
    }

    /// True when `row` is hidden by a closed fold (i.e. inside the
    /// fold but not on its `start_row` marker line).
    pub fn hides(&self, row: usize) -> bool {
        self.closed && row > self.start_row && row <= self.end_row
    }

    /// Number of rows the fold spans.
    pub fn line_count(&self) -> usize {
        self.end_row.saturating_sub(self.start_row) + 1
    }
}

impl crate::Buffer {
    /// Returns a snapshot of all folds as an owned `Vec<Fold>`.
    ///
    /// Owned rather than `&[Fold]` because a `Buffer` is a per-window
    /// view onto a shared `Content`; another view could mutate the folds vec
    /// between when this returns and when the caller reads the slice.
    pub fn folds(&self) -> Vec<Fold> {
        self.content_lock().folds.clone()
    }

    /// Register a new fold. If an existing fold has the same
    /// `start_row`, it's replaced; otherwise the new one is inserted
    /// in start-row order. Empty / inverted ranges are rejected.
    pub fn add_fold(&mut self, start_row: usize, end_row: usize, closed: bool) {
        if end_row < start_row {
            return;
        }
        let last = self.row_count().saturating_sub(1);
        if start_row > last {
            return;
        }
        let end_row = end_row.min(last);
        let fold = Fold {
            start_row,
            end_row,
            closed,
        };
        {
            let mut c = self.content_lock_mut();
            if let Some(idx) = c.folds.iter().position(|f| f.start_row == start_row) {
                c.folds[idx] = fold;
            } else {
                let pos = c
                    .folds
                    .iter()
                    .position(|f| f.start_row > start_row)
                    .unwrap_or(c.folds.len());
                c.folds.insert(pos, fold);
            }
        }
        self.dirty_gen_bump();
    }

    /// Drop the fold whose range covers `row`. Returns `true` when a
    /// fold was actually removed.
    pub fn remove_fold_at(&mut self, row: usize) -> bool {
        let idx = self
            .content_lock()
            .folds
            .iter()
            .position(|f| f.contains(row));
        let Some(idx) = idx else {
            return false;
        };
        self.content_lock_mut().folds.remove(idx);
        self.dirty_gen_bump();
        true
    }

    /// Open the fold at `row` (no-op if already open or no fold).
    pub fn open_fold_at(&mut self, row: usize) -> bool {
        let changed = {
            let mut c = self.content_lock_mut();
            let Some(f) = c.folds.iter_mut().find(|f| f.contains(row)) else {
                return false;
            };
            if !f.closed {
                return false;
            }
            f.closed = false;
            true
        };
        if changed {
            self.dirty_gen_bump();
        }
        changed
    }

    /// Close the fold at `row` (no-op if already closed or no fold).
    pub fn close_fold_at(&mut self, row: usize) -> bool {
        let changed = {
            let mut c = self.content_lock_mut();
            let Some(f) = c.folds.iter_mut().find(|f| f.contains(row)) else {
                return false;
            };
            if f.closed {
                return false;
            }
            f.closed = true;
            true
        };
        if changed {
            self.dirty_gen_bump();
        }
        changed
    }

    /// Flip the closed/open state of the fold containing `row`.
    pub fn toggle_fold_at(&mut self, row: usize) -> bool {
        let changed = {
            let mut c = self.content_lock_mut();
            let Some(f) = c.folds.iter_mut().find(|f| f.contains(row)) else {
                return false;
            };
            f.closed = !f.closed;
            true
        };
        if changed {
            self.dirty_gen_bump();
        }
        changed
    }

    /// `zR` — open every fold.
    pub fn open_all_folds(&mut self) {
        let changed = {
            let mut c = self.content_lock_mut();
            let mut any = false;
            for f in c.folds.iter_mut() {
                if f.closed {
                    f.closed = false;
                    any = true;
                }
            }
            any
        };
        if changed {
            self.dirty_gen_bump();
        }
    }

    /// `zE` — eliminate every fold.
    pub fn clear_all_folds(&mut self) {
        let was_nonempty = !self.content_lock().folds.is_empty();
        if was_nonempty {
            self.content_lock_mut().folds.clear();
            self.dirty_gen_bump();
        }
    }

    /// `zM` — close every fold.
    pub fn close_all_folds(&mut self) {
        let changed = {
            let mut c = self.content_lock_mut();
            let mut any = false;
            for f in c.folds.iter_mut() {
                if !f.closed {
                    f.closed = true;
                    any = true;
                }
            }
            any
        };
        if changed {
            self.dirty_gen_bump();
        }
    }

    /// First fold whose range contains `row`. Useful for the host's
    /// `za`/`zo`/`zc` handlers.
    pub fn fold_at_row(&self, row: usize) -> Option<Fold> {
        self.content_lock()
            .folds
            .iter()
            .find(|f| f.contains(row))
            .copied()
    }

    /// True iff `row` is hidden by a closed fold (any fold).
    pub fn is_row_hidden(&self, row: usize) -> bool {
        self.folds().iter().any(|f| f.hides(row))
    }

    /// Open every closed fold whose body hides `row`, so the row becomes
    /// visible. Handles nested folds in a single pass — unlike
    /// `open_fold_at` / `FoldOp::OpenAt`, which only act on the first fold
    /// containing the row and so can never reach a nested inner fold.
    /// Used by `goto_line` so a jump into a folded region reveals the
    /// target line instead of stranding the cursor on a hidden row.
    /// Returns `true` if any fold was opened.
    pub fn reveal_row(&mut self, row: usize) -> bool {
        let changed = {
            let mut c = self.content_lock_mut();
            let mut any = false;
            for f in c.folds.iter_mut() {
                if f.hides(row) {
                    f.closed = false;
                    any = true;
                }
            }
            any
        };
        if changed {
            self.dirty_gen_bump();
        }
        changed
    }

    /// First visible row strictly after `row`, skipping any rows hidden
    /// by closed folds. Returns `None` past the end of the buffer.
    pub fn next_visible_row(&self, row: usize) -> Option<usize> {
        let last = self.row_count().saturating_sub(1);
        if last == 0 && row == 0 {
            return None;
        }
        let mut r = row.checked_add(1)?;
        while r <= last && self.is_row_hidden(r) {
            r += 1;
        }
        (r <= last).then_some(r)
    }

    /// First visible row strictly before `row`, skipping hidden rows.
    pub fn prev_visible_row(&self, row: usize) -> Option<usize> {
        let mut r = row.checked_sub(1)?;
        while self.is_row_hidden(r) {
            r = r.checked_sub(1)?;
        }
        Some(r)
    }

    /// Drop every fold that touches `[start_row, end_row]`.
    pub fn invalidate_folds_in_range(&mut self, start_row: usize, end_row: usize) {
        let before = self.content_lock().folds.len();
        self.content_lock_mut()
            .folds
            .retain(|f| f.end_row < start_row || f.start_row > end_row);
        if self.content_lock().folds.len() != before {
            self.dirty_gen_bump();
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Buffer;

    fn b() -> Buffer {
        Buffer::from_str("a\nb\nc\nd\ne")
    }

    #[test]
    fn add_keeps_folds_in_start_row_order() {
        let mut buf = b();
        buf.add_fold(2, 3, true);
        buf.add_fold(0, 1, false);
        let starts: Vec<usize> = buf.folds().iter().map(|f| f.start_row).collect();
        assert_eq!(starts, vec![0, 2]);
    }

    #[test]
    fn add_replaces_existing_with_same_start_row() {
        let mut buf = b();
        buf.add_fold(1, 2, true);
        buf.add_fold(1, 4, false);
        assert_eq!(buf.folds().len(), 1);
        assert_eq!(buf.folds()[0].end_row, 4);
        assert!(!buf.folds()[0].closed);
    }

    #[test]
    fn add_clamps_end_row_to_buffer_bounds() {
        let mut buf = b();
        buf.add_fold(2, 99, true);
        assert_eq!(buf.folds()[0].end_row, 4);
    }

    #[test]
    fn add_rejects_inverted_range() {
        let mut buf = b();
        buf.add_fold(3, 1, true);
        assert!(buf.folds().is_empty());
    }

    #[test]
    fn toggle_flips_state() {
        let mut buf = b();
        buf.add_fold(1, 3, false);
        assert!(!buf.folds()[0].closed);
        assert!(buf.toggle_fold_at(2));
        assert!(buf.folds()[0].closed);
        assert!(buf.toggle_fold_at(2));
        assert!(!buf.folds()[0].closed);
    }

    #[test]
    fn is_row_hidden_excludes_start_row() {
        let mut buf = b();
        buf.add_fold(1, 3, true);
        assert!(!buf.is_row_hidden(0));
        assert!(!buf.is_row_hidden(1)); // start row stays visible
        assert!(buf.is_row_hidden(2));
        assert!(buf.is_row_hidden(3));
        assert!(!buf.is_row_hidden(4));
    }

    #[test]
    fn open_close_all_changes_every_fold() {
        let mut buf = b();
        buf.add_fold(0, 1, false);
        buf.add_fold(2, 3, true);
        buf.close_all_folds();
        assert!(buf.folds().iter().all(|f| f.closed));
        buf.open_all_folds();
        assert!(buf.folds().iter().all(|f| !f.closed));
    }

    #[test]
    fn invalidate_drops_overlapping_folds() {
        let mut buf = b();
        buf.add_fold(0, 1, true);
        buf.add_fold(2, 3, true);
        buf.add_fold(4, 4, true);
        buf.invalidate_folds_in_range(2, 3);
        let starts: Vec<usize> = buf.folds().iter().map(|f| f.start_row).collect();
        assert_eq!(starts, vec![0, 4]);
    }
}
