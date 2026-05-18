//! Per-document text content. Arc-shareable across multiple [`crate::Buffer`]
//! views.
//!
//! [`Content`] owns everything that belongs to the document itself:
//!
//! - The `lines` rope (text content).
//! - The `dirty_gen` render-cache generation counter.
//! - Manual folds (`folds`).
//!
//! [`crate::Buffer`] is the per-window wrapper. It holds an
//! `Arc<Mutex<Content>>` plus the per-window cursor. Two `Buffer`
//! instances that share one `Content` see the same text and folds, but
//! each moves its cursor independently.
//!
//! ## Concurrency
//!
//! Held inside `Arc<Mutex<Content>>` so multiple `Buffer` views can share
//! one document safely. `Mutex` (not `RefCell`) because the engine's
//! `Cursor`, `Query`, `BufferEdit`, and `Search` traits require `Send`,
//! and `RefCell` is `!Send`. Lock contention is near-zero in the
//! single-threaded app loop; the Mutex is essentially a free `Send`
//! adapter.

use crate::folds::Fold;

/// Per-document state shared across all [`crate::Buffer`] views of the
/// same file. Wrap in `Arc<Mutex<Content>>` and pass to
/// [`crate::Buffer::new_view`] to create an additional window onto the
/// same content.
///
/// Fields intentionally parallel [`crate::Buffer`]'s pre-0.8 layout so
/// the diff stays mechanical: `lines`, `dirty_gen`, and `folds` moved
/// here; `cursor` stayed on `Buffer`.
pub struct Content {
    /// One entry per logical row. Always non-empty: a freshly
    /// constructed `Content` holds a single empty `String` so cursor
    /// positions never need an "is the buffer empty?" branch.
    pub(crate) lines: Vec<String>,
    /// Bumps on every mutation; render cache keys against this so a
    /// per-row `Line` gets recomputed when its source row changes.
    pub(crate) dirty_gen: u64,
    /// Manual folds — closed ranges hide rows in the render path.
    /// `pub(crate)` so the [`crate::folds`] module can read/write
    /// directly (same visibility as before the split).
    pub(crate) folds: Vec<Fold>,
}

impl Default for Content {
    fn default() -> Self {
        Self::new()
    }
}

impl Content {
    /// New empty content with one empty row.
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            dirty_gen: 0,
            folds: Vec::new(),
        }
    }

    /// Build content from a flat string. Splits on `\n`; a trailing
    /// `\n` produces a trailing empty line.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Self {
        let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        Self {
            lines,
            dirty_gen: 0,
            folds: Vec::new(),
        }
    }
}
