//! Undo/redo entry type for per-buffer undo history.
//!
//! Lives in `hjkl-buffer` so that [`crate::Buffer`] can own the undo stack
//! directly, keeping per-buffer state co-located with the rope.

use std::collections::BTreeMap;
use std::time::SystemTime;

/// A single entry in the undo or redo stack.
///
/// The `timestamp` records the wall-clock time at which the snapshot was
/// taken (i.e. when `push_undo` was called), enabling the `:earlier` /
/// `:later` time-travel ex commands to walk the stack by duration rather
/// than by step count.
///
/// Stored as a `ropey::Rope` (O(1) Arc-clone) rather than a `String` so
/// snapshot cost is negligible even on multi-MB buffers.
#[derive(Debug, Clone)]
pub struct UndoEntry {
    pub rope: ropey::Rope,
    pub cursor: (usize, usize),
    pub timestamp: SystemTime,
    /// Local marks / jumplist / changelist / this-buffer's-global-marks
    /// snapshot, so undo/redo restore mark-ish positions alongside the
    /// text instead of leaving them shifted by the edit being undone
    /// (audit-r2 fix 2). `Default::default()` (all empty) for callers
    /// that don't populate it — restoring an all-empty snapshot is a
    /// no-op against a freshly-constructed buffer's own empty state, so
    /// existing fixtures that only care about text/cursor stay valid.
    pub marks: MarkSnapshot,
}

/// Buffer-scoped "edit coherence" state snapshotted alongside a
/// [`UndoEntry`]'s rope so undo/redo can restore marks, not just text.
///
/// Positions are plain `(row, col)` (or `(row, col)` values keyed by
/// mark char) — no buffer-id tagging needed here even for
/// `global_marks`, because a `MarkSnapshot` always belongs to exactly
/// one buffer's undo stack; the engine is responsible for reattaching
/// its own `buffer_id` when writing entries back into the session-global
/// marks map (see `Editor::restore_marks`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarkSnapshot {
    /// `ma`-`mz` local marks (`View::marks_cloned`).
    pub local_marks: BTreeMap<char, (usize, usize)>,
    /// Back-jumplist (`Ctrl-o` stack), newest at the back.
    pub jump_back: Vec<(usize, usize)>,
    /// Forward-jumplist (`Ctrl-i` stack), newest at the back.
    pub jump_fwd: Vec<(usize, usize)>,
    /// `` `. ``  / `'.` — position of the most recent change.
    pub change_last_edit: Option<(usize, usize)>,
    /// Changelist ring (`g;` / `g,`).
    pub change_list: Vec<(usize, usize)>,
    /// Walk cursor into `change_list`; `None` outside a walk.
    pub change_cursor: Option<usize>,
    /// `mA`-`mZ` global marks that belong to THIS buffer (bare
    /// `(row, col)` — the buffer-id is implicit, this buffer).
    pub global_marks: BTreeMap<char, (usize, usize)>,
}
