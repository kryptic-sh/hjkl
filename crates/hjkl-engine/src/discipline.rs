//! Pluggable input-discipline state (#265 G3 / #267).
//!
//! The engine owns the editing *core* (buffer, undo, registers, marks,
//! search, viewport) but is agnostic about the *keybinding discipline* (vim,
//! vscode, future helix/emacs). Each discipline's FSM state lives in its own
//! crate (e.g. `hjkl-vim`'s `VimState`) and is stored on the [`Editor`] through
//! this type-erased slot — the engine never names the concrete state type.
//!
//! The trait is intentionally **minimal**: the engine only asks a discipline
//! for its [`CoarseMode`] (status badge / cursor shape) plus an `Any` upcast.
//! All discipline-specific behavior lives behind that downcast in the
//! discipline crate (e.g. `hjkl_vim::vim_state`), never on this trait.
//!
//! [`Editor`]: crate::Editor

use crate::CoarseMode;

/// Discipline-private FSM state, stored type-erased on the [`Editor`].
///
/// [`Editor`]: crate::Editor
pub trait DisciplineState: std::any::Any + std::fmt::Debug {
    /// Discipline-agnostic coarse mode for app chrome (status badge, cursor
    /// shape). Every discipline projects its internal mode onto this.
    fn coarse_mode(&self) -> CoarseMode;

    /// Upcast to `&dyn Any` so the owning crate can `downcast_ref` to its
    /// concrete state type.
    fn as_any(&self) -> &dyn std::any::Any;

    /// Mutable upcast — see [`DisciplineState::as_any`].
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;

    /// Return the discipline to its idle / command-ready state, discarding any
    /// in-flight input (pending chords, counts, insert sessions).
    ///
    /// The engine calls this when an operation must leave the editor in a known
    /// resting state regardless of which discipline is installed — after undo /
    /// redo, and after a `:!` filter rewrites the buffer. Without this hook the
    /// engine core would have to name vim to reset vim (#267).
    ///
    /// For vim this is Normal mode; a non-modal discipline may treat it as a
    /// no-op.
    fn reset_to_idle(&mut self);

    /// Put the discipline's *mode* back to idle after undo / redo rewound the
    /// buffer, WITHOUT discarding in-flight session state.
    ///
    /// Deliberately weaker than [`DisciplineState::reset_to_idle`]: undo must
    /// not clear an open insert session, because non-modal disciplines (vscode)
    /// live inside one permanently and rely on it for undo granularity. Getting
    /// this wrong silently breaks vscode-mode undo while leaving vim green.
    fn reset_mode_after_history(&mut self);
}

/// Default discipline for an [`Editor`] that has not had a real discipline
/// installed: no FSM, always reports [`CoarseMode::Normal`]. Editors that
/// receive vim/vscode/etc. input install their discipline at construction
/// (e.g. `hjkl_vim::install_vim_discipline`).
///
/// [`Editor`]: crate::Editor
#[derive(Debug, Default)]
pub struct NoDiscipline;

impl DisciplineState for NoDiscipline {
    fn coarse_mode(&self) -> CoarseMode {
        CoarseMode::Normal
    }
    /// No FSM state to discard.
    fn reset_to_idle(&mut self) {}
    /// No FSM mode to reset.
    fn reset_mode_after_history(&mut self) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
