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

/// Sorted in canonical `(start_row ASC, end_row DESC)` order over a fold slice
/// for O(log F) "is this row hidden by a closed fold" queries.
///
/// The O(log F) twin of `folds.iter().any(|f| f.hides(row))`. Closed
/// folds' `(start_row, end_row]` hidden ranges are merged into a disjoint
/// union, and one `partition_point` over the merged ranges answers the
/// query. Merging (not just sorting) is what keeps this correct for
/// nested folds: a fold that starts inside an enclosing one can end
/// before it while the enclosing fold still covers the query row, so
/// "the last fold with `start_row <= row`" alone is not enough — that
/// fold may have already ended while an earlier, longer one still hides
/// the row.
///
/// Mirrors the TUI renderer's `FoldIndex` (`hjkl-buffer-tui`
/// render.rs) — the same merge, the same query, the same answers. That
/// copy sorts its input defensively because its fold source is not
/// guaranteed ordered; this one relies on the buffer's canonical
/// `(start_row ASC, end_row DESC)` ordering invariant (see
/// [`crate::View::add_fold`] and [`crate::View::set_auto_folds`]) and skips the
/// sort so the build stays O(F) — it runs per keystroke / per frame on hot
/// paths. Debug builds assert the invariant, so feeding unsorted folds fails
/// loudly in tests rather than silently mis-answering in release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldIndex {
    /// Merged, disjoint half-open intervals `(start, end]` covering rows
    /// hidden by at least one closed fold, sorted by `start`. A row `r` is
    /// hidden iff `start < r <= end` for the interval with the greatest
    /// `start <= r`.
    hidden_ranges: Vec<(usize, usize)>,
}

impl FoldIndex {
    /// Build the merged hidden-range index from a fold slice in canonical
    /// `(start_row ASC, end_row DESC)` order (the buffer invariant — see the
    /// type docs).
    ///
    /// O(F): one filter pass over the folds, then a single merge pass.
    pub fn new(folds: &[Fold]) -> Self {
        debug_assert!(
            folds
                .windows(2)
                .all(|window| compare_folds(&window[0], &window[1]).is_le()),
            "FoldIndex::new requires folds in canonical order"
        );
        // Merge the closed folds' (start, end] intervals into a disjoint
        // union. `hides` membership is "in the union", so a single binary
        // search over the merged ranges is correct even with nesting.
        let mut hidden_ranges: Vec<(usize, usize)> = Vec::with_capacity(folds.len());
        for f in folds.iter().filter(|f| f.closed) {
            let (s, e) = (f.start_row, f.end_row);
            match hidden_ranges.last_mut() {
                // Overlaps the previous interval (or abuts it exactly —
                // `s <= last_end` leaves no uncovered row between them) →
                // extend. A gap needs `s > last_end`, e.g. (0,5] then (7,9]:
                // row 6 is covered by neither, so they stay separate.
                Some((_, last_end)) if s <= *last_end => {
                    *last_end = (*last_end).max(e);
                }
                _ => hidden_ranges.push((s, e)),
            }
        }
        Self { hidden_ranges }
    }

    /// True when `row` is hidden by a closed fold — the O(log F) twin of
    /// `folds.iter().any(|f| f.hides(row))`.
    pub fn hides_row(&self, row: usize) -> bool {
        let idx = self.hidden_ranges.partition_point(|&(s, _)| s <= row);
        if idx == 0 {
            return false;
        }
        let (s, e) = self.hidden_ranges[idx - 1];
        row > s && row <= e
    }
}

/// Compare folds in canonical order: earliest start first, then widest range
/// first when ranges share a start row.
fn compare_folds(left: &Fold, right: &Fold) -> std::cmp::Ordering {
    left.start_row
        .cmp(&right.start_row)
        .then_with(|| right.end_row.cmp(&left.end_row))
}

/// Index of the deepest fold containing `row`: latest start first, then the
/// shortest range for folds that share a start row.
fn innermost_containing_fold_index(folds: &[Fold], row: usize) -> Option<usize> {
    folds
        .iter()
        .enumerate()
        .filter(|(_, fold)| fold.contains(row))
        .max_by_key(|(_, fold)| (fold.start_row, std::cmp::Reverse(fold.end_row)))
        .map(|(index, _)| index)
}

/// Index of the recursive fold target at `row`. A fold that starts on the
/// cursor row takes precedence; among same-start folds, that is the widest
/// range so `zC` / `zO` / `zA` include the complete same-start nesting.
/// Otherwise, use the innermost containing fold, matching the single-fold
/// commands' target selection.
fn recursive_fold_target_index(folds: &[Fold], row: usize) -> Option<usize> {
    folds
        .iter()
        .position(|fold| fold.start_row == row)
        .or_else(|| innermost_containing_fold_index(folds, row))
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

    /// Run `f` against the fold list under a **single** content lock, with
    /// no clone. The borrow-style twin of [`Self::folds`] — prefer it for
    /// every read-only query (`hides` scans, `is_empty`, per-row loops);
    /// keep [`Self::folds`] only where an owned snapshot must outlive the
    /// lock (e.g. it is stored, or the buffer is re-borrowed mutably).
    ///
    /// The closure runs with the content mutex held: it must not call back
    /// into any `&self` method of this `View` (they all re-lock, and the
    /// mutex is not re-entrant). Hoist such reads — `row_count()`,
    /// `cursor()`, … — above the call.
    pub fn with_folds<T>(&self, f: impl FnOnce(&[Fold]) -> T) -> T {
        f(&self.content_lock().folds)
    }

    /// True when at least one fold is defined (open or closed). One lock,
    /// no clone — the cheap form of `!folds().is_empty()`.
    pub fn has_folds(&self) -> bool {
        !self.content_lock().folds.is_empty()
    }

    /// Monotonic fold-mutation generation. Bumps only when a fold mutator
    /// actually changes the fold set; read-only queries and plain text
    /// edits leave it alone. Hosts caching a fold snapshot (`hjkl`'s
    /// per-window `window_folds`) compare this instead of re-cloning the
    /// fold `Vec` every keystroke.
    ///
    /// Conservative in the same sense as [`Self::dirty_gen`]: "if it
    /// changed, the folds **may** have changed" (a `rebase_folds` whose
    /// row-shift happens to move nothing still bumps).
    pub fn fold_gen(&self) -> u64 {
        self.content_lock().fold_gen
    }

    /// Record a fold-set mutation: bump the fold generation *and* the
    /// render-cache generation (a fold change repaints). Every mutator in
    /// this module funnels through here.
    fn folds_changed(&mut self) {
        self.fold_gen_bump();
        self.dirty_gen_bump();
    }

    /// Register a new fold. An existing fold with the exact same inclusive
    /// range is replaced; distinct ranges sharing a start row are retained.
    /// Folds stay in canonical `(start_row ASC, end_row DESC)` order. Empty /
    /// inverted ranges are rejected.
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
            if let Some(idx) = c
                .folds
                .iter()
                .position(|f| (f.start_row, f.end_row) == (start_row, end_row))
            {
                c.folds[idx] = fold;
            } else {
                let pos = c
                    .folds
                    .partition_point(|existing| compare_folds(existing, &fold).is_lt());
                c.folds.insert(pos, fold);
            }
        }
        self.folds_changed();
    }

    /// Replace all auto-generated folds with a new set derived from
    /// `ranges`, while leaving manual folds untouched.
    ///
    /// ## `foldlevelstart`, not a boolean
    ///
    /// The second parameter is vim's `'foldlevelstart'` verbatim, and the rule
    /// it implements is vim's: **a fold whose (1-based) nesting level is
    /// greater than `foldlevelstart` starts closed**, everything at or above
    /// that level starts open. So `0` closes everything, `1` leaves top-level
    /// folds open and closes what is inside them, `99` (hjkl's default) opens
    /// everything.
    ///
    /// It takes the raw option rather than a `default_closed: bool` — the
    /// shape it replaced, which could only express "all closed" / "all open" —
    /// and rather than a per-range level supplied by the caller, because:
    ///
    /// - **Nesting level is derivable from the ranges alone** (a fold's level
    ///   is one more than the number of other ranges that strictly contain
    ///   it), so a caller passing levels would be passing information this
    ///   function can compute — and would have to recompute identically at
    ///   every call site, marker scan and tree-sitter query alike.
    /// - **Only this function knows the final set.** Levels have to be counted
    ///   over the ranges that actually become folds, and that is decided
    ///   *here*: single-row, inverted and out-of-range entries are dropped,
    ///   `end_row` is clamped, duplicate start rows collapse, and rows owned
    ///   by a manual fold are skipped. A level computed by the caller over the
    ///   raw ranges would be counting folds that do not exist.
    ///
    /// Levels are counted over the **auto** folds only: a `zf` fold enclosing
    /// an auto one is the user's own structure, and letting it push the auto
    /// fold a level deeper would make the auto fold's start state depend on
    /// unrelated manual folding.
    ///
    /// vim's own default for the option is `-1`, meaning "do nothing, leave
    /// `'foldlevel'` alone" (which, with `'foldlevel'` at its own default of
    /// `0`, ends up closing everything). hjkl's `foldlevelstart` is a `u32`
    /// defaulting to `99`, so that mode does not exist here and no value is
    /// reserved for it — every value is a real level.
    ///
    /// ## Algorithm (O(N log N) — bounded by `ranges.len()`, no unbounded growth)
    ///
    /// 1. Snapshot `(start_row, end_row) → closed` for every existing auto fold
    ///    so each range's open/closed state survives a reparse.
    /// 2. Retain only manual folds (`auto_generated == false`).
    /// 3. Normalise `ranges` into the set that will actually become folds.
    /// 4. Assign each of those a nesting level by a containment sweep.
    /// 5. Insert one new `Fold` per surviving range, re-using the snapshotted
    ///    closed state when its exact range existed before, else
    ///    `level > foldlevelstart`.
    ///
    /// Invariants preserved:
    /// - Folds stay in canonical `(start_row ASC, end_row DESC)` order.
    /// - Duplicate exact ranges are deduplicated deterministically. Distinct
    ///   ranges sharing a start row are retained.
    /// - Empty / inverted ranges (end_row < start_row) are silently skipped.
    /// - `end_row` is clamped to the last valid row, same as `add_fold`.
    /// - A MANUAL fold's start_row is never taken over: the auto range for
    ///   that row is dropped instead. `zf` is an explicit choice of extent,
    ///   and converting it to an auto fold both changed it and handed it to
    ///   the auto engine to overwrite on the next reparse.
    /// - An auto fold with an existing exact range keeps its open / closed
    ///   state: `foldlevelstart` decides how a fold *starts*, so it applies to
    ///   newly-appearing ranges only and never re-closes a fold the user opened.
    /// - When the resulting fold set is identical to the current one, nothing
    ///   is written and NO generation bumps — [`Self::fold_gen`] promises to
    ///   move only on a real change, and `dirty_gen` moving every pass made
    ///   the caller's "recompute once per edit" guard fire every frame (and
    ///   with it a full re-highlight, since `dirty_gen` keys that cache).
    pub fn set_auto_folds(&mut self, ranges: &[(usize, usize)], foldlevelstart: u32) {
        // 1. Snapshot closed state of existing auto folds by exact range.
        let prev_closed: std::collections::HashMap<(usize, usize), bool> = self
            .content_lock()
            .folds
            .iter()
            .filter(|f| f.auto_generated)
            .map(|f| ((f.start_row, f.end_row), f.closed))
            .collect();

        // 2. Start from the manual folds — they survive untouched, and their
        //    start rows are off-limits to the ranges below.
        let mut next: Vec<Fold> = self
            .content_lock()
            .folds
            .iter()
            .filter(|f| !f.auto_generated)
            .copied()
            .collect();
        let manual_starts: std::collections::HashSet<usize> =
            next.iter().map(|f| f.start_row).collect();

        // 3. Normalise: drop what will never become a fold, then sort and
        //    deduplicate exact ranges so the level sweep counts only real folds.
        let last = self.row_count().saturating_sub(1);
        let mut accepted: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
        for &(start_row, end_row) in ranges {
            // Skip empty/inverted and out-of-bounds ranges.
            if end_row < start_row || start_row > last {
                continue;
            }
            let end_row = end_row.min(last);
            // Only folds spanning more than one row are meaningful.
            if end_row == start_row || manual_starts.contains(&start_row) {
                continue;
            }
            accepted.push((start_row, end_row));
        }
        accepted
            .sort_unstable_by_key(|&(start_row, end_row)| (start_row, std::cmp::Reverse(end_row)));
        accepted.dedup();

        // 4. Nesting level per accepted range, by a single containment sweep.
        //    Visiting outermost-first makes `stack` the chain of enclosing
        //    folds: pop everything that ends before this range does — that
        //    covers both a sibling that already closed and a partial overlap,
        //    neither of which encloses it — and what is left is exactly the
        //    enclosing chain.
        let mut levels = Vec::with_capacity(accepted.len());
        let mut stack: Vec<usize> = Vec::new();
        for &(_, end_row) in &accepted {
            while stack.last().is_some_and(|&open_end| open_end < end_row) {
                stack.pop();
            }
            // 1-based, matching vim: an unnested fold is level 1.
            levels.push(stack.len() as u32 + 1);
            stack.push(end_row);
        }

        // 5. Insert the new auto folds, then restore canonical ordering.
        next.extend(
            accepted
                .iter()
                .zip(levels)
                .map(|(&(start_row, end_row), level)| Fold {
                    start_row,
                    end_row,
                    closed: prev_closed
                        .get(&(start_row, end_row))
                        .copied()
                        .unwrap_or(level > foldlevelstart),
                    auto_generated: true,
                }),
        );
        next.sort_by(compare_folds);

        // 6. No-op when nothing actually changed — see the invariant above.
        if self.content_lock().folds == next {
            return;
        }
        self.content_lock_mut().folds = next;
        self.folds_changed();
    }

    /// Drop the fold whose range covers `row`. Returns `true` when a
    /// fold was actually removed.
    pub fn remove_fold_at(&mut self, row: usize) -> bool {
        // Remove the innermost fold containing `row`, so `zd` on a nested
        // fold drops the inner one, not the enclosing block.
        let idx = innermost_containing_fold_index(&self.content_lock().folds, row);
        let Some(idx) = idx else {
            return false;
        };
        self.content_lock_mut().folds.remove(idx);
        self.folds_changed();
        true
    }

    /// Open the fold at `row` (no-op if already open or no fold).
    pub fn open_fold_at(&mut self, row: usize) -> bool {
        let Some(idx) = innermost_containing_fold_index(&self.content_lock().folds, row) else {
            return false;
        };
        let changed = {
            let mut c = self.content_lock_mut();
            let f = &mut c.folds[idx];
            if !f.closed {
                return false;
            }
            f.closed = false;
            true
        };
        if changed {
            self.folds_changed();
        }
        changed
    }

    /// Close the fold at `row` (no-op if already closed or no fold).
    pub fn close_fold_at(&mut self, row: usize) -> bool {
        let Some(idx) = innermost_containing_fold_index(&self.content_lock().folds, row) else {
            return false;
        };
        let changed = {
            let mut c = self.content_lock_mut();
            let f = &mut c.folds[idx];
            if f.closed {
                return false;
            }
            f.closed = true;
            true
        };
        if changed {
            self.folds_changed();
        }
        changed
    }

    /// Flip the closed/open state of the fold containing `row`.
    pub fn toggle_fold_at(&mut self, row: usize) -> bool {
        let Some(idx) = innermost_containing_fold_index(&self.content_lock().folds, row) else {
            return false;
        };
        let changed = {
            let mut c = self.content_lock_mut();
            let f = &mut c.folds[idx];
            f.closed = !f.closed;
            true
        };
        if changed {
            self.folds_changed();
        }
        changed
    }

    /// `zC` — close the fold subtree at `row`. Folds that start on `row`
    /// select their widest range, so same-start nested folds close together;
    /// otherwise the innermost containing fold and its descendants close.
    pub fn close_folds_recursively_at(&mut self, row: usize) -> bool {
        let changed = {
            let mut c = self.content_lock_mut();
            let Some(target) = recursive_fold_target_index(&c.folds, row).map(|idx| c.folds[idx])
            else {
                return false;
            };
            let mut any = false;
            for fold in &mut c.folds {
                if fold.start_row >= target.start_row
                    && fold.end_row <= target.end_row
                    && !fold.closed
                {
                    fold.closed = true;
                    any = true;
                }
            }
            any
        };
        if changed {
            self.folds_changed();
        }
        changed
    }

    /// `zO` — open the fold subtree at `row`. Target selection matches
    /// [`Self::close_folds_recursively_at`].
    pub fn open_folds_recursively_at(&mut self, row: usize) -> bool {
        let changed = {
            let mut c = self.content_lock_mut();
            let Some(target) = recursive_fold_target_index(&c.folds, row).map(|idx| c.folds[idx])
            else {
                return false;
            };
            let mut any = false;
            for fold in &mut c.folds {
                if fold.start_row >= target.start_row
                    && fold.end_row <= target.end_row
                    && fold.closed
                {
                    fold.closed = false;
                    any = true;
                }
            }
            any
        };
        if changed {
            self.folds_changed();
        }
        changed
    }

    /// `zA` — toggle the fold subtree at `row`. If every fold in the subtree
    /// is open, close it; otherwise open the entire subtree.
    pub fn toggle_folds_recursively_at(&mut self, row: usize) -> bool {
        let changed = {
            let mut c = self.content_lock_mut();
            let Some(target) = recursive_fold_target_index(&c.folds, row).map(|idx| c.folds[idx])
            else {
                return false;
            };
            let closed = !c.folds.iter().any(|fold| {
                fold.start_row >= target.start_row && fold.end_row <= target.end_row && fold.closed
            });
            for fold in &mut c.folds {
                if fold.start_row >= target.start_row && fold.end_row <= target.end_row {
                    fold.closed = closed;
                }
            }
            true
        };
        if changed {
            self.folds_changed();
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
            self.folds_changed();
        }
    }

    /// `zE` — eliminate every fold.
    pub fn clear_all_folds(&mut self) {
        let was_nonempty = !self.content_lock().folds.is_empty();
        if was_nonempty {
            self.content_lock_mut().folds.clear();
            self.folds_changed();
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
            self.folds_changed();
        }
    }

    /// Deepest fold whose range contains `row`. Useful for the host's
    /// `za`/`zo`/`zc` handlers. Ranges sharing a start row select the shortest
    /// one.
    pub fn fold_at_row(&self, row: usize) -> Option<Fold> {
        let folds = self.content_lock();
        innermost_containing_fold_index(&folds.folds, row).map(|idx| folds.folds[idx])
    }

    /// True iff `row` is hidden by a closed fold (any fold).
    pub fn is_row_hidden(&self, row: usize) -> bool {
        self.with_folds(|folds| folds.iter().any(|f| f.hides(row)))
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
            self.folds_changed();
        }
        changed
    }

    /// Last row index containing real content — skips vim's single
    /// phantom trailing empty row. `ropey`'s `len_lines()` always
    /// synthesizes one extra empty final "line" when the buffer text
    /// ends in `\n` (vim treats that `\n` as a terminator, not a
    /// separator). Mirrors `hjkl_engine::motions::move_bottom`'s clamp
    /// (`G`) so vertical motions agree with `G` on where the buffer
    /// "ends". A buffer whose *real* last line happens to be empty
    /// (e.g. `"foo\n\n"`, row 1) is untouched — only a single trailing
    /// phantom row is ever skipped.
    ///
    /// The emptiness test reads the last row's byte length rather than
    /// materializing the row as a `String`: for the final rope line the two
    /// agree exactly (ropey never gives the last line a trailing `\n`, so
    /// `rope_line_str` has nothing to strip and
    /// `rope_line_bytes(last) == 0` iff `rope_line_str(last).is_empty()`).
    pub fn last_content_row(&self) -> usize {
        let raw_last = self.row_count().saturating_sub(1);
        if raw_last > 0 {
            let c = self.content_lock();
            if crate::buffer::rope_line_bytes(&c.text, raw_last) == 0 {
                return raw_last - 1;
            }
        }
        raw_last
    }

    /// First visible row strictly after `row`, skipping any rows hidden
    /// by closed folds. Returns `None` past the end of the buffer.
    ///
    /// Takes the content lock **once** for the whole walk (via
    /// [`Self::with_folds`]) instead of once per skipped row — `j` over a
    /// long closed fold used to pay a lock + full `Vec<Fold>` clone per row.
    /// `last_content_row()` locks too, so it is resolved before the scan.
    pub fn next_visible_row(&self, row: usize) -> Option<usize> {
        let last = self.last_content_row();
        if last == 0 && row == 0 {
            return None;
        }
        let mut r = row.checked_add(1)?;
        self.with_folds(|folds| {
            let index = FoldIndex::new(folds);
            while r <= last && index.hides_row(r) {
                r += 1;
            }
            (r <= last).then_some(r)
        })
    }

    /// First visible row strictly before `row`, skipping hidden rows.
    ///
    /// One lock for the whole walk, same as [`Self::next_visible_row`].
    pub fn prev_visible_row(&self, row: usize) -> Option<usize> {
        let mut r = row.checked_sub(1)?;
        self.with_folds(|folds| {
            let index = FoldIndex::new(folds);
            while index.hides_row(r) {
                r = r.checked_sub(1)?;
            }
            Some(r)
        })
    }

    /// Drop every fold that touches `[start_row, end_row]`.
    pub fn invalidate_folds_in_range(&mut self, start_row: usize, end_row: usize) {
        let before = self.content_lock().folds.len();
        invalidate_folds(&mut self.content_lock_mut().folds, start_row, end_row);
        if self.content_lock().folds.len() != before {
            self.folds_changed();
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
        let touched = {
            let mut c = self.content_lock_mut();
            if c.folds.is_empty() {
                false
            } else {
                shift_folds_after_edit(&mut c.folds, edit_start, drop_end, shift_threshold, delta);
                true
            }
        };
        if touched {
            // Conservative: a shift that happened to move nothing (every
            // fold entirely below the edit) still bumps. Matches the
            // `dirty_gen` contract — "may have changed".
            self.fold_gen_bump();
        }
    }

    /// Replace the entire fold set wholesale. Used to install a per-window fold
    /// snapshot into the shared buffer on focus change (window-level folds): the
    /// app keeps each window's open/closed state and swaps it in before dispatch,
    /// so motions/render/`z`-ops operate on the focused window's folds. Input
    /// is normalized to canonical `(start_row ASC, end_row DESC)` order, with
    /// exact-range duplicates collapsed.
    pub fn set_folds(&mut self, folds: &[Fold]) {
        let mut next = folds.to_vec();
        next.sort_by(compare_folds);
        next.dedup_by_key(|fold| (fold.start_row, fold.end_row));
        {
            let mut c = self.content_lock_mut();
            if c.folds == next {
                return; // no-op — avoid a spurious dirty_gen bump
            }
            c.folds = next;
        }
        self.folds_changed();
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
/// [`invalidate_folds`] for the folds that DO survive but move). The surviving
/// folds are normalized to canonical `(start_row ASC, end_row DESC)` order and
/// exact-range collisions retain the first stable canonical fold.
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
    folds.sort_by(compare_folds);
    folds.dedup_by_key(|fold| (fold.start_row, fold.end_row));
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
    fn add_fold_retains_same_start_ranges_and_replaces_exact_duplicate() {
        let mut buf = b();
        buf.add_fold(1, 4, false);
        buf.add_fold(1, 2, false);
        buf.add_fold(1, 2, true);

        assert_eq!(
            buf.folds()
                .iter()
                .map(|f| (f.start_row, f.end_row, f.closed))
                .collect::<Vec<_>>(),
            vec![(1, 4, false), (1, 2, true)]
        );
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
    fn same_start_selectors_target_the_innermost_fold() {
        let mut buf = b();
        buf.add_fold(0, 4, false);
        buf.add_fold(0, 2, true);
        assert!(buf.open_fold_at(1));
        assert!(!buf.is_row_hidden(1));
        assert!(!buf.is_row_hidden(2));
        assert!(!buf.is_row_hidden(3));
        assert!(!buf.is_row_hidden(4));

        assert!(buf.toggle_fold_at(1));
        assert!(buf.is_row_hidden(1));
        assert!(buf.is_row_hidden(2));
        assert!(!buf.is_row_hidden(3));
        assert!(!buf.is_row_hidden(4));
        assert_eq!(buf.fold_at_row(1).map(|fold| fold.end_row), Some(2));

        assert!(buf.remove_fold_at(1));
        assert!(!buf.is_row_hidden(1));
        assert!(!buf.is_row_hidden(2));
        assert!(!buf.is_row_hidden(3));
        assert!(!buf.is_row_hidden(4));
    }

    #[test]
    fn recursive_fold_commands_apply_to_only_the_target_subtree() {
        let mut buf = View::from_str("0\n1\n2\n3\n4\n5\n6\n7\n8");
        buf.add_fold(0, 6, false);
        buf.add_fold(2, 4, false);
        buf.add_fold(3, 3, false);
        buf.add_fold(5, 6, false);

        assert!(buf.close_folds_recursively_at(2));
        assert_eq!(
            buf.folds()
                .iter()
                .map(|fold| (fold.start_row, fold.end_row, fold.closed))
                .collect::<Vec<_>>(),
            vec![(0, 6, false), (2, 4, true), (3, 3, true), (5, 6, false)]
        );
        assert_eq!(buf.next_visible_row(2), Some(5));

        assert!(buf.open_folds_recursively_at(2));
        assert!(buf.folds().iter().all(|fold| !fold.closed));
    }

    #[test]
    fn recursive_toggle_uses_the_same_start_subtree_state() {
        let mut buf = View::from_str("a\nb\nc\nd\ne\nf");
        buf.add_fold(0, 4, false);
        buf.add_fold(0, 2, false);

        assert!(buf.toggle_folds_recursively_at(0));
        assert_eq!(
            buf.folds()
                .iter()
                .map(|fold| (fold.start_row, fold.end_row, fold.closed))
                .collect::<Vec<_>>(),
            vec![(0, 4, true), (0, 2, true)]
        );
        assert!((1..=4).all(|row| buf.is_row_hidden(row)));
        assert_eq!(buf.next_visible_row(0), Some(5));

        assert!(buf.toggle_folds_recursively_at(0));
        assert_eq!(
            buf.folds()
                .iter()
                .map(|fold| (fold.start_row, fold.end_row, fold.closed))
                .collect::<Vec<_>>(),
            vec![(0, 4, false), (0, 2, false)]
        );
        assert!((0..5).all(|row| !buf.is_row_hidden(row)));

        assert!(buf.close_fold_at(0));
        assert_eq!(
            buf.folds()
                .iter()
                .map(|fold| (fold.start_row, fold.end_row, fold.closed))
                .collect::<Vec<_>>(),
            vec![(0, 4, false), (0, 2, true)]
        );
        assert!(buf.is_row_hidden(1));
        assert!(buf.is_row_hidden(2));
        assert!(!buf.is_row_hidden(3));
        assert!(!buf.is_row_hidden(4));
        assert_eq!(buf.next_visible_row(0), Some(3));

        assert!(buf.toggle_folds_recursively_at(0));
        assert_eq!(
            buf.folds()
                .iter()
                .map(|fold| (fold.start_row, fold.end_row, fold.closed))
                .collect::<Vec<_>>(),
            vec![(0, 4, false), (0, 2, false)]
        );
        assert!((0..5).all(|row| !buf.is_row_hidden(row)));
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
    fn fold_index_hides_row_matches_naive_scan() {
        // The cases a naive "last fold with start_row <= row" binary search
        // gets wrong, pinned against the linear scan it replaces:
        // - a fold starting well before the queried row: row 99/100 are
        //   covered by [0,100], but the last start_row <= 99/100 is [50,60];
        // - a nested fold ending before the enclosing one: row 7 is covered
        //   by [1,10], but the last start_row <= 7 is [5,6] which ended at 6;
        // - row 10: covered by [1,10] and [0,100], while the last start_row
        //   <= 10 is [8,9], which ends at 9.
        let f = |s, e, closed| super::Fold {
            start_row: s,
            end_row: e,
            closed,
            auto_generated: false,
        };
        let folds = vec![
            f(0, 100, true),  // starts well before the queried rows
            f(1, 10, true),   // encloses the nested folds below
            f(5, 6, true),    // nested; ends before rows 7..9
            f(8, 9, true),    // nested; covers rows 8..9
            f(20, 20, true),  // degenerate: hides nothing itself
            f(30, 40, false), // open: hides nothing
            f(50, 60, true),
        ];
        let index = super::FoldIndex::new(&folds);
        for row in 0..200 {
            let naive = folds.iter().any(|f| f.hides(row));
            assert_eq!(index.hides_row(row), naive, "row {row}");
        }
    }

    #[test]
    fn fold_index_agrees_with_buffer_folds_across_nested_and_open_mixes() {
        // Same guarantee through the real buffer API: `add_fold` keeps the
        // list in canonical order regardless of insertion order, and the
        // index must answer identically to the buffer's linear scan for a
        // nesting of closed/open/degenerate folds.
        let mut buf = View::from_str(&"x\n".repeat(60));
        buf.add_fold(0, 55, true);
        buf.add_fold(2, 3, true);
        buf.add_fold(5, 6, false); // open
        buf.add_fold(7, 7, true); // degenerate — hides nothing itself
        buf.add_fold(10, 20, true);
        buf.add_fold(15, 16, true); // nested inside [10,20]
        for row in 0..buf.row_count() {
            let naive = buf.folds().iter().any(|f| f.hides(row));
            let via_index = buf.with_folds(|folds| super::FoldIndex::new(folds).hides_row(row));
            assert_eq!(via_index, naive, "row {row}");
        }
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
    fn set_auto_folds_retains_same_start_nested_ranges() {
        let mut buf = b();
        buf.set_auto_folds(&[(0, 4), (0, 2)], 99);

        assert_eq!(
            buf.folds()
                .iter()
                .map(|f| (f.start_row, f.end_row))
                .collect::<Vec<_>>(),
            vec![(0, 4), (0, 2)]
        );
        assert!(buf.close_fold_at(1));
        assert!(buf.is_row_hidden(1));
        assert!(buf.is_row_hidden(2));
        assert!(!buf.is_row_hidden(3));
        assert!(!buf.is_row_hidden(4));
        assert_eq!(buf.next_visible_row(0), Some(3));
    }

    #[test]
    fn set_auto_folds_deduplicates_same_start_exact_ranges() {
        let mut buf = b();
        buf.set_auto_folds(&[(0, 4), (0, 2), (0, 4)], 99);
        assert_eq!(
            buf.folds()
                .iter()
                .map(|fold| (fold.start_row, fold.end_row))
                .collect::<Vec<_>>(),
            vec![(0, 4), (0, 2)]
        );
    }

    #[test]
    fn set_auto_folds_adds_auto_folds() {
        let mut buf = b();
        buf.set_auto_folds(&[(0, 2), (3, 4)], 99);
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
        buf.set_auto_folds(&[(0, 2), (3, 4)], 99);
        assert_eq!(buf.folds().len(), 2);
        // Replace with a different set.
        buf.set_auto_folds(&[(1, 4)], 99);
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
        buf.set_auto_folds(&[(2, 4)], 99);
        let folds = buf.folds();
        assert_eq!(folds.len(), 2, "manual fold must survive set_auto_folds");
        let manual = folds.iter().find(|f| f.start_row == 0).unwrap();
        assert!(!manual.auto_generated, "manual fold flag must stay false");
        let auto = folds.iter().find(|f| f.start_row == 2).unwrap();
        assert!(auto.auto_generated);
    }

    #[test]
    fn set_auto_folds_preserves_same_start_range_states_after_reordering() {
        let mut buf = b();
        buf.set_auto_folds(&[(0, 4), (0, 2)], 99);
        assert!(buf.close_fold_at(1));

        buf.set_auto_folds(&[(0, 2), (0, 4)], 99);
        assert_eq!(
            buf.folds()
                .iter()
                .map(|fold| (fold.start_row, fold.end_row, fold.closed))
                .collect::<Vec<_>>(),
            vec![(0, 4, false), (0, 2, true)]
        );
    }

    #[test]
    fn set_auto_folds_preserves_open_closed_state_by_exact_range() {
        let mut buf = b();
        // First auto-fold pass: create a closed fold at row 0.
        buf.set_auto_folds(&[(0, 2)], 0); // foldlevelstart=0 → starts closed
        assert!(buf.folds()[0].closed, "fold must start closed per default");

        // User opens the fold (simulated by toggle).
        buf.toggle_fold_at(0);
        assert!(!buf.folds()[0].closed, "fold must now be open");

        // Second auto-fold pass with the same exact range preserves open state.
        buf.set_auto_folds(&[(0, 2)], 0); // foldlevelstart=0 but prev was open
        assert!(
            !buf.folds()[0].closed,
            "open/closed state must be preserved across set_auto_folds"
        );
    }

    #[test]
    fn set_auto_folds_skips_single_row_and_inverted_ranges() {
        let mut buf = b();
        buf.set_auto_folds(&[(1, 1), (3, 2)], 99);
        assert!(
            buf.folds().is_empty(),
            "single-row and inverted ranges must be skipped"
        );
    }

    #[test]
    fn set_auto_folds_new_folds_start_per_foldlevelstart() {
        let mut buf = b();
        buf.set_auto_folds(&[(0, 4)], 0);
        assert!(
            buf.folds()[0].closed,
            "new auto fold must start closed at foldlevelstart=0"
        );

        // Re-running the same exact range at foldlevelstart=99 must NOT reopen
        // it: the snapshot preserves the state the fold already has, and
        // `foldlevelstart` only decides how a fold *starts*.
        buf.set_auto_folds(&[(0, 4)], 99);
        assert!(
            buf.folds()[0].closed,
            "an existing fold keeps its state — foldlevelstart is start-only"
        );

        // A brand-new start row takes the option: level 1 <= 99 → open.
        let mut buf2 = b();
        buf2.set_auto_folds(&[(2, 4)], 99);
        assert!(
            !buf2.folds()[0].closed,
            "brand-new level-1 auto fold must start open at foldlevelstart=99"
        );
    }

    // ── foldlevelstart: level semantics, not a boolean ────────────────────
    //
    // The expectations below are neovim 0.12.4's, measured rather than
    // reasoned: the same nesting opened with `foldmethod=expr` +
    // `v:lua.vim.treesitter.foldexpr()` and `foldlevelstart` set to each
    // value, with the closed folds enumerated via `foldclosed` /
    // `foldclosedend` (`foldlevel()` merges adjacent siblings and reads as a
    // false difference). Converted from vim's 1-based lines to 0-based rows.
    //
    //  row  source                        fold        level
    //    2  function M.outer(a, b)        2..16         1
    //    3    if a > b then               3..14         2
    //    4      local t = {               4..7          3
    //   10      for i = 1, 10 do         10..13         3
    //   18  function M.second(c)         18..23         1
    //   19    while c > 0 do             19..21         2
    const NVIM_LUA_RANGES: &[(usize, usize)] =
        &[(2, 16), (3, 14), (4, 7), (10, 13), (18, 23), (19, 21)];

    /// Closed auto folds, as `(start_row, end_row)`, after one pass at `fls`.
    fn closed_at(ranges: &[(usize, usize)], fls: u32) -> Vec<(usize, usize)> {
        let mut buf = View::from_str(&"x\n".repeat(26));
        buf.set_auto_folds(ranges, fls);
        buf.folds()
            .iter()
            .filter(|f| f.closed)
            .map(|f| (f.start_row, f.end_row))
            .collect()
    }

    #[test]
    fn set_auto_folds_closes_folds_deeper_than_foldlevelstart() {
        // fls=0 → every fold closed (vim: level 1 > 0).
        assert_eq!(
            closed_at(NVIM_LUA_RANGES, 0),
            vec![(2, 16), (3, 14), (4, 7), (10, 13), (18, 23), (19, 21)],
            "foldlevelstart=0 must close every fold"
        );
        // fls=1 → level 1 open, levels 2+ closed. nvim closed 4..15 / 20..22
        // (1-based), i.e. the level-2 folds became the outermost closed ones.
        assert_eq!(
            closed_at(NVIM_LUA_RANGES, 1),
            vec![(3, 14), (4, 7), (10, 13), (19, 21)],
            "foldlevelstart=1 must open level 1 and close levels 2+"
        );
        // fls=2 → nvim closed 5..8 / 11..14 (1-based): the level-3 folds only.
        assert_eq!(
            closed_at(NVIM_LUA_RANGES, 2),
            vec![(4, 7), (10, 13)],
            "foldlevelstart=2 must close only level 3 and deeper"
        );
        // fls=3 and fls=99 → nvim closed nothing (max level here is 3).
        assert!(
            closed_at(NVIM_LUA_RANGES, 3).is_empty(),
            "foldlevelstart=3 must leave a 3-level nesting fully open"
        );
        assert!(
            closed_at(NVIM_LUA_RANGES, 99).is_empty(),
            "foldlevelstart=99 (hjkl's default) must open everything"
        );
    }

    #[test]
    fn set_auto_folds_levels_are_independent_of_range_order() {
        // Tree-sitter query order is capture order, not document order: the
        // level sweep must sort, not trust the caller.
        let mut shuffled = NVIM_LUA_RANGES.to_vec();
        shuffled.reverse();
        assert_eq!(
            closed_at(&shuffled, 1),
            closed_at(NVIM_LUA_RANGES, 1),
            "nesting level must come from containment, not from range order"
        );
    }

    #[test]
    fn set_auto_folds_levels_ignore_dropped_ranges() {
        // A single-row range never becomes a fold, so it must not push the
        // ranges around it a level deeper.
        let mut with_noise = NVIM_LUA_RANGES.to_vec();
        with_noise.push((5, 5)); // single row → dropped
        with_noise.push((9, 8)); // inverted → dropped
        assert_eq!(
            closed_at(&with_noise, 1),
            closed_at(NVIM_LUA_RANGES, 1),
            "ranges that never become folds must not count as a nesting level"
        );
    }

    #[test]
    fn set_auto_folds_manual_folds_do_not_add_a_nesting_level() {
        // A `zf` fold wrapping the whole file must not make every auto fold
        // one level deeper — the auto set's levels are its own.
        let mut buf = View::from_str(&"x\n".repeat(26));
        buf.add_fold(0, 25, false);
        buf.set_auto_folds(NVIM_LUA_RANGES, 1);
        let closed: Vec<(usize, usize)> = buf
            .folds()
            .iter()
            .filter(|f| f.auto_generated && f.closed)
            .map(|f| (f.start_row, f.end_row))
            .collect();
        assert_eq!(
            closed,
            vec![(3, 14), (4, 7), (10, 13), (19, 21)],
            "an enclosing manual fold must not shift auto fold levels"
        );
    }

    #[test]
    fn set_auto_folds_at_a_level_settles_without_bumping_generations() {
        // The mixed open/closed set a non-zero `foldlevelstart` produces has
        // to compare equal on the next pass, or the caller's once-per-edit
        // guard fires every frame.
        let mut buf = View::from_str(&"x\n".repeat(26));
        buf.set_auto_folds(NVIM_LUA_RANGES, 1);
        let fg = buf.fold_gen();
        let dg = buf.dirty_gen();
        for _ in 0..5 {
            buf.set_auto_folds(NVIM_LUA_RANGES, 1);
        }
        assert_eq!(
            buf.fold_gen(),
            fg,
            "unchanged fold set must not bump fold_gen"
        );
        assert_eq!(
            buf.dirty_gen(),
            dg,
            "unchanged fold set must not bump dirty_gen"
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
        assert_eq!(ranges, vec![(2, 3), (5, 7)]);
    }

    #[test]
    fn shift_folds_after_edit_restores_canonical_order_after_start_collapse() {
        let mut folds = vec![fold(0, 3), fold(1, 5)];

        super::shift_folds_after_edit(&mut folds, 0, 2, 2, -2);

        assert_eq!(
            folds
                .iter()
                .map(|fold| (fold.start_row, fold.end_row))
                .collect::<Vec<_>>(),
            vec![(0, 3), (0, 1)]
        );
        let index = super::FoldIndex::new(&folds);
        assert!(index.hides_row(2));
    }

    #[test]
    fn shift_folds_after_edit_deduplicates_exact_range_collisions() {
        let mut folds = vec![
            super::Fold {
                start_row: 0,
                end_row: 3,
                closed: false,
                auto_generated: false,
            },
            super::Fold {
                start_row: 1,
                end_row: 3,
                closed: true,
                auto_generated: true,
            },
        ];

        super::shift_folds_after_edit(&mut folds, 0, 2, 2, -2);

        assert_eq!(
            folds,
            vec![super::Fold {
                start_row: 0,
                end_row: 1,
                closed: false,
                auto_generated: false,
            }]
        );
    }

    #[test]
    fn shift_folds_after_edit_drops_fully_consumed() {
        let mut folds = vec![fold(4, 6)];
        super::shift_folds_after_edit(&mut folds, 4, 7, 7, -3);
        assert!(folds.is_empty());
    }

    // ── Borrow-style accessors + fold generation (round-2 perf item 10) ───

    #[test]
    fn with_folds_sees_the_same_data_as_folds() {
        let mut buf = b();
        // Empty case.
        assert_eq!(buf.with_folds(<[super::Fold]>::to_vec), buf.folds());
        assert!(!buf.has_folds());

        buf.add_fold(0, 1, false);
        buf.add_fold(2, 3, true);
        assert_eq!(buf.with_folds(<[super::Fold]>::to_vec), buf.folds());
        assert!(buf.has_folds());
        // And the borrow-style scan agrees with the owning one row by row.
        for row in 0..buf.row_count() {
            assert_eq!(
                buf.with_folds(|f| f.iter().any(|f| f.hides(row))),
                buf.folds().iter().any(|f| f.hides(row)),
                "row {row}"
            );
        }
    }

    #[test]
    fn fold_gen_bumps_on_every_mutator() {
        fn none(_: &mut View) {}
        fn open_fold(b: &mut View) {
            b.add_fold(1, 3, false);
        }
        fn closed_fold(b: &mut View) {
            b.add_fold(1, 3, true);
        }
        /// (name, setup, mutation-that-must-bump)
        type Case = (&'static str, fn(&mut View), fn(&mut View));
        let cases: [Case; 13] = [
            ("add_fold", none, |b| b.add_fold(1, 3, false)),
            ("set_auto_folds", none, |b| b.set_auto_folds(&[(1, 3)], 99)),
            ("remove_fold_at", open_fold, |b| {
                assert!(b.remove_fold_at(2));
            }),
            ("open_fold_at", closed_fold, |b| {
                assert!(b.open_fold_at(2));
            }),
            ("close_fold_at", open_fold, |b| {
                assert!(b.close_fold_at(2));
            }),
            ("toggle_fold_at", open_fold, |b| {
                assert!(b.toggle_fold_at(2));
            }),
            ("open_all_folds", closed_fold, View::open_all_folds),
            ("close_all_folds", open_fold, View::close_all_folds),
            ("clear_all_folds", open_fold, View::clear_all_folds),
            ("reveal_row", closed_fold, |b| {
                assert!(b.reveal_row(2));
            }),
            ("invalidate_folds_in_range", closed_fold, |b| {
                b.invalidate_folds_in_range(2, 2);
            }),
            ("rebase_folds", closed_fold, |b| b.rebase_folds(0, 0, 1, 1)),
            ("set_folds", none, |b| {
                b.set_folds(&[super::Fold {
                    start_row: 1,
                    end_row: 3,
                    closed: true,
                    auto_generated: false,
                }]);
            }),
        ];
        for (name, setup, mutate) in cases {
            let mut buf = b();
            setup(&mut buf);
            let before = buf.fold_gen();
            mutate(&mut buf);
            assert!(
                buf.fold_gen() > before,
                "{name} mutated the fold set and must bump fold_gen"
            );
        }
    }

    #[test]
    fn fold_gen_is_stable_across_reads_and_plain_text_edits() {
        let mut buf = b();
        buf.add_fold(1, 3, true);
        let fg = buf.fold_gen();
        assert!(fg > 0);

        // Pure reads.
        let _ = buf.folds();
        let _ = buf.with_folds(<[super::Fold]>::to_vec);
        let _ = buf.has_folds();
        let _ = buf.is_row_hidden(2);
        let _ = buf.fold_at_row(2);
        let _ = buf.next_visible_row(1);
        let _ = buf.prev_visible_row(4);
        assert_eq!(buf.fold_gen(), fg, "read-only queries must not bump");

        // No-op mutators (nothing actually changes).
        buf.close_all_folds(); // already closed
        buf.add_fold(9, 9, true); // out of bounds → rejected
        buf.rebase_folds(0, 0, 1, 0); // delta == 0 → early return
        let same = buf.folds();
        buf.set_folds(&same); // identical set
        assert_eq!(buf.fold_gen(), fg, "no-op mutators must not bump");

        // A plain text edit must not masquerade as a fold change — that is
        // the whole reason `fold_gen` is separate from `dirty_gen`.
        let dg = buf.dirty_gen();
        buf.apply_edit(crate::Edit::InsertChar {
            at: crate::Position { row: 0, col: 0 },
            ch: 'x',
        });
        assert_ne!(buf.dirty_gen(), dg, "text edit must bump dirty_gen");
        assert_eq!(buf.fold_gen(), fg, "text edit must not bump fold_gen");
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
