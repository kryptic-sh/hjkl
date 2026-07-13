//! Downcast helpers from the editor's type-erased discipline slot to
//! [`VimState`] (#265 G3 / #267).
//!
//! The engine stores the active keyboard discipline as
//! `Box<dyn DisciplineState>` and never names a concrete discipline type. The
//! vim FSM reaches its own state back through these two helpers, which are the
//! *only* place in this crate that performs the downcast.

use crate::vim::VimState;
use hjkl_engine::Editor;
use hjkl_engine::types::Host;

/// Borrow the vim FSM state out of `ed`'s discipline slot.
///
/// Panics if a different discipline is installed. That is a wiring bug — vim
/// input was dispatched at an `Editor` that never had the vim discipline
/// installed — not a runtime condition worth recovering from.
pub(crate) fn vim<H: Host>(ed: &Editor<hjkl_buffer::Buffer, H>) -> &VimState {
    ed.discipline()
        .as_any()
        .downcast_ref::<VimState>()
        .expect("vim discipline not installed on this Editor")
}

/// Mutable counterpart of [`vim`].
pub(crate) fn vim_mut<H: Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) -> &mut VimState {
    ed.discipline_mut()
        .as_any_mut()
        .downcast_mut::<VimState>()
        .expect("vim discipline not installed on this Editor")
}
