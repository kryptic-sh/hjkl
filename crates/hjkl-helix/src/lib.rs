//! Helix-style, selection-first keyboard discipline (#63 Phase D / #265).
//!
//! The second discipline to run on `hjkl-engine`, and the reason the engine was
//! made discipline-agnostic in the first place: it exists to prove that a
//! non-vim grammar needs **no engine changes**. This crate implements one trait
//! ([`hjkl_engine::DisciplineState`]) and drives the editor through its public
//! API. Nothing in `hjkl-engine` knows this crate exists.
//!
//! # Selection model
//!
//! Helix is selection-first: every motion produces a *selection*, and operators
//! act on it. A selection is `(anchor, head)`.
//!
//! The engine already stores the **heads** — the primary cursor plus
//! [`Editor::extra_cursors`] (#63). So this crate stores only the **anchors**.
//! That is the same split vim already uses for visual mode (its `visual_anchor`
//! lives in `VimState`, not the engine), and it means multi-cursor comes for
//! free: the engine shifts every head across every edit, and
//! [`Editor::edit_at_all_cursors`] fans an edit out over all of them.
//!
//! # Scope
//!
//! This is a working scaffold, not feature-parity with Helix. It implements the
//! grammar needed to exercise multi-cursor end to end: motions, `v` select mode,
//! `d`, `i`/`a`, `C` (add cursor below), `,` (collapse), and insert-mode typing
//! that lands at *every* cursor.
//!
//! What it deliberately does not do yet: ranged selections on the *secondary*
//! cursors. The primary carries an anchor, the secondaries are bare carets. A
//! secondary anchor would have to be kept in lockstep with `extra_cursors`,
//! which the engine may drop mid-edit — that needs a real answer, not a
//! hopeful `Vec` that silently desyncs. See the module TODO.
//!
//! [`Editor::extra_cursors`]: hjkl_engine::Editor::extra_cursors
//! [`Editor::edit_at_all_cursors`]: hjkl_engine::Editor::edit_at_all_cursors

use hjkl_buffer::{Buffer, Edit, MotionKind, Position};
use hjkl_engine::input::{Input, Key};
use hjkl_engine::types::Host;
use hjkl_engine::{CoarseMode, Editor};

mod motion;
mod normal;

pub use motion::Motion;

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

/// The helix FSM's state. Lives in the editor's type-erased discipline slot.
#[derive(Debug, Default)]
pub struct HelixState {
    pub mode: HelixMode,
    /// Anchor of the **primary** selection. The head is the engine's cursor, so
    /// `anchor == cursor` means a caret with no extent.
    pub anchor: Position,
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

    /// Idle for helix is Normal with a collapsed selection.
    fn reset_to_idle(&mut self) {
        self.mode = HelixMode::Normal;
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
            true
        }
        Key::Enter => {
            ed.push_undo();
            ed.edit_at_all_cursors(|at| Edit::InsertStr {
                at,
                text: "\n".to_string(),
            });
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
            true
        }
        _ => false,
    }
}
