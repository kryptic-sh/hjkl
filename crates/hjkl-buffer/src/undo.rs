//! Undo/redo entry type for per-buffer undo history.
//!
//! Lives in `hjkl-buffer` so that [`crate::Content`] can own the undo stack
//! directly, keeping per-buffer state co-located with the rope.

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
pub struct UndoEntry {
    pub rope: ropey::Rope,
    pub cursor: (usize, usize),
    pub timestamp: SystemTime,
}
