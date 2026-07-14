//! Helix-style, selection-first keyboard discipline (#63 Phase D / #265).
//!
//! The second discipline to run on `hjkl-engine`, and the reason the engine was
//! made discipline-agnostic in the first place: it exists to prove that a
//! non-vim grammar needs no engine *special-casing*. This crate implements one
//! trait ([`hjkl_engine::DisciplineState`]) and drives the editor through its
//! public API. Nothing in `hjkl-engine` knows this crate exists.
//!
//! # Selection model
//!
//! Helix is selection-first: every motion produces a *selection*, and operators
//! act on it. A selection is `(anchor, head)`, both ends inclusive.
//!
//! - The **secondary** selections live in the engine, as
//!   [`hjkl_engine::Sel`]. Both ends, together — because the engine may DROP a
//!   selection it cannot track across an edit, and a discipline-side `Vec` of
//!   anchors running alongside the engine's carets would silently desync the
//!   moment that happened, pairing anchors with the wrong heads and landing the
//!   next edit on text nobody selected.
//! - The **primary** selection is asymmetric: its head is the engine's cursor
//!   ([`Editor::cursor`]) and its anchor is [`HelixState::anchor`], here. That
//!   is the same split vim uses for visual mode (its `visual_anchor` lives in
//!   `VimState`), it predates multi-cursor, and unifying it would mean rewriting
//!   vim's visual mode. It stays. [`sels`] and [`set_sels`] are the seam that
//!   makes the asymmetry invisible to the rest of this crate.
//!
//! [`Editor::cursor`]: hjkl_engine::Editor::cursor

use hjkl_buffer::{Buffer, Edit, MotionKind, Position};
use hjkl_engine::input::{Input, Key};
use hjkl_engine::types::Host;
use hjkl_engine::{CoarseMode, Editor, Sel};

mod doc;
mod motion;
mod normal;
mod ops;
mod word;

pub use motion::Motion;
pub use word::WordTarget;

/// Helix's mode set. Smaller than vim's: there is no operator-pending mode,
/// because the selection *is* the operand — you select first, then act.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HelixMode {
    /// Keys are commands. The selection is whatever the last motion produced.
    #[default]
    Normal,
    /// Keys are text. Typing lands at every cursor.
    Insert,
    /// Like Normal, but motions *extend* the selection instead of replacing it.
    Select,
}

/// A chord waiting for its second key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pending {
    /// `g` — goto mode (`gg`, `ge`, `gh`, `gl`, `gs`).
    Goto,
    /// `f` / `t` / `F` / `T` — waiting for the char to find.
    Find { till: bool, fwd: bool },
    /// `r` — waiting for the replacement char.
    Replace,
}

/// The helix FSM's state. Lives in the editor's type-erased discipline slot.
#[derive(Debug, Default)]
pub struct HelixState {
    pub mode: HelixMode,
    /// Anchor of the **primary** selection. Its head is the engine's cursor, so
    /// `anchor == cursor` means a caret with no extent. See the crate docs for
    /// why only this one lives outside the engine.
    pub anchor: Position,
    /// Count prefix under construction (`3w`). `0` means "no count".
    pub(crate) count: usize,
    /// A chord waiting for its second key.
    pub(crate) pending: Option<Pending>,
    /// The last `f` / `t` / `F` / `T`, kept for a future repeat binding.
    pub(crate) last_find: Option<(char, bool, bool)>,
}

impl HelixState {
    /// True when the primary selection has extent (anchor and head differ).
    pub fn has_extent(&self, head: Position) -> bool {
        self.anchor != head
    }
}

impl hjkl_engine::DisciplineState for HelixState {
    fn coarse_mode(&self) -> CoarseMode {
        match self.mode {
            HelixMode::Normal => CoarseMode::Normal,
            HelixMode::Insert => CoarseMode::Insert,
            // Helix's Select is character-wise; app chrome renders it like any
            // other "a selection is live" state.
            HelixMode::Select => CoarseMode::Select,
        }
    }

    /// Idle for helix is Normal with no half-typed count or chord.
    fn reset_to_idle(&mut self) {
        self.mode = HelixMode::Normal;
        self.count = 0;
        self.pending = None;
    }

    /// Only the mode. Deliberately weaker than `reset_to_idle` — see the trait
    /// docs: undo must not discard session state a non-modal discipline relies
    /// on, and the two hooks are not interchangeable.
    fn reset_mode_after_history(&mut self) {
        self.mode = HelixMode::Normal;
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// ─── Install ────────────────────────────────────────────────────────────────

/// Install the helix discipline on `ed`, replacing whatever was there.
pub fn install_helix_discipline<H: Host>(ed: &mut Editor<Buffer, H>) {
    ed.set_discipline(Box::new(HelixState::default()));
}

/// Build an [`Editor`] that interprets keys as helix.
pub fn helix_editor<H: Host>(
    buffer: Buffer,
    host: H,
    options: hjkl_engine::types::Options,
) -> Editor<Buffer, H> {
    let mut ed = Editor::new(buffer, host, options);
    install_helix_discipline(&mut ed);
    ed
}

// ─── Discipline downcast ────────────────────────────────────────────────────

/// Borrow the helix state out of the editor's type-erased discipline slot.
///
/// Panics if a different discipline is installed — that is a wiring bug (helix
/// keys dispatched at an editor that never had the helix discipline installed),
/// not a runtime condition to recover from.
pub(crate) fn hx<H: Host>(ed: &Editor<Buffer, H>) -> &HelixState {
    ed.discipline()
        .as_any()
        .downcast_ref::<HelixState>()
        .expect("helix discipline not installed on this Editor")
}

/// Mutable counterpart of [`hx`].
pub(crate) fn hx_mut<H: Host>(ed: &mut Editor<Buffer, H>) -> &mut HelixState {
    ed.discipline_mut()
        .as_any_mut()
        .downcast_mut::<HelixState>()
        .expect("helix discipline not installed on this Editor")
}

/// The primary cursor as a [`Position`] — the *head* of the primary selection.
pub(crate) fn head<H: Host>(ed: &Editor<Buffer, H>) -> Position {
    let (row, col) = ed.cursor();
    Position::new(row, col)
}

// ─── The selection set ──────────────────────────────────────────────────────

/// Every selection, **primary first**.
///
/// The one place the primary's split storage (head in the engine, anchor here)
/// is reassembled. Everything downstream — motions, operators — sees a flat list
/// and treats the primary like any other selection.
pub(crate) fn sels<H: Host>(ed: &Editor<Buffer, H>) -> Vec<Sel> {
    let mut out = Vec::with_capacity(1 + ed.extra_selections().len());
    out.push(Sel::new(hx(ed).anchor, head(ed)));
    out.extend_from_slice(ed.extra_selections());
    out
}

/// Write the selection set back. `sels[0]` becomes the primary.
///
/// The cursor is moved first on purpose: [`Editor::set_extra_selections`] drops
/// any secondary whose head collides with the primary's, and it can only do that
/// against the *new* primary.
pub(crate) fn set_sels<H: Host>(ed: &mut Editor<Buffer, H>, sels: &[Sel]) {
    let Some(primary) = sels.first() else { return };
    ed.set_cursor_quiet(primary.head.row, primary.head.col);
    hx_mut(ed).anchor = primary.anchor;
    ed.set_extra_selections(sels[1..].to_vec());
}

// ─── Entry point ────────────────────────────────────────────────────────────

/// Drive the helix FSM with one [`Input`]. Returns `true` if it was consumed.
pub fn dispatch_input<H: Host>(ed: &mut Editor<Buffer, H>, input: Input) -> bool {
    match hx(ed).mode {
        HelixMode::Insert => step_insert(ed, input),
        HelixMode::Normal | HelixMode::Select => normal::step(ed, input),
    }
}

// ─── Insert mode ────────────────────────────────────────────────────────────

/// Insert mode. Every keystroke lands at **every** cursor — that is the whole
/// point of the multi-cursor work, and it is why this goes through
/// `edit_at_all_cursors` rather than a bare `mutate_edit`.
fn step_insert<H: Host>(ed: &mut Editor<Buffer, H>, input: Input) -> bool {
    match input.key {
        Key::Esc => {
            hx_mut(ed).mode = HelixMode::Normal;
            let h = head(ed);
            hx_mut(ed).anchor = h;
            true
        }
        Key::Char(c) if !input.ctrl && !input.alt => {
            ed.push_undo();
            ed.edit_at_all_cursors(|at| Edit::InsertStr {
                at,
                text: c.to_string(),
            });
            sync_anchor_to_head(ed);
            true
        }
        Key::Enter => {
            ed.push_undo();
            ed.edit_at_all_cursors(|at| Edit::InsertStr {
                at,
                text: "\n".to_string(),
            });
            sync_anchor_to_head(ed);
            true
        }
        Key::Backspace => {
            ed.push_undo();
            ed.edit_at_all_cursors(|at| {
                // Nothing to delete at the very start of a line — hand back a
                // self-inverse no-op rather than inventing a negative column.
                if at.col == 0 {
                    return Edit::InsertStr {
                        at,
                        text: String::new(),
                    };
                }
                Edit::DeleteRange {
                    start: Position::new(at.row, at.col - 1),
                    end: at,
                    kind: MotionKind::Char,
                }
            });
            sync_anchor_to_head(ed);
            true
        }
        _ => false,
    }
}

/// Insert mode has no selection: keep the primary anchor glued to the caret so
/// leaving insert does not resurrect a stale range.
fn sync_anchor_to_head<H: Host>(ed: &mut Editor<Buffer, H>) {
    let h = head(ed);
    hx_mut(ed).anchor = h;
}
