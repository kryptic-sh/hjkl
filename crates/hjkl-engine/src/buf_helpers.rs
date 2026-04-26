//! Trait-surface cast helpers shared between [`crate::editor`] and
//! [`crate::vim`].
//!
//! Promoted from `editor.rs` in 0.0.42 (Patch C-δ.7) so the vim free
//! functions can route their `ed.buffer().*` reaches through the
//! `Cursor` / `Query` / `BufferEdit` trait surface using the same cast
//! primitives the editor body uses. Mirrors the pattern lifted into
//! `motions.rs` in 0.0.40.
//!
//! All helpers take a generic `B: <trait> + ?Sized` so they compile
//! against the in-tree `hjkl_buffer::Buffer` and the engine's mock
//! buffers (used by motion / search / vim trait-routing tests). The
//! `Pos { line: u32, col: u32 }` ⇄ `Position { row: usize, col: usize }`
//! cast lives at the boundary so call sites stay terse.

use crate::types::{Cursor, Query};

/// Read the cursor as a `(row, col)` `usize` tuple — the shape every
/// editor / vim free fn body expects. One inline cast at the trait
/// boundary.
#[inline]
pub(crate) fn buf_cursor_rc<B: Cursor + ?Sized>(b: &B) -> (usize, usize) {
    let p = Cursor::cursor(b);
    (p.line as usize, p.col as usize)
}

/// Read the cursor row.
#[inline]
pub(crate) fn buf_cursor_row<B: Cursor + ?Sized>(b: &B) -> usize {
    Cursor::cursor(b).line as usize
}

/// Read the cursor as an `hjkl_buffer::Position` — the shape the
/// concrete-buffer call sites consumed before the trait routing.
#[inline]
pub(crate) fn buf_cursor_pos<B: Cursor + ?Sized>(b: &B) -> hjkl_buffer::Position {
    let p = Cursor::cursor(b);
    hjkl_buffer::Position::new(p.line as usize, p.col as usize)
}

/// Set the cursor from `(row, col)` `usize` coordinates.
#[inline]
pub(crate) fn buf_set_cursor_rc<B: Cursor + ?Sized>(b: &mut B, row: usize, col: usize) {
    Cursor::set_cursor(
        b,
        crate::types::Pos {
            line: row as u32,
            col: col as u32,
        },
    );
}

/// Set the cursor from a concrete `hjkl_buffer::Position`. Routes the
/// `ed.buffer_mut().set_cursor(Position::new(...))` call sites in
/// `vim.rs` through the trait surface without a dedicated helper at
/// each site.
#[inline]
pub(crate) fn buf_set_cursor_pos<B: Cursor + ?Sized>(b: &mut B, pos: hjkl_buffer::Position) {
    buf_set_cursor_rc(b, pos.row, pos.col);
}

/// Number of rows.
#[inline]
pub(crate) fn buf_row_count<B: Query + ?Sized>(b: &B) -> usize {
    Query::line_count(b) as usize
}

/// Borrow line `row`, returning `None` for out-of-bounds. Mirrors the
/// pre-0.0.41 `hjkl_buffer::Buffer::line(row) -> Option<&str>` shape.
#[inline]
pub(crate) fn buf_line<B: Query + ?Sized>(b: &B, row: usize) -> Option<&str> {
    let n = Query::line_count(b) as usize;
    if row >= n {
        return None;
    }
    Some(Query::line(b, row as u32))
}

/// Snapshot every line into a `Vec<String>`. Allocates — call sites
/// that previously borrowed `lines() -> &[String]` and immediately
/// `.to_vec()`'d / `.iter().map(...)`'d collapse cleanly onto this.
#[inline]
pub(crate) fn buf_lines_to_vec<B: Query + ?Sized>(b: &B) -> Vec<String> {
    let n = Query::line_count(b) as usize;
    let mut out = Vec::with_capacity(n);
    for r in 0..n {
        out.push(Query::line(b, r as u32).to_string());
    }
    out
}

/// Length (chars) of `row`. Returns 0 for out-of-bounds rows so call
/// sites that previously did
/// `buf.line(r).map(|l| l.chars().count()).unwrap_or(0)` collapse to
/// one call.
#[inline]
pub(crate) fn buf_line_chars<B: Query + ?Sized>(b: &B, row: usize) -> usize {
    buf_line(b, row).map(|l| l.chars().count()).unwrap_or(0)
}

/// Length (bytes) of `row`. Returns 0 for out-of-bounds rows. The
/// byte-shape mirror of [`buf_line_chars`] — used by call sites that
/// pre-0.0.42 inspected `buf.lines()[row].len()`.
#[inline]
pub(crate) fn buf_line_bytes<B: Query + ?Sized>(b: &B, row: usize) -> usize {
    buf_line(b, row).map(|l| l.len()).unwrap_or(0)
}

/// Apply a [`hjkl_buffer::Edit`] and return the inverse for undo.
///
/// 0.0.42 (Patch C-δ.7): the `apply_edit` reach is intentionally kept
/// against the concrete `&mut hjkl_buffer::Buffer` rather than lifted
/// onto a trait method. Rationale:
///
/// - `hjkl_buffer::Edit` is the rich buffer-side enum (~8 variants —
///   `InsertChar`, `InsertStr`, `DeleteRange`, `JoinLines`,
///   `SplitLines`, `Replace`, `InsertBlock`, `DeleteBlockChunks`)
///   with ~700 LOC of `do_*` machinery in `hjkl-buffer`. Lifting it
///   onto `BufferEdit` would require either an associated `Edit` type
///   (forces every backend to design its own rich-edit enum just to
///   compile) or duplicating the 8 variants on the trait surface
///   (busts the discipline cap).
/// - `crate::types::Edit` is a separate value type (`Range<Pos>` +
///   `String` replacement) used by the change-log emitter; it's
///   intentionally simpler and lossy for block / join / split ops.
///
/// Centralizing the reach in this free fn keeps `Editor::mutate_edit`
/// trait-shaped at the call site (no `self.buffer.<inherent>` hop in
/// the editor body) and gives 0.1.0 a single seam to flip when the
/// `B: Buffer` generic lands.
///
/// The 0.1.0 design will introduce
/// `BufferEdit::apply_edit(&mut self, op: Self::Edit) -> Self::Edit`
/// with `type Edit;` so backends pick their own edit enum. This free
/// fn forwards there once that lands.
#[inline]
pub(crate) fn apply_buffer_edit(
    buf: &mut hjkl_buffer::Buffer,
    edit: hjkl_buffer::Edit,
) -> hjkl_buffer::Edit {
    buf.apply_edit(edit)
}
