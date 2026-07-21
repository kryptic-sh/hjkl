//! Per-document text content. Arc-shareable across multiple [`crate::View`]
//! views.
//!
//! [`Buffer`] owns everything that belongs to the document itself:
//!
//! - The `text` rope (text content).
//! - The `dirty_gen` render-cache generation counter.
//! - Manual folds (`folds`).
//!
//! [`crate::View`] is the per-window wrapper. It holds an
//! `Arc<Mutex<Buffer>>` plus the per-window cursor. Two `View`
//! instances that share one `Buffer` see the same text and folds, but
//! each moves its cursor independently.
//!
//! ## Concurrency
//!
//! Held inside `Arc<Mutex<Buffer>>` so multiple `View` views can share
//! one document safely. `Mutex` (not `RefCell`) because the engine's
//! `Cursor`, `Query`, `BufferEdit`, and `Search` traits require `Send`,
//! and `RefCell` is `!Send`. Lock contention is near-zero in the
//! single-threaded app loop; the Mutex is essentially a free `Send`
//! adapter.

use crate::folds::Fold;

/// Per-document state shared across all [`crate::View`] views of the
/// same file. Wrap in `Arc<Mutex<Buffer>>` and pass to
/// [`crate::View::new_view`] to create an additional window onto the
/// same content.
///
/// Uses a `ropey::Rope` for O(log N) edits and O(1) byte-length queries.
/// The rope always contains at least one logical line: a freshly constructed
/// `Buffer` holds an empty rope (which `ropey` reports as 1 line) so
/// cursor positions never need an "is the buffer empty?" branch.
///
/// ## Line semantics
///
/// `ropey::Rope::len_lines()` and `split('\n').count()` agree for all inputs:
/// - `""` → 1 line
/// - `"foo\n"` → 2 lines (trailing empty line)
/// - `"a\nb\n"` → 3 lines
///
/// `Rope::line(i)` returns a `RopeSlice` that includes the trailing `\n`
/// for non-final lines. Public accessors strip it before returning `String`.
pub struct Buffer {
    /// Rope-backed document text. Always non-empty: `ropey::Rope::new()`
    /// (an empty rope) reports `len_lines() == 1`, satisfying the "at least
    /// one row" invariant without a separate sentinel.
    pub(crate) text: ropey::Rope,
    /// Bumps on every mutation; render cache keys against this so a
    /// per-row `Line` gets recomputed when its source row changes.
    pub(crate) dirty_gen: u64,
    /// Manual folds — closed ranges hide rows in the render path.
    /// `pub(crate)` so the [`crate::folds`] module can read/write
    /// directly (same visibility as before the split).
    pub(crate) folds: Vec<Fold>,
    /// Cached `rope.to_string()` keyed by the `dirty_gen` at build time.
    /// Multiple per-tick consumers (syntax submit, LSP notify, git
    /// signature, dirty hash) all need the joined document; rebuilding
    /// per consumer was ~4× the line-clone + alloc cost per keystroke
    /// on a 400-line file (visible as insert-mode lag).
    pub(crate) cached_joined: Option<(u64, std::sync::Arc<String>)>,
    /// Cached canonical byte length keyed by `dirty_gen` at compute time.
    /// `Rope::len_bytes()` is O(1) but holding the cache avoids even that
    /// small overhead on repeated callers within the same tick.
    pub(crate) cached_byte_len: Option<(u64, usize)>,

    // ── Per-buffer engine state (relocated from hjkl-engine::Editor) ──────
    /// Undo history: O(1)-clone rope snapshots before each edit group.
    pub(crate) undo_stack: Vec<crate::UndoEntry>,
    /// Redo history: entries pushed when the user undoes.
    pub(crate) redo_stack: Vec<crate::UndoEntry>,
    /// Undo-group nesting depth. `> 0` while an [`crate::UndoGroup`] guard is
    /// live (see hjkl-engine). At depth `0` `push_undo` behaves exactly as it
    /// always has (one entry per call); at depth `> 0` every mutation inside
    /// the outermost group coalesces into a single undo entry.
    pub(crate) undo_group_depth: u32,
    /// Set once the outermost open group has taken its single pre-group
    /// snapshot; every later `push_undo` in the group is then suppressed.
    pub(crate) undo_group_armed: bool,
    /// `dirty_gen` captured when the outermost group opened. If it is
    /// unchanged when the group closes, the group mutated nothing and its
    /// armed snapshot is popped so a no-op group leaves zero undo entries.
    pub(crate) undo_group_open_gen: u64,
    /// Set whenever the buffer content changes; cleared by the engine's
    /// `take_dirty` accessor.
    pub(crate) content_dirty: bool,
    /// Cached `Arc<String>` of the joined document for the engine's
    /// `content_arc` fast path. Invalidated by `mark_content_dirty`.
    pub(crate) cached_editor_content: Option<std::sync::Arc<String>>,
    /// Pending [`crate::FoldOp`]s raised by `z…` keystrokes, `:fold*` ex
    /// commands, and the edit-pipeline's fold invalidation. Drained by
    /// hosts via `Editor::take_fold_ops`.
    pub(crate) pending_fold_ops: Vec<crate::FoldOp>,
    /// Pending edit log drained by `Editor::take_changes`. Each entry is
    /// a [`crate::EngineEdit`] mapped from the underlying buffer edit.
    pub(crate) change_log: Vec<crate::EngineEdit>,
    /// Pending `ContentEdit` records emitted by `mutate_edit`. Drained by
    /// hosts via `Editor::take_content_edits` for fan-in to a syntax tree.
    pub(crate) pending_content_edits: Vec<crate::ContentEdit>,
    /// Pending "reset" flag set when the entire buffer is replaced
    /// (e.g. `set_content` / `restore`). Supersedes any queued
    /// `pending_content_edits` on the same frame.
    pub(crate) pending_content_reset: bool,
    /// Named marks (`'a`–`'z`, `'A`–`'Z`) — buffer-scoped cursor positions
    /// `(row, col)`. Shared across all window views of this buffer (#154).
    pub(crate) marks: std::collections::BTreeMap<char, (usize, usize)>,
    /// Cached syntax-derived foldable block ranges that `:foldsyntax`
    /// consumes; a property of the buffer content, shared across views (#154).
    pub(crate) syntax_fold_ranges: Vec<(usize, usize)>,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Buffer {
    /// New empty content with one empty row.
    pub fn new() -> Self {
        Self {
            text: ropey::Rope::new(),
            dirty_gen: 0,
            folds: Vec::new(),
            cached_joined: None,
            cached_byte_len: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            undo_group_depth: 0,
            undo_group_armed: false,
            undo_group_open_gen: 0,
            content_dirty: false,
            cached_editor_content: None,
            pending_fold_ops: Vec::new(),
            change_log: Vec::new(),
            pending_content_edits: Vec::new(),
            pending_content_reset: false,
            marks: std::collections::BTreeMap::new(),
            syntax_fold_ranges: Vec::new(),
        }
    }

    /// Build content from a flat string. Splits on `\n`; a trailing
    /// `\n` produces a trailing empty line (matches ropey's own convention).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Self {
        Self {
            text: ropey::Rope::from_str(text),
            dirty_gen: 0,
            folds: Vec::new(),
            cached_joined: None,
            cached_byte_len: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            undo_group_depth: 0,
            undo_group_armed: false,
            undo_group_open_gen: 0,
            content_dirty: false,
            cached_editor_content: None,
            pending_fold_ops: Vec::new(),
            change_log: Vec::new(),
            pending_content_edits: Vec::new(),
            pending_content_reset: false,
            marks: std::collections::BTreeMap::new(),
            syntax_fold_ranges: Vec::new(),
        }
    }

    // ── Undo-group coalescing (Phase 1, see docs/undo-architecture.md §4) ──
    //
    // A group makes a composed operation (`:g`, `:normal`, a macro replay)
    // record ONE undo step instead of one per underlying `push_undo`. The
    // depth counter is re-entrant: nested groups just nest, only the
    // outermost close commits.

    /// Open (nest into) an undo group. On the outermost open (depth `0→1`)
    /// record the current `dirty_gen` and disarm, so the group's first
    /// mutating `push_undo` takes exactly one snapshot.
    pub fn undo_group_enter(&mut self) {
        if self.undo_group_depth == 0 {
            self.undo_group_armed = false;
            self.undo_group_open_gen = self.dirty_gen;
        }
        self.undo_group_depth = self.undo_group_depth.saturating_add(1);
    }

    /// Close (unnest) an undo group. On the outermost close (depth `1→0`), if
    /// the group armed a snapshot but `dirty_gen` is unchanged since it opened
    /// (nothing was mutated), pop that snapshot so a no-op group leaves zero
    /// undo entries. Resets the group flags.
    pub fn undo_group_exit(&mut self) {
        if self.undo_group_depth == 0 {
            return;
        }
        self.undo_group_depth -= 1;
        if self.undo_group_depth == 0 {
            if self.undo_group_armed && self.dirty_gen == self.undo_group_open_gen {
                self.undo_stack.pop();
            }
            self.undo_group_armed = false;
        }
    }

    /// Whether an undo group is currently open (depth `> 0`).
    pub fn undo_group_active(&self) -> bool {
        self.undo_group_depth > 0
    }

    /// Arm the open group's single snapshot. Returns `true` if this call armed
    /// it (the first mutating `push_undo` in the group, which must take the
    /// snapshot), `false` if it was already armed (a later push to suppress).
    pub fn undo_group_arm(&mut self) -> bool {
        if self.undo_group_armed {
            false
        } else {
            self.undo_group_armed = true;
            true
        }
    }
}
