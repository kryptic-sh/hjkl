use std::sync::{Arc, Mutex, MutexGuard};

use crate::content::Buffer;
use crate::{Position, Viewport};

/// Per-window view onto a [`Buffer`].
///
/// `View` is the type the rest of `hjkl-buffer` — and all consumers —
/// use directly. It owns exactly the state that is local to one editor
/// window:
///
/// - `cursor` — the charwise caret for this window.
///
/// All document-level state (text rope, dirty generation, folds) lives on
/// the inner [`Buffer`] and is accessed via `Arc<Mutex<Buffer>>`.
/// Two `View` instances that share the same `Arc` share text + folds
/// but carry independent cursors — the Helix Document+View model.
///
/// ## `Send` + `Sync`
///
/// `Arc<Mutex<Buffer>>` is `Send + Sync`, so `View` remains `Send`.
/// The engine trait surface requires `View: Send`; this constraint
/// drove the choice of `Mutex` over `RefCell`. The mutex is never
/// contended in normal operation (single-threaded app loop), so the
/// lock cost is negligible (~5 ns uncontested).
///
/// ## 0.8.0 migration notes
///
/// The existing constructors ([`View::new`], [`View::from_str`],
/// [`View::replace_all`], etc.) keep the same external signatures.
/// Callers that do not need multi-window sharing see no behaviour change.
/// Use [`View::new_view`] to create a second window onto the same
/// [`Buffer`].
///
/// ## Viewport
///
/// The rope invariant — at least one line, never empty — is preserved by
/// every mutation (ropey's empty rope already reports `len_lines() == 1`).
/// The viewport itself (top_row, top_col, width, height, wrap, text_width)
/// lives on the engine `Host` adapter; methods that need it take a
/// `&Viewport` / `&mut Viewport` parameter so the rope-walking math stays
/// here while runtime state lives there.
pub struct View {
    /// Shared per-document state (text rope, dirty gen, folds).
    pub(crate) content: Arc<Mutex<Buffer>>,
    /// Charwise cursor. `col` is bound by the char count of `row` in
    /// normal mode, one past it in operator-pending / insert.
    cursor: Position,
}

impl Default for View {
    fn default() -> Self {
        Self::new()
    }
}

impl View {
    // ── Constructors ──────────────────────────────────────────────

    /// Construct an empty buffer with one empty row + cursor at `(0, 0)`.
    pub fn new() -> Self {
        Self {
            content: Arc::new(Mutex::new(Buffer::new())),
            cursor: Position::default(),
        }
    }

    /// Build a buffer from a flat string. Splits on `\n`; a trailing
    /// `\n` produces a trailing empty line (matches every text
    /// editor's behaviour and keeps `from_text(buf.as_string())` an
    /// identity round-trip in the common case).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Self {
        Self {
            content: Arc::new(Mutex::new(Buffer::from_str(text))),
            cursor: Position::default(),
        }
    }

    /// Create a second per-window view onto existing [`Buffer`].
    ///
    /// The new `View` shares text + folds with every other view on the
    /// same `Arc`. Its cursor starts at `(0, 0)` independently. This is
    /// the primary entry point for split-window features.
    ///
    /// ```rust
    /// # use hjkl_buffer::{View, Buffer, Position};
    /// # use std::sync::Arc;
    /// # use std::sync::Mutex;
    /// let a = View::from_str("hello\nworld");
    /// let content = a.content_arc();
    /// let mut b = View::new_view(Arc::clone(&content));
    ///
    /// // Cursors are independent.
    /// let mut a = View::new_view(Arc::clone(&content));
    /// a.set_cursor(Position::new(1, 0));
    /// assert_eq!(b.cursor(), Position::new(0, 0));
    /// ```
    pub fn new_view(content: Arc<Mutex<Buffer>>) -> Self {
        Self {
            content,
            cursor: Position::default(),
        }
    }

    /// Return a clone of the `Arc<Mutex<Buffer>>` so callers can
    /// create additional views with [`View::new_view`].
    pub fn content_arc(&self) -> Arc<Mutex<Buffer>> {
        Arc::clone(&self.content)
    }

    // ── Read-only accessors (delegate to Buffer) ─────────────────

    pub fn cursor(&self) -> Position {
        self.cursor
    }

    /// The last cursor `(row, col)` committed on the shared [`Buffer`] by any
    /// view (see [`View::set_cursor`]). This is the "last-moved cursor across
    /// all windows" the cross-session cursor store persists — not this view's
    /// own live cursor. Best-effort; read at write/close/exit.
    pub fn last_cursor(&self) -> (usize, usize) {
        self.content.lock().unwrap().last_cursor
    }

    pub fn dirty_gen(&self) -> u64 {
        self.content.lock().unwrap().dirty_gen
    }

    /// Number of rows in the buffer. Always `>= 1`.
    pub fn row_count(&self) -> usize {
        self.content.lock().unwrap().text.len_lines()
    }

    /// Concatenate the rows into a single `String` joined by `\n`.
    ///
    /// Equivalent to `rope.to_string()` — ropey's rope-to-string already
    /// produces `\n`-joined content matching `split('\n').join("\n")`.
    pub fn as_string(&self) -> String {
        self.content.lock().unwrap().text.to_string()
    }

    // ── Cursor ops ────────────────────────────────────────────────

    /// Set cursor without scrolling. Clamps to valid positions.
    ///
    /// The optional sticky column for `j`/`k` motions is **not** reset
    /// by this call — it survives `set_cursor` intentionally.
    pub fn set_cursor(&mut self, pos: Position) {
        let mut c = self.content.lock().unwrap();
        let n = c.text.len_lines();
        let last_row = n.saturating_sub(1);
        let row = pos.row.min(last_row);
        let line_chars = rope_line_char_count(&c.text, row);
        let col = pos.col.min(line_chars);
        // Single choke point for cursor moves: record the last-moved cursor on
        // the shared `Buffer` so the most-recent move across ALL views onto
        // this document wins (docs/undo-architecture.md §6b). Cheap — no I/O.
        c.last_cursor = (row, col);
        drop(c);
        self.cursor = Position::new(row, col);
    }

    /// Bring the cursor into the visible [`Viewport`], scrolling by the
    /// minimum amount needed.
    pub fn ensure_cursor_visible(&mut self, viewport: &mut Viewport) {
        let cursor = self.cursor;
        let v = *viewport;
        let wrap_active = !matches!(v.wrap, crate::Wrap::None) && v.text_width > 0;
        if !wrap_active {
            viewport.ensure_visible(cursor);
            return;
        }
        if v.height == 0 {
            return;
        }
        // Cursor above the visible region: snap top_row to it.
        if cursor.row < v.top_row {
            viewport.top_row = cursor.row;
            viewport.top_col = 0;
            return;
        }
        let height = v.height as usize;
        // Compute the cursor's screen row once, then push `top_row` down
        // incrementally: each dropped row reduces the screen row by its own
        // visible height. O(distance) instead of recomputing
        // `cursor_screen_row_from` every step (which was O(distance^2) on a
        // large soft-wrapped jump).
        let mut screen = match self.cursor_screen_row_from(viewport, viewport.top_row) {
            Some(s) => s,
            None => {
                viewport.top_col = 0;
                return;
            }
        };
        while screen >= height {
            let c = self.content.lock().unwrap();
            let mut next = viewport.top_row + 1;
            while next <= cursor.row && c.folds.iter().any(|f| f.hides(next)) {
                next += 1;
            }
            if next > cursor.row {
                drop(c);
                viewport.top_row = cursor.row;
                break;
            }
            // Removing rows [top_row, next) drops their visible heights (hidden
            // rows contribute 0). After this, `screen` equals
            // `cursor_screen_row_from(next)`.
            for r in viewport.top_row..next {
                if c.folds.iter().any(|f| f.hides(r)) {
                    continue;
                }
                let line = rope_line_str(&c.text, r);
                screen -= crate::wrap::wrap_segments(&line, v.text_width, v.wrap).len();
            }
            drop(c);
            viewport.top_row = next;
        }
        viewport.top_col = 0;
    }

    /// Cursor's screen row offset (0-based) from `viewport.top_row`.
    pub fn cursor_screen_row(&self, viewport: &Viewport) -> Option<usize> {
        if matches!(viewport.wrap, crate::Wrap::None) || viewport.text_width == 0 {
            return None;
        }
        self.cursor_screen_row_from(viewport, viewport.top_row)
    }

    /// Number of screen rows the doc range `start..=end` occupies.
    pub fn screen_rows_between(&self, viewport: &Viewport, start: usize, end: usize) -> usize {
        if start > end {
            return 0;
        }
        let c = self.content.lock().unwrap();
        let n = c.text.len_lines();
        let last = n.saturating_sub(1);
        let end = end.min(last);
        let v = *viewport;
        let mut total = 0usize;
        for r in start..=end {
            if c.folds.iter().any(|f| f.hides(r)) {
                continue;
            }
            if matches!(v.wrap, crate::Wrap::None) || v.text_width == 0 {
                total += 1;
            } else {
                let line = rope_line_str(&c.text, r);
                total += crate::wrap::wrap_segments(&line, v.text_width, v.wrap).len();
            }
        }
        total
    }

    /// Earliest `top_row` such that `screen_rows_between(top, last)`
    /// is at least `height`.
    pub fn max_top_for_height(&self, viewport: &Viewport, height: usize) -> usize {
        if height == 0 {
            return 0;
        }
        let c = self.content.lock().unwrap();
        let n = c.text.len_lines();
        let last = n.saturating_sub(1);
        let mut total = 0usize;
        let mut row = last;
        loop {
            if !c.folds.iter().any(|f| f.hides(row)) {
                let v = *viewport;
                total += if matches!(v.wrap, crate::Wrap::None) || v.text_width == 0 {
                    1
                } else {
                    let line = rope_line_str(&c.text, row);
                    crate::wrap::wrap_segments(&line, v.text_width, v.wrap).len()
                };
            }
            if total >= height {
                return row;
            }
            if row == 0 {
                return 0;
            }
            row -= 1;
        }
    }

    /// Clamp `pos` to the buffer's content.
    pub fn clamp_position(&self, pos: Position) -> Position {
        let c = self.content.lock().unwrap();
        let n = c.text.len_lines();
        let last_row = n.saturating_sub(1);
        let row = pos.row.min(last_row);
        let line_chars = rope_line_char_count(&c.text, row);
        let col = pos.col.min(line_chars);
        Position::new(row, col)
    }

    /// Replace the buffer's full text in place. Cursor is clamped to
    /// the new content.
    pub fn replace_all(&mut self, text: &str) {
        let new_cursor = {
            let mut c = self.content.lock().unwrap();
            c.text = ropey::Rope::from_str(text);
            let n = c.text.len_lines();
            let last_row = n.saturating_sub(1);
            let row = self.cursor.row.min(last_row);
            let line_chars = rope_line_char_count(&c.text, row);
            let col = self.cursor.col.min(line_chars);
            c.dirty_gen = c.dirty_gen.wrapping_add(1);
            c.cached_joined = None;
            c.cached_byte_len = None;
            Position::new(row, col)
        };
        self.cursor = new_cursor;
    }

    // ── Crate-internal accessors (used by folds.rs) ───────────────

    /// Bump the render-cache generation. Crate-internal.
    pub(crate) fn dirty_gen_bump(&mut self) {
        let mut c = self.content.lock().unwrap();
        c.dirty_gen = c.dirty_gen.wrapping_add(1);
        c.cached_joined = None;
        c.cached_byte_len = None;
    }

    /// Canonical byte length of the document. `Rope::len_bytes()` is O(1)
    /// and returns the same value as `to_string().len()` (i.e.
    /// `sum(line_bytes) + (n_lines-1)` separators). Cached against
    /// `dirty_gen` for API compatibility; the O(1) rope call makes the
    /// cache essentially free but keeps the invalidation contract identical.
    pub fn byte_len(&self) -> usize {
        let mut c = self.content.lock().unwrap();
        let dg = c.dirty_gen;
        if let Some((cached_dg, len)) = c.cached_byte_len
            && cached_dg == dg
        {
            return len;
        }
        let total = c.text.len_bytes();
        c.cached_byte_len = Some((dg, total));
        total
    }

    /// Return an `Arc<String>` of the full document, cached against
    /// `dirty_gen`. Multiple per-tick consumers (syntax pipeline, LSP
    /// notify, git signature, dirty hash) share the same `Arc` for the
    /// same generation — first caller pays the `rope.to_string()` cost
    /// (one alloc + one lock), the rest are O(1).
    ///
    /// Cache invalidates automatically on every `dirty_gen_bump` and on
    /// `replace_all`, so callers never need to manage invalidation.
    pub fn content_joined(&self) -> std::sync::Arc<String> {
        let mut c = self.content.lock().unwrap();
        let dg = c.dirty_gen;
        if let Some((cached_dg, ref s)) = c.cached_joined
            && cached_dg == dg
        {
            return std::sync::Arc::clone(s);
        }
        let joined = std::sync::Arc::new(c.text.to_string());
        c.cached_joined = Some((dg, std::sync::Arc::clone(&joined)));
        joined
    }

    /// Borrow the underlying rope. Hot-path consumers (tree-sitter
    /// streaming parse, byte-range slicing) should use this instead of
    /// `content_joined()` to avoid materializing the whole document as
    /// a `String`.
    ///
    /// `ropey::Rope::clone` is O(1) — it Arc-clones the root node.
    /// The clone gives the caller a snapshot they can read without
    /// holding the content mutex.
    pub fn rope(&self) -> ropey::Rope {
        self.content.lock().unwrap().text.clone()
    }

    /// Shared access to the content guard. Crate-internal.
    pub(crate) fn content_lock(&self) -> MutexGuard<'_, Buffer> {
        self.content.lock().unwrap()
    }

    /// Exclusive access to Buffer. Crate-internal.
    pub(crate) fn content_lock_mut(&mut self) -> MutexGuard<'_, Buffer> {
        self.content.lock().unwrap()
    }

    // ── Screen-row helpers (private) ──────────────────────────────

    fn cursor_screen_row_from(&self, viewport: &Viewport, top: usize) -> Option<usize> {
        let cursor = self.cursor;
        if cursor.row < top {
            return None;
        }
        let c = self.content.lock().unwrap();
        // Clamp against the live rope: another view sharing this Buffer may
        // have removed rows since this view's cursor was last clamped, and
        // `rope.line(r)` panics past the last line.
        let cursor_row = cursor.row.min(c.text.len_lines().saturating_sub(1));
        if cursor_row < top {
            return None;
        }
        let v = *viewport;
        let mut screen = 0usize;
        for r in top..=cursor_row {
            if c.folds.iter().any(|f| f.hides(r)) {
                continue;
            }
            let line = rope_line_str(&c.text, r);
            let segs = crate::wrap::wrap_segments(&line, v.text_width, v.wrap);
            if r == cursor_row {
                let seg_idx = crate::wrap::segment_for_col(&segs, cursor.col);
                return Some(screen + seg_idx);
            }
            screen += segs.len();
        }
        None
    }

    // ── Per-buffer engine state accessors ─────────────────────────────────

    // ── Undo arena tree accessors (Phase 2a) ─────────────────────────────
    //
    // These delegate to the per-document [`crate::UndoTree`]. Their names and
    // semantics mirror the two-stack API they replace (`undo_stack` /
    // `redo_stack`), so the engine's undo/redo drivers are untouched apart
    // from the two `*_step` moves below.

    pub fn undo_stack_is_empty(&self) -> bool {
        self.content.lock().unwrap().undo.is_at_root()
    }

    pub fn redo_stack_is_empty(&self) -> bool {
        !self.content.lock().unwrap().undo.has_redo()
    }

    pub fn undo_stack_len(&self) -> usize {
        self.content.lock().unwrap().undo.depth()
    }

    /// Commit the pre-edit LIVE state `entry` as an undo boundary: the current
    /// node takes `entry` and a fresh child becomes current, with the forward
    /// (redo) branch dropped — the tree form of `undo_stack.push` + `clear_redo`.
    pub fn push_undo_entry(&self, entry: crate::UndoEntry) {
        self.content.lock().unwrap().undo.push(entry);
    }

    /// One undo move. `live` is the current buffer state (rope/cursor/marks) of
    /// the node being left; returns the parent snapshot to restore, or `None`
    /// at the root. See [`crate::UndoTree::undo_step`].
    pub fn undo_step(
        &self,
        rope: ropey::Rope,
        cursor: (usize, usize),
        marks: crate::MarkSnapshot,
    ) -> Option<crate::UndoEntry> {
        self.content
            .lock()
            .unwrap()
            .undo
            .undo_step(rope, cursor, marks)
    }

    /// One redo move. Symmetric to [`Self::undo_step`]; returns the child
    /// snapshot to restore, or `None` when there is no forward branch.
    pub fn redo_step(
        &self,
        rope: ropey::Rope,
        cursor: (usize, usize),
        marks: crate::MarkSnapshot,
    ) -> Option<crate::UndoEntry> {
        self.content
            .lock()
            .unwrap()
            .undo
            .redo_step(rope, cursor, marks)
    }

    /// Discard the most-recent undo boundary without moving the live state
    /// (`undo_stack.pop()`); `false` at the root. See
    /// [`crate::UndoTree::pop_committed`].
    pub fn pop_committed(&self) -> bool {
        self.content.lock().unwrap().undo.pop_committed()
    }

    /// One `g-` / `:earlier` step: move to the next-lower-`seq` state tree-wide,
    /// returning its snapshot to restore, or `None` at the lowest state. `live`
    /// is the current buffer state, stashed into the node being left. See
    /// [`crate::UndoTree::seq_earlier_step`].
    pub fn seq_earlier_step(
        &self,
        rope: ropey::Rope,
        cursor: (usize, usize),
        marks: crate::MarkSnapshot,
    ) -> Option<crate::UndoEntry> {
        self.content
            .lock()
            .unwrap()
            .undo
            .seq_earlier_step(rope, cursor, marks)
    }

    /// One `g+` / `:later` step: move to the next-higher-`seq` state tree-wide.
    /// Symmetric to [`Self::seq_earlier_step`].
    pub fn seq_later_step(
        &self,
        rope: ropey::Rope,
        cursor: (usize, usize),
        marks: crate::MarkSnapshot,
    ) -> Option<crate::UndoEntry> {
        self.content
            .lock()
            .unwrap()
            .undo
            .seq_later_step(rope, cursor, marks)
    }

    /// Timestamp of the next-lower-`seq` state (`:earlier Ns` predicate).
    pub fn seq_earlier_timestamp(&self) -> Option<std::time::SystemTime> {
        self.content.lock().unwrap().undo.seq_earlier_timestamp()
    }

    /// Timestamp of the next-higher-`seq` state (`:later Ns` predicate).
    pub fn seq_later_timestamp(&self) -> Option<std::time::SystemTime> {
        self.content.lock().unwrap().undo.seq_later_timestamp()
    }

    /// Undo-tree leaves for `:undolist`, each `(seq, changes/depth, timestamp,
    /// is_current)`, sorted by `seq`. See [`crate::UndoTree::leaves`].
    pub fn undo_leaves(&self) -> Vec<(u64, usize, std::time::SystemTime, bool)> {
        self.content.lock().unwrap().undo.leaves()
    }

    pub fn peek_undo_timestamp(&self) -> Option<std::time::SystemTime> {
        self.content.lock().unwrap().undo.parent_timestamp()
    }

    pub fn peek_redo_timestamp(&self) -> Option<std::time::SystemTime> {
        self.content.lock().unwrap().undo.child_timestamp()
    }

    pub fn clear_undo_redo(&self) {
        self.content.lock().unwrap().undo.clear_all();
    }

    // ── Undofile persistence (Phase 3b, docs/undo-architecture.md §6) ─────

    /// Project this buffer's undo tree into its serializable form for the
    /// undofile, plus the current node's `seq`. Syncs the current node to the
    /// live buffer text first, so the on-disk tree's `current` edge is exact
    /// even when `current` is a fresh (still-stale) leaf.
    pub fn undo_to_serializable(&self) -> (crate::SerTree, u64) {
        let mut c = self.content.lock().unwrap();
        let rope = c.text.clone();
        c.undo.sync_current(rope);
        let seq = c.undo.current_node_seq();
        (c.undo.to_serializable(), seq)
    }

    /// Replace this buffer's fresh single-node undo tree with one deserialized
    /// from an undofile. Must run BEFORE the first user edit and after the
    /// buffer text is populated (the tree's `current` materializes to the loaded
    /// content). Returns `false` — leaving the fresh tree untouched — if the
    /// projection is structurally inconsistent.
    pub fn install_undo_tree(&self, ser: &crate::SerTree) -> bool {
        match crate::UndoTree::from_serializable(ser) {
            Some(tree) => {
                self.content.lock().unwrap().undo = tree;
                true
            }
            None => false,
        }
    }

    pub fn clear_redo(&self) {
        self.content.lock().unwrap().undo.clear_redo();
    }

    /// Whether an undo group is currently open on this content (depth `> 0`).
    /// Used by the engine's `push_undo` to decide whether to coalesce.
    pub fn undo_group_active(&self) -> bool {
        self.content.lock().unwrap().undo_group_active()
    }

    /// Arm the open group's single snapshot. See [`Buffer::undo_group_arm`].
    pub fn undo_group_arm(&self) -> bool {
        self.content.lock().unwrap().undo_group_arm()
    }

    pub fn cap_undo(&self, cap: usize) {
        self.content.lock().unwrap().undo.cap(cap);
    }

    pub fn content_dirty(&self) -> bool {
        self.content.lock().unwrap().content_dirty
    }

    pub fn set_content_dirty(&self, v: bool) {
        self.content.lock().unwrap().content_dirty = v;
    }

    pub fn mark_content_dirty(&self) {
        let mut c = self.content.lock().unwrap();
        c.content_dirty = true;
        c.cached_editor_content = None;
    }

    pub fn take_dirty(&self) -> bool {
        let mut c = self.content.lock().unwrap();
        let v = c.content_dirty;
        c.content_dirty = false;
        v
    }

    pub fn cached_editor_content(&self) -> Option<std::sync::Arc<String>> {
        self.content.lock().unwrap().cached_editor_content.clone()
    }

    pub fn set_cached_editor_content(&self, arc: std::sync::Arc<String>) {
        self.content.lock().unwrap().cached_editor_content = Some(arc);
    }

    pub fn push_fold_op(&self, op: crate::FoldOp) {
        self.content.lock().unwrap().pending_fold_ops.push(op);
    }

    pub fn take_fold_ops(&self) -> Vec<crate::FoldOp> {
        std::mem::take(&mut self.content.lock().unwrap().pending_fold_ops)
    }

    pub fn extend_change_log(&self, edits: impl IntoIterator<Item = crate::EngineEdit>) {
        self.content.lock().unwrap().change_log.extend(edits);
    }

    pub fn take_change_log(&self) -> Vec<crate::EngineEdit> {
        std::mem::take(&mut self.content.lock().unwrap().change_log)
    }

    pub fn extend_pending_content_edits(
        &self,
        edits: impl IntoIterator<Item = crate::ContentEdit>,
    ) {
        self.content
            .lock()
            .unwrap()
            .pending_content_edits
            .extend(edits);
    }

    pub fn push_pending_content_edit(&self, edit: crate::ContentEdit) {
        self.content
            .lock()
            .unwrap()
            .pending_content_edits
            .push(edit);
    }

    pub fn take_pending_content_edits(&self) -> Vec<crate::ContentEdit> {
        std::mem::take(&mut self.content.lock().unwrap().pending_content_edits)
    }

    pub fn clear_pending_content_edits(&self) {
        self.content.lock().unwrap().pending_content_edits.clear();
    }

    pub fn pending_content_reset(&self) -> bool {
        self.content.lock().unwrap().pending_content_reset
    }

    pub fn set_pending_content_reset(&self, v: bool) {
        self.content.lock().unwrap().pending_content_reset = v;
    }

    pub fn take_pending_content_reset(&self) -> bool {
        let mut c = self.content.lock().unwrap();
        let v = c.pending_content_reset;
        c.pending_content_reset = false;
        v
    }

    pub fn mark(&self, c: char) -> Option<(usize, usize)> {
        self.content_lock().marks.get(&c).copied()
    }
    pub fn set_mark(&mut self, c: char, pos: (usize, usize)) {
        self.content_lock_mut().marks.insert(c, pos);
    }
    pub fn clear_mark(&mut self, c: char) {
        self.content_lock_mut().marks.remove(&c);
    }
    pub fn marks_cloned(&self) -> std::collections::BTreeMap<char, (usize, usize)> {
        self.content_lock().marks.clone()
    }
    pub fn set_marks(&mut self, marks: std::collections::BTreeMap<char, (usize, usize)>) {
        self.content_lock_mut().marks = marks;
    }
    /// Drop marks inside `[edit_start, drop_end)` and shift marks at/after
    /// `shift_threshold` by `delta` rows (clamped to 0). Mirrors the engine's
    /// edit-coherence pass for the per-buffer mark map (#154).
    pub fn rebase_marks(
        &mut self,
        edit_start: usize,
        drop_end: usize,
        shift_threshold: usize,
        delta: isize,
    ) {
        let mut c = self.content_lock_mut();
        let mut to_drop: Vec<char> = Vec::new();
        for (ch, (row, _col)) in c.marks.iter_mut() {
            if (edit_start..drop_end).contains(row) {
                to_drop.push(*ch);
            } else if *row >= shift_threshold {
                *row = ((*row as isize) + delta).max(0) as usize;
            }
        }
        for ch in to_drop {
            c.marks.remove(&ch);
        }
    }
    pub fn syntax_fold_ranges_cloned(&self) -> Vec<(usize, usize)> {
        self.content_lock().syntax_fold_ranges.clone()
    }
    pub fn set_syntax_fold_ranges(&mut self, ranges: Vec<(usize, usize)>) {
        self.content_lock_mut().syntax_fold_ranges = ranges;
    }
}

// ── Rope line helpers (free functions over &ropey::Rope) ─────────────

/// Return logical line `row` as a `String`, stripping the trailing `\n`
/// that ropey includes for non-final lines.
pub fn rope_line_str(rope: &ropey::Rope, row: usize) -> String {
    let mut s = rope.line(row).to_string();
    // ropey includes the trailing '\n' for non-final lines; strip it.
    if s.ends_with('\n') {
        s.pop();
    }
    s
}

/// Byte length of logical line `row` (excluding the trailing `\n`).
pub fn rope_line_bytes(rope: &ropey::Rope, row: usize) -> usize {
    let slice = rope.line(row);
    let bytes = slice.len_bytes();
    // ropey includes the '\n' byte for non-final lines; subtract it.
    if row + 1 < rope.len_lines() && bytes > 0 {
        bytes - 1
    } else {
        bytes
    }
}

/// Char count of logical line `row` (excluding the trailing `\n`).
pub(crate) fn rope_line_char_count(rope: &ropey::Rope, row: usize) -> usize {
    let slice = rope.line(row);
    let chars = slice.len_chars();
    // ropey includes the '\n' char for non-final lines; subtract it.
    if row + 1 < rope.len_lines() && chars > 0 {
        chars - 1
    } else {
        chars
    }
}

/// Char index from `(row, col)` where `col` is a char index within the line.
/// Both coordinates are clamped to the rope's bounds so a position that went
/// stale (e.g. another view shrank the shared `Buffer` between the caller's
/// clamp and this call) can never panic `line_to_char`.
pub(crate) fn pos_to_char_idx(rope: &ropey::Rope, row: usize, col: usize) -> usize {
    let row = row.min(rope.len_lines().saturating_sub(1));
    let line_start = rope.line_to_char(row);
    let line_char_count = rope_line_char_count(rope, row);
    line_start + col.min(line_char_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_one_empty_row() {
        let b = View::new();
        assert_eq!(b.row_count(), 1);
        assert_eq!(rope_line_str(&b.rope(), 0), "");
        assert_eq!(b.cursor(), Position::default());
    }

    #[test]
    fn from_str_splits_on_newline() {
        let b = View::from_str("foo\nbar\nbaz");
        assert_eq!(b.row_count(), 3);
        assert_eq!(rope_line_str(&b.rope(), 0), "foo");
        assert_eq!(rope_line_str(&b.rope(), 2), "baz");
    }

    #[test]
    fn from_str_trailing_newline_keeps_empty_row() {
        let b = View::from_str("foo\n");
        assert_eq!(b.row_count(), 2);
        assert_eq!(rope_line_str(&b.rope(), 1), "");
    }

    #[test]
    fn from_str_empty_input_keeps_one_row() {
        let b = View::from_str("");
        assert_eq!(b.row_count(), 1);
        assert_eq!(rope_line_str(&b.rope(), 0), "");
    }

    #[test]
    fn as_string_round_trips() {
        let b = View::from_str("a\nb\nc");
        assert_eq!(b.as_string(), "a\nb\nc");
    }

    #[test]
    fn dirty_gen_starts_at_zero() {
        assert_eq!(View::new().dirty_gen(), 0);
    }

    fn vp_wrap(width: u16, height: u16) -> Viewport {
        Viewport {
            top_row: 0,
            top_col: 0,
            width,
            height,
            wrap: crate::Wrap::Char,
            text_width: width,
            tab_width: 0,
        }
    }

    #[test]
    fn ensure_cursor_visible_wrap_scrolls_when_cursor_below_screen() {
        let mut b = View::from_str("aaaaaaaaaa\nb\nc");
        let mut v = vp_wrap(4, 3);
        b.set_cursor(Position::new(2, 0));
        b.ensure_cursor_visible(&mut v);
        assert_eq!(v.top_row, 1);
    }

    #[test]
    fn ensure_cursor_visible_wrap_no_scroll_when_visible() {
        let mut b = View::from_str("aaaaaaaaaa\nb");
        let mut v = vp_wrap(4, 4);
        b.set_cursor(Position::new(0, 5));
        b.ensure_cursor_visible(&mut v);
        assert_eq!(v.top_row, 0);
    }

    #[test]
    fn ensure_cursor_visible_wrap_snaps_top_when_cursor_above() {
        let mut b = View::from_str("a\nb\nc\nd\ne");
        let mut v = vp_wrap(4, 2);
        v.top_row = 3;
        b.set_cursor(Position::new(1, 0));
        b.ensure_cursor_visible(&mut v);
        assert_eq!(v.top_row, 1);
    }

    #[test]
    fn screen_rows_between_sums_segments_under_wrap() {
        let b = View::from_str("aaaaaaaaa\nb\n");
        let v = vp_wrap(4, 0);
        assert_eq!(b.screen_rows_between(&v, 0, 0), 3);
        assert_eq!(b.screen_rows_between(&v, 0, 1), 4);
        assert_eq!(b.screen_rows_between(&v, 0, 2), 5);
        assert_eq!(b.screen_rows_between(&v, 1, 2), 2);
    }

    #[test]
    fn screen_rows_between_one_per_doc_row_when_wrap_off() {
        let b = View::from_str("aaaaa\nb\nc");
        let v = Viewport::default();
        assert_eq!(b.screen_rows_between(&v, 0, 2), 3);
    }

    #[test]
    fn max_top_for_height_walks_back_until_height_reached() {
        let b = View::from_str("a\nb\nc\nd\neeeeeeee");
        let v = vp_wrap(4, 0);
        assert_eq!(b.max_top_for_height(&v, 4), 2);
        assert_eq!(b.max_top_for_height(&v, 99), 0);
    }

    #[test]
    fn cursor_screen_row_returns_none_when_wrap_off() {
        let b = View::from_str("a");
        let v = Viewport::default();
        assert!(b.cursor_screen_row(&v).is_none());
    }

    #[test]
    fn cursor_screen_row_under_wrap() {
        let mut b = View::from_str("aaaaaaaaaa\nb");
        let v = vp_wrap(4, 0);
        b.set_cursor(Position::new(0, 5));
        assert_eq!(b.cursor_screen_row(&v), Some(1));
        b.set_cursor(Position::new(1, 0));
        assert_eq!(b.cursor_screen_row(&v), Some(3));
    }

    /// Regression: a view whose cursor went stale after another view shrank
    /// the shared Buffer used to panic `rope.line()` inside
    /// `cursor_screen_row_from`. The row must clamp to the live rope.
    #[test]
    fn cursor_screen_row_survives_shrink_from_other_view() {
        let a = View::from_str("a\nb\nc\nd\ne");
        let arc = a.content_arc();
        let mut view_a = View::new_view(Arc::clone(&arc));
        let mut view_b = View::new_view(Arc::clone(&arc));
        view_b.set_cursor(Position::new(4, 0));
        // view_a truncates the document; view_b's cursor row 4 is now stale.
        view_a.replace_all("a");
        let v = vp_wrap(4, 3);
        assert_eq!(view_b.cursor_screen_row(&v), Some(0));
        let mut v2 = vp_wrap(4, 3);
        view_b.ensure_cursor_visible(&mut v2); // must not panic
    }

    #[test]
    fn ensure_cursor_visible_falls_back_when_wrap_disabled() {
        let mut b = View::from_str("a\nb\nc\nd\ne");
        let mut v = Viewport {
            top_row: 0,
            top_col: 0,
            width: 4,
            height: 2,
            wrap: crate::Wrap::None,
            text_width: 4,
            tab_width: 0,
        };
        b.set_cursor(Position::new(4, 0));
        b.ensure_cursor_visible(&mut v);
        assert_eq!(v.top_row, 3);
    }

    // ── Per-buffer engine state tests (new in 0.33.0 / Phase B) ──────

    /// Undo entries pushed via one `View` view are visible via
    /// another view sharing the same `Buffer` — proving that the
    /// undo stack lives on `Buffer`, not on the per-window `View`.
    #[test]
    fn undo_stack_shared_across_views() {
        use crate::UndoEntry;
        use std::time::SystemTime;

        let a = View::from_str("hello");
        let arc = a.content_arc();
        let view_a = View::new_view(Arc::clone(&arc));
        let view_b = View::new_view(Arc::clone(&arc));

        assert!(view_a.undo_stack_is_empty());
        assert_eq!(view_a.undo_stack_len(), 0);

        view_a.push_undo_entry(UndoEntry {
            rope: view_a.rope(),
            cursor: (0, 0),
            timestamp: SystemTime::UNIX_EPOCH,
            marks: Default::default(),
        });

        // Push via view_a is visible via view_b.
        assert_eq!(view_b.undo_stack_len(), 1);
        assert!(!view_b.undo_stack_is_empty());
    }

    /// A redo branch created via one view is visible via another — the undo
    /// tree lives on the shared `Buffer`, not the per-window `View`. (Phase 2a:
    /// re-expressed against the arena-tree API — a redo entry is now a forward
    /// child left behind by an undo move, not a `push_redo_entry` onto a Vec.)
    #[test]
    fn redo_stack_shared_across_views() {
        use crate::UndoEntry;
        use std::time::SystemTime;

        let a = View::from_str("world");
        let arc = a.content_arc();
        let view_a = View::new_view(Arc::clone(&arc));
        let view_b = View::new_view(Arc::clone(&arc));

        assert!(view_a.redo_stack_is_empty());

        // Commit an undo boundary via view_b, then undo it — that leaves the
        // node we left (cursor (0, 2)) as a forward/redo branch on the shared
        // Content.
        view_b.push_undo_entry(UndoEntry {
            rope: view_b.rope(),
            cursor: (0, 0),
            timestamp: SystemTime::UNIX_EPOCH,
            marks: Default::default(),
        });
        view_b.undo_step(view_b.rope(), (0, 2), Default::default());

        // The redo branch is visible + walkable via view_a.
        assert!(!view_a.redo_stack_is_empty());
        let entry = view_a.redo_step(view_a.rope(), (0, 0), Default::default());
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().cursor, (0, 2));
    }

    /// `clear_undo_redo` collapses the shared undo tree to a single node, wiping
    /// both directions, and the effect is visible from every view. (Phase 2a:
    /// the redo side is now seeded by an undo move rather than a raw
    /// `push_redo_entry`.)
    #[test]
    fn clear_undo_redo_shared_across_views() {
        use crate::UndoEntry;
        use std::time::SystemTime;

        let a = View::from_str("abc");
        let arc = a.content_arc();
        let view_a = View::new_view(Arc::clone(&arc));
        let view_b = View::new_view(Arc::clone(&arc));

        // Two boundaries then one undo → undo side AND redo side both populated.
        for _ in 0..2 {
            view_a.push_undo_entry(UndoEntry {
                rope: view_a.rope(),
                cursor: (0, 0),
                timestamp: SystemTime::UNIX_EPOCH,
                marks: Default::default(),
            });
        }
        view_a.undo_step(view_a.rope(), (0, 1), Default::default());
        assert!(!view_a.undo_stack_is_empty());
        assert!(!view_a.redo_stack_is_empty());

        view_b.clear_undo_redo();
        assert!(view_a.undo_stack_is_empty());
        assert!(view_a.redo_stack_is_empty());
    }

    /// `content_dirty` flag is shared across views.
    #[test]
    fn content_dirty_shared_across_views() {
        let a = View::from_str("test");
        let arc = a.content_arc();
        let view_a = View::new_view(Arc::clone(&arc));
        let view_b = View::new_view(Arc::clone(&arc));

        assert!(!view_a.content_dirty());

        view_b.mark_content_dirty();
        assert!(view_a.content_dirty());

        let taken = view_a.take_dirty();
        assert!(taken);
        assert!(!view_b.content_dirty());
    }

    /// `pending_fold_ops` push and take are shared across views.
    #[test]
    fn pending_fold_ops_shared_across_views() {
        let a = View::from_str("a\nb\nc");
        let arc = a.content_arc();
        let view_a = View::new_view(Arc::clone(&arc));
        let view_b = View::new_view(Arc::clone(&arc));

        view_a.push_fold_op(crate::FoldOp::Add {
            start_row: 0,
            end_row: 1,
            closed: true,
        });

        let ops = view_b.take_fold_ops();
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            ops[0],
            crate::FoldOp::Add {
                start_row: 0,
                end_row: 1,
                closed: true
            }
        ));
    }

    /// `pending_content_reset` flag is shared across views.
    #[test]
    fn pending_content_reset_shared_across_views() {
        let a = View::from_str("x");
        let arc = a.content_arc();
        let view_a = View::new_view(Arc::clone(&arc));
        let view_b = View::new_view(Arc::clone(&arc));

        assert!(!view_a.pending_content_reset());
        view_b.set_pending_content_reset(true);
        assert!(view_a.pending_content_reset());
        let taken = view_a.take_pending_content_reset();
        assert!(taken);
        assert!(!view_b.pending_content_reset());
    }

    // ── View-split tests (new in 0.8.0) ──────────────────────────

    /// Two `View` views sharing one `Buffer` must have independent
    /// cursors.
    #[test]
    fn buffer_views_independent_cursors() {
        let a = View::from_str("hello\nworld");
        let arc = a.content_arc();
        let mut view_a = View::new_view(Arc::clone(&arc));
        let mut view_b = View::new_view(Arc::clone(&arc));

        view_a.set_cursor(Position::new(1, 3));
        // view_b cursor must remain at (0, 0).
        assert_eq!(view_b.cursor(), Position::new(0, 0));

        view_b.set_cursor(Position::new(0, 2));
        // view_a cursor must remain at (1, 3).
        assert_eq!(view_a.cursor(), Position::new(1, 3));
    }

    /// `last_cursor` on the shared `Buffer` reflects the most recent move
    /// across two independent `View`s — the "last-moved window wins" contract
    /// the cross-session cursor store depends on (docs §6b).
    #[test]
    fn last_cursor_reflects_most_recent_move_across_views() {
        let a = View::from_str("aaaa\nbbbb\ncccc\ndddd");
        let arc = a.content_arc();
        let mut view_a = View::new_view(Arc::clone(&arc));
        let mut view_b = View::new_view(Arc::clone(&arc));

        view_a.set_cursor(Position::new(1, 2));
        assert_eq!(view_a.last_cursor(), (1, 2));
        // Both views see the same shared last_cursor.
        assert_eq!(view_b.last_cursor(), (1, 2));

        // A later move on view_b wins.
        view_b.set_cursor(Position::new(3, 1));
        assert_eq!(view_a.last_cursor(), (3, 1));
        assert_eq!(view_b.last_cursor(), (3, 1));

        // last_cursor stores the CLAMPED landing position, not the request.
        view_a.set_cursor(Position::new(99, 99));
        assert_eq!(view_a.last_cursor(), (3, 4));
    }

    /// Cursor-restore clamp contract (docs §6b): a stored row past EOF clamps
    /// to the last line, and a col past the line's char count clamps to its
    /// length. `clamp_position` is what `build_slot` runs on the stored cursor.
    #[test]
    fn clamp_position_restore_semantics() {
        let b = View::from_str("ab\ncdef\ng");
        // Row past the last line (2) → clamped to last line, col to its length.
        assert_eq!(b.clamp_position(Position::new(99, 99)), Position::new(2, 1));
        // Col past the line's char count → clamped to the char count (4).
        assert_eq!(b.clamp_position(Position::new(1, 99)), Position::new(1, 4));
        // In-bounds position is unchanged (exact restore on a content match).
        assert_eq!(b.clamp_position(Position::new(1, 2)), Position::new(1, 2));
    }

    /// An edit applied via one view must be visible via the other.
    #[test]
    fn buffer_views_share_content() {
        use crate::edit::Edit;

        let a = View::from_str("foo");
        let arc = a.content_arc();
        let mut view_a = View::new_view(Arc::clone(&arc));
        let view_b = View::new_view(Arc::clone(&arc));

        view_a.apply_edit(Edit::InsertStr {
            at: Position::new(0, 3),
            text: "bar".into(),
        });

        assert_eq!(rope_line_str(&view_a.rope(), 0), "foobar");
        assert_eq!(rope_line_str(&view_b.rope(), 0), "foobar");
    }
}

#[cfg(test)]
mod marks_shared_content_tests {
    use super::*;

    #[test]
    fn marks_shared_across_views() {
        // Two View views on the same Buffer share marks (#154).
        let a = View::from_str("hello\nworld");
        let content = a.content_arc();
        let mut view_a = View::new_view(std::sync::Arc::clone(&content));
        let view_b = View::new_view(std::sync::Arc::clone(&content));

        // Set mark 'x' on view_a.
        view_a.set_mark('x', (1, 3));

        // view_b must see the same mark via shared Buffer.
        assert_eq!(view_b.mark('x'), Some((1, 3)));
    }

    #[test]
    fn syntax_fold_ranges_shared_across_views() {
        let a = View::from_str("fn foo() {\n  bar();\n}");
        let content = a.content_arc();
        let mut view_a = View::new_view(std::sync::Arc::clone(&content));
        let view_b = View::new_view(std::sync::Arc::clone(&content));

        view_a.set_syntax_fold_ranges(vec![(0, 2)]);

        assert_eq!(view_b.syntax_fold_ranges_cloned(), vec![(0, 2)]);
    }
}
