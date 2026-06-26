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
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
