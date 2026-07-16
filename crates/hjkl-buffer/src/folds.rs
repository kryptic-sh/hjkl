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
//! [`crate::View::lines`] always returns the underlying logical text.
//!
//! Add / remove / toggle goes through
//! [`crate::View::add_fold`] / [`crate::View::remove_fold_at`] /
//! [`crate::View::toggle_fold_at`]. Open-all / close-all (`zR` / `zM`)
//! go through [`crate::View::open_all_folds`] /
//! [`crate::View::close_all_folds`]; folds keep their definitions across
//! open/close cycles.

/// A contiguous range of rows that the host can collapse to a single
/// fold-marker line.
///
/// Folds are row-range spans: `[start_row, end_row]` inclusive. The buffer
/// never elides content — [`crate::View::lines`] always returns the full
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
    /// `true` when this fold was created by the auto-fold engine
    /// (tree-sitter foldmethod=expr). Manual folds created via `zf` /
    /// [`crate::View::add_fold`] set this to `false`.
    ///
    /// Used by [`crate::View::set_auto_folds`] to distinguish auto
    /// folds (which it manages) from manual folds (which it leaves
    /// untouched).
    pub auto_generated: bool,
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

impl crate::View {
    /// Returns a snapshot of all folds as an owned `Vec<Fold>`.
    ///
    /// Owned rather than `&[Fold]` because a `View` is a per-window
    /// view onto a shared `Buffer`; another view could mutate the folds vec
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
            auto_generated: false,
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

    /// Replace all auto-generated folds with a new set derived from
    /// `ranges`, while leaving manual folds untouched.
    ///
    /// ## Algorithm (O(N) — bounded by `ranges.len()`, no unbounded growth)
    ///
    /// 1. Snapshot `start_row → closed` for every existing auto fold so
    ///    open/closed state survives a reparse.
    /// 2. Retain only manual folds (`auto_generated == false`).
    /// 3. Insert one new `Fold` per range, re-using the snapshotted closed
    ///    state when the start_row existed before, else `default_closed`.
    ///
    /// Invariants preserved:
    /// - Folds stay sorted by `start_row` (same ordering as `add_fold`).
    /// - Duplicate start_rows: the last range in `ranges` wins (consistent
    ///   with `add_fold`'s replace-on-same-start-row semantics). In practice
    ///   TS query ranges are already deduplicated.
    /// - Empty / inverted ranges (end_row < start_row) are silently skipped.
    /// - `end_row` is clamped to the last valid row, same as `add_fold`.
    pub fn set_auto_folds(&mut self, ranges: &[(usize, usize)], default_closed: bool) {
        // 1. Snapshot closed state of existing auto folds by start_row.
        let prev_closed: std::collections::HashMap<usize, bool> = self
            .content_lock()
            .folds
            .iter()
            .filter(|f| f.auto_generated)
            .map(|f| (f.start_row, f.closed))
            .collect();

        // 2. Retain manual folds only.
        {
            let mut c = self.content_lock_mut();
            c.folds.retain(|f| !f.auto_generated);
        }

        // 3. Insert new auto folds in sorted order.
        let last = self.row_count().saturating_sub(1);
        for &(start_row, end_row) in ranges {
            // Skip empty/inverted and out-of-bounds ranges.
            if end_row < start_row || start_row > last {
                continue;
            }
            let end_row = end_row.min(last);
            // Only folds spanning more than one row are meaningful.
            if end_row == start_row {
                continue;
            }
            let closed = prev_closed
                .get(&start_row)
                .copied()
                .unwrap_or(default_closed);
            let fold = Fold {
                start_row,
                end_row,
                closed,
                auto_generated: true,
            };
            let mut c = self.content_lock_mut();
            // Replace any existing fold at this start_row (manual or auto).
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
        // Remove the INNERMOST fold containing `row` (largest start_row), so
        // `zd` on a nested fold drops the inner one, not the enclosing block.
        let idx = self
            .content_lock()
            .folds
            .iter()
            .enumerate()
            .filter(|(_, f)| f.contains(row))
            .max_by_key(|(_, f)| f.start_row)
            .map(|(i, _)| i);
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
            let Some(f) = c
                .folds
                .iter_mut()
                .filter(|f| f.contains(row))
                .max_by_key(|f| f.start_row)
            else {
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
            let Some(f) = c
                .folds
                .iter_mut()
                .filter(|f| f.contains(row))
                .max_by_key(|f| f.start_row)
            else {
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
            let Some(f) = c
                .folds
                .iter_mut()
                .filter(|f| f.contains(row))
                .max_by_key(|f| f.start_row)
            else {
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
        // Innermost fold containing `row`: with nested folds, the one with the
        // largest `start_row` is the most-deeply-nested. Folds are stored in
        // start-row order, so a plain `.find` would return the OUTERMOST fold
        // and `zc`/`za`/`zo` would act on the wrong level.
        self.content_lock()
            .folds
            .iter()
            .filter(|f| f.contains(row))
            .max_by_key(|f| f.start_row)
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
        invalidate_folds(&mut self.content_lock_mut().folds, start_row, end_row);
        if self.content_lock().folds.len() != before {
            self.dirty_gen_bump();
        }
    }

    /// Shift every buffer fold by an edit's row-delta band. Mirrors
    /// [`crate::buffer::View::rebase_marks`] for the shared fold storage —
    /// see [`shift_folds_after_edit`] for the per-fold rules.
    pub fn rebase_folds(
        &mut self,
        edit_start: usize,
        drop_end: usize,
        shift_threshold: usize,
        delta: isize,
    ) {
        if delta == 0 {
            return;
        }
        let mut c = self.content_lock_mut();
        shift_folds_after_edit(&mut c.folds, edit_start, drop_end, shift_threshold, delta);
    }

    /// Replace the entire fold set wholesale. Used to install a per-window fold
    /// snapshot into the shared buffer on focus change (window-level folds): the
    /// app keeps each window's open/closed state and swaps it in before dispatch,
    /// so motions/render/`z`-ops operate on the focused window's folds.
    pub fn set_folds(&mut self, folds: &[Fold]) {
        {
            let mut c = self.content_lock_mut();
            if c.folds.as_slice() == folds {
                return; // no-op — avoid a spurious dirty_gen bump
            }
            c.folds = folds.to_vec();
        }
        self.dirty_gen_bump();
    }
}

/// Drop every fold in `folds` that touches `[start_row, end_row]`, in place.
///
/// Free helper so both [`crate::View::invalidate_folds_in_range`] (operating
/// on the shared content) and the app's window-level edit-coherence pass
/// (operating on a sibling window's owned `Vec<Fold>`) share one rule — vim
/// opens/forgets any fold the edit overlapped.
pub fn invalidate_folds(folds: &mut Vec<Fold>, start_row: usize, end_row: usize) {
    folds.retain(|f| f.end_row < start_row || f.start_row > end_row);
}

// ── Row-delta shifting (edit-coherence) ──────────────────────────────────
//
// A manual (`zf`) fold is a row-range that has to track the same
// insert/delete row-shift the engine already applies to marks and the
// jumplist (see `Editor::shift_marks_after_edit`). Without this, a fold
// below an edit keeps stale row numbers and the renderer / fold-aware ops
// (`dd`, `p`, …) act on the wrong rows (#audit-r2 fix 1).
//
// The four `(edit_start, drop_end, shift_threshold, delta)` parameters are
// the exact same band description `Editor::shift_marks_after_edit` computes
// for marks: `[edit_start, drop_end)` is the row band the edit deleted
// (empty for inserts), and any row `>= shift_threshold` moves by `delta`.
// Reusing the identical band keeps folds, marks, and jumplist entries
// shifting in lockstep for the same edit.

/// Shift a single fold's `start_row` / `end_row` by an edit's row-delta band.
/// Returns `None` when the edit's deleted band fully consumes the fold.
///
/// Each endpoint is mapped independently through the same drop/shift rule
/// [`crate::buffer::View::rebase_marks`] applies to a point mark. Mapping
/// the two endpoints independently is what produces the vim-shaped "edit
/// inside a fold" semantics for free:
/// - Both endpoints below `shift_threshold` and outside the deleted band →
///   fold untouched (edit happened entirely outside the fold).
/// - `start_row` outside the deleted band but `end_row` inside it → the
///   edit deleted the fold's tail; it clips to end at the last surviving
///   row (`edit_start - 1`).
/// - `start_row` inside the deleted band but `end_row` outside it → the
///   edit deleted the fold's head; it clips to start at `edit_start` (the
///   row the surviving tail now occupies).
/// - Both endpoints inside the deleted band → the edit consumed the whole
///   fold; it's dropped.
/// - `start_row` below the threshold and `end_row` at/above it (an insert
///   or a deletion landing strictly inside the fold) → `start_row` stays,
///   `end_row` shifts by `delta`: the fold grows (insert) or shrinks
///   (delete) around the edit, matching vim.
/// - Both endpoints at/above the threshold → the fold shifts wholesale.
pub fn shift_fold(
    fold: Fold,
    edit_start: usize,
    drop_end: usize,
    shift_threshold: usize,
    delta: isize,
) -> Option<Fold> {
    if delta == 0 {
        return Some(fold);
    }
    let map_row = |row: usize| -> Option<usize> {
        if (edit_start..drop_end).contains(&row) {
            None
        } else if row >= shift_threshold {
            Some(((row as isize) + delta).max(0) as usize)
        } else {
            Some(row)
        }
    };
    let mapped_start = map_row(fold.start_row);
    let mapped_end = map_row(fold.end_row);
    if mapped_start.is_none() && mapped_end.is_none() {
        return None;
    }
    let new_start = mapped_start.unwrap_or(edit_start);
    let new_end = mapped_end.unwrap_or_else(|| edit_start.saturating_sub(1));
    if new_end < new_start {
        return None;
    }
    Some(Fold {
        start_row: new_start,
        end_row: new_end,
        closed: fold.closed,
        auto_generated: fold.auto_generated,
    })
}

/// Shift every fold in `folds` by an edit's row-delta band, in place.
/// Folds the edit's deleted band fully consumes are dropped (mirrors
/// [`invalidate_folds`] for the folds that DO survive but move).
///
/// Shared by [`crate::View::rebase_folds`] (engine-side, the buffer's own
/// fold storage) and the app's sibling-window fold snapshot shift, so both
/// converge on the identical row-shift rule.
pub fn shift_folds_after_edit(
    folds: &mut Vec<Fold>,
    edit_start: usize,
    drop_end: usize,
    shift_threshold: usize,
    delta: isize,
) {
    if delta == 0 {
        return;
    }
    folds.retain_mut(
        |f| match shift_fold(*f, edit_start, drop_end, shift_threshold, delta) {
            Some(shifted) => {
                *f = shifted;
                true
            }
            None => false,
        },
    );
}

#[cfg(test)]
mod tests {
    use crate::View;

    fn b() -> View {
        View::from_str("a\nb\nc\nd\ne")
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
    fn set_folds_replaces_wholesale() {
        let mut buf = b();
        buf.add_fold(0, 1, false);
        // Install a different per-window snapshot.
        let snapshot = vec![super::Fold {
            start_row: 2,
            end_row: 3,
            closed: true,
            auto_generated: false,
        }];
        buf.set_folds(&snapshot);
        assert_eq!(buf.folds(), snapshot);
        // Idempotent: re-installing the same set is a no-op (no dirty bump).
        let dg = buf.dirty_gen();
        buf.set_folds(&snapshot);
        assert_eq!(buf.dirty_gen(), dg);
    }

    #[test]
    fn invalidate_folds_helper_drops_overlapping() {
        let f = |s, e| super::Fold {
            start_row: s,
            end_row: e,
            closed: true,
            auto_generated: false,
        };
        let mut folds = vec![f(0, 2), f(4, 6), f(8, 10)];
        // Edit touches rows 5..5 → only the [4,6] fold overlaps.
        super::invalidate_folds(&mut folds, 5, 5);
        let starts: Vec<usize> = folds.iter().map(|x| x.start_row).collect();
        assert_eq!(starts, vec![0, 8]);
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

    // ── auto_generated flag + set_auto_folds ─────────────────────────────────

    #[test]
    fn add_fold_sets_auto_generated_false() {
        let mut buf = b();
        buf.add_fold(1, 3, false);
        assert!(
            !buf.folds()[0].auto_generated,
            "manual add_fold must have auto_generated=false"
        );
    }

    #[test]
    fn set_auto_folds_adds_auto_folds() {
        let mut buf = b();
        buf.set_auto_folds(&[(0, 2), (3, 4)], false);
        let folds = buf.folds();
        assert_eq!(folds.len(), 2);
        assert!(folds[0].auto_generated);
        assert!(folds[1].auto_generated);
        assert_eq!(folds[0].start_row, 0);
        assert_eq!(folds[1].start_row, 3);
    }

    #[test]
    fn set_auto_folds_second_call_replaces_first() {
        let mut buf = b();
        buf.set_auto_folds(&[(0, 2), (3, 4)], false);
        assert_eq!(buf.folds().len(), 2);
        // Replace with a different set.
        buf.set_auto_folds(&[(1, 4)], false);
        let folds = buf.folds();
        assert_eq!(folds.len(), 1, "second call must replace first set");
        assert_eq!(folds[0].start_row, 1);
        assert!(folds[0].auto_generated);
    }

    #[test]
    fn set_auto_folds_preserves_manual_folds() {
        let mut buf = b();
        // Add a manual fold.
        buf.add_fold(0, 1, true);
        // Auto-fold the remaining range.
        buf.set_auto_folds(&[(2, 4)], false);
        let folds = buf.folds();
        assert_eq!(folds.len(), 2, "manual fold must survive set_auto_folds");
        let manual = folds.iter().find(|f| f.start_row == 0).unwrap();
        assert!(!manual.auto_generated, "manual fold flag must stay false");
        let auto = folds.iter().find(|f| f.start_row == 2).unwrap();
        assert!(auto.auto_generated);
    }

    #[test]
    fn set_auto_folds_preserves_open_closed_state_by_start_row() {
        let mut buf = b();
        // First auto-fold pass: create a closed fold at row 0.
        buf.set_auto_folds(&[(0, 2)], true); // default_closed=true → starts closed
        assert!(buf.folds()[0].closed, "fold must start closed per default");

        // User opens the fold (simulated by toggle).
        buf.toggle_fold_at(0);
        assert!(!buf.folds()[0].closed, "fold must now be open");

        // Second auto-fold pass with same start_row — must preserve open state.
        buf.set_auto_folds(&[(0, 2)], true); // default_closed=true but prev was open
        assert!(
            !buf.folds()[0].closed,
            "open/closed state must be preserved across set_auto_folds"
        );
    }

    #[test]
    fn set_auto_folds_skips_single_row_and_inverted_ranges() {
        let mut buf = b();
        buf.set_auto_folds(&[(1, 1), (3, 2)], false);
        assert!(
            buf.folds().is_empty(),
            "single-row and inverted ranges must be skipped"
        );
    }

    #[test]
    fn set_auto_folds_new_folds_use_default_closed() {
        let mut buf = b();
        buf.set_auto_folds(&[(0, 4)], true);
        assert!(
            buf.folds()[0].closed,
            "new auto fold must use default_closed=true"
        );

        // Clear and re-run with default_closed=false.
        buf.set_auto_folds(&[(0, 4)], false);
        // This is a *new* start_row (it was removed + re-added), BUT the
        // snapshot preserved the previous state (closed=true from above)
        // because the start_row is the same.
        // Wait — the test verifies the preservation path, not the default path.
        // Let's use a fresh start_row to test the default path:
        let mut buf2 = b();
        buf2.set_auto_folds(&[(2, 4)], false);
        assert!(
            !buf2.folds()[0].closed,
            "brand-new auto fold must start open when default_closed=false"
        );
    }

    // ── row-delta shifting (audit-r2 fix 1) ───────────────────────────────

    fn fold(s: usize, e: usize) -> super::Fold {
        super::Fold {
            start_row: s,
            end_row: e,
            closed: true,
            auto_generated: false,
        }
    }

    #[test]
    fn shift_fold_insert_above_shifts_down() {
        // 10-line file, fold at rows 4..6, insert one row at row 0
        // (`ggO x<Esc>`): vim shifts the fold to 5..7.
        let f = fold(4, 6);
        let shifted = super::shift_fold(f, 0, 0, 1, 1).unwrap();
        assert_eq!((shifted.start_row, shifted.end_row), (5, 7));
    }

    #[test]
    fn shift_fold_delete_above_shifts_up() {
        // Fold at rows 4..6, one row deleted above at row 0.
        let f = fold(4, 6);
        let shifted = super::shift_fold(f, 0, 1, 1, -1).unwrap();
        assert_eq!((shifted.start_row, shifted.end_row), (3, 5));
    }

    #[test]
    fn shift_fold_delete_fully_overlapping_drops() {
        // Fold at rows 4..6, deletion covers rows 4..6 entirely.
        let f = fold(4, 6);
        assert!(super::shift_fold(f, 4, 7, 7, -3).is_none());
    }

    #[test]
    fn shift_fold_delete_overlapping_tail_clips() {
        // Fold at rows 4..6, deletion of rows 6..8 (tail only) clips the
        // fold to end at the last surviving row.
        let f = fold(4, 6);
        let shifted = super::shift_fold(f, 6, 9, 9, -3).unwrap();
        assert_eq!((shifted.start_row, shifted.end_row), (4, 5));
    }

    #[test]
    fn shift_fold_delete_overlapping_head_clips() {
        // Fold at rows 4..6, deletion of rows 3..4 (head only) clips the
        // fold to start where the surviving tail now sits.
        let f = fold(4, 6);
        let shifted = super::shift_fold(f, 3, 5, 5, -2).unwrap();
        assert_eq!((shifted.start_row, shifted.end_row), (3, 4));
    }

    #[test]
    fn shift_fold_edit_inside_grows_on_insert() {
        // Fold at rows 4..6, a line inserted at row 5 (strictly inside):
        // vim grows the fold's end, leaves the start alone.
        let f = fold(4, 6);
        let shifted = super::shift_fold(f, 5, 5, 6, 1).unwrap();
        assert_eq!((shifted.start_row, shifted.end_row), (4, 7));
    }

    #[test]
    fn shift_fold_edit_inside_shrinks_on_delete() {
        // Fold at rows 4..8, rows 5..6 deleted (strictly inside): the fold
        // shrinks around the deletion instead of clipping or dropping.
        let f = fold(4, 8);
        let shifted = super::shift_fold(f, 5, 7, 7, -2).unwrap();
        assert_eq!((shifted.start_row, shifted.end_row), (4, 6));
    }

    #[test]
    fn shift_fold_unaffected_when_entirely_before_edit() {
        let f = fold(1, 2);
        let shifted = super::shift_fold(f, 10, 11, 11, 1).unwrap();
        assert_eq!((shifted.start_row, shifted.end_row), (1, 2));
    }

    #[test]
    fn shift_fold_zero_delta_is_noop() {
        let f = fold(4, 6);
        let shifted = super::shift_fold(f, 0, 0, 0, 0).unwrap();
        assert_eq!(shifted, f);
    }

    #[test]
    fn shift_folds_after_edit_shifts_vec_in_place() {
        let mut folds = vec![fold(4, 6), fold(1, 2)];
        // Insert one row at row 0: both folds shift down.
        super::shift_folds_after_edit(&mut folds, 0, 0, 1, 1);
        let ranges: Vec<(usize, usize)> = folds.iter().map(|f| (f.start_row, f.end_row)).collect();
        assert_eq!(ranges, vec![(5, 7), (2, 3)]);
    }

    #[test]
    fn shift_folds_after_edit_drops_fully_consumed() {
        let mut folds = vec![fold(4, 6)];
        super::shift_folds_after_edit(&mut folds, 4, 7, 7, -3);
        assert!(folds.is_empty());
    }

    #[test]
    fn rebase_folds_shifts_buffer_fold_storage() {
        let mut buf = View::from_str("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        buf.add_fold(4, 6, true);
        buf.rebase_folds(0, 0, 1, 1);
        let folds = buf.folds();
        assert_eq!(folds.len(), 1);
        assert_eq!((folds[0].start_row, folds[0].end_row), (5, 7));
    }
}
