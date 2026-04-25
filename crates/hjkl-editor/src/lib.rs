//! # hjkl-editor
//!
//! Front door for the hjkl modal editor stack. Re-exports the working
//! parts of [`hjkl_engine`] and [`hjkl_buffer`] under a curated
//! namespace so downstream consumers (sqeel, buffr, hjkl binary) add
//! one dependency instead of three and don't need to know the
//! crate-split.
//!
//! Two layers ride alongside each other during the 0.0.x churn:
//!
//! - **Legacy surface** (today's runtime): the [`runtime`] module
//!   re-exports the existing [`Editor`], [`KeybindingMode`], [`VimMode`],
//!   [`Input`], [`Key`], [`SearchPrompt`], [`Registers`], [`Slot`], and
//!   [`LspIntent`]. This is what sqeel-tui consumes today.
//! - **Planned surface** (0.1.0 SPEC): the [`spec`] module re-exports
//!   the additive types from [`hjkl_engine::types`] —
//!   [`spec::Pos`], [`spec::Selection`], [`spec::SelectionSet`],
//!   [`spec::Edit`], [`spec::Mode`], [`spec::Style`], [`spec::Highlight`],
//!   [`spec::Options`], [`spec::Input`], [`spec::Host`], [`spec::EngineError`].
//!   Trait extraction will rewire the runtime onto these — once it
//!   ships, [`runtime`] becomes a thin compat layer.
//!
//! ## Usage
//!
//! ```no_run
//! use hjkl_editor::runtime::{Editor, KeybindingMode};
//!
//! let mut editor = Editor::new(KeybindingMode::Vim);
//! editor.set_content("hello world");
//! ```
//!
//! Buffer and rope helpers are re-exported at the [`buffer`] module,
//! mirroring the [`hjkl_buffer`] surface.
//!
//! [`Editor`]: hjkl_engine::Editor
//! [`KeybindingMode`]: hjkl_engine::KeybindingMode
//! [`VimMode`]: hjkl_engine::VimMode
//! [`Input`]: hjkl_engine::Input
//! [`Key`]: hjkl_engine::Key
//! [`SearchPrompt`]: hjkl_engine::SearchPrompt
//! [`Registers`]: hjkl_engine::Registers
//! [`Slot`]: hjkl_engine::Slot
//! [`LspIntent`]: hjkl_engine::LspIntent
#![forbid(unsafe_code)]

pub mod buffer {
    //! Re-export of [`hjkl_buffer`]'s public surface.

    pub use hjkl_buffer::{
        Buffer, BufferView, Edit, Fold, Gutter, MotionKind, Position, RowSpan, Selection, Sign,
        Span, StyleResolver, Viewport, Wrap,
    };
}

pub mod runtime {
    //! Legacy runtime surface — the working sqeel-vim port.
    //!
    //! These types drive editing today. The trait extraction lands a
    //! generic `Editor<B: Buffer, H: Host>` into [`crate::spec`] that
    //! eventually replaces this surface; pre-1.0 churn means the swap
    //! can land on a patch bump.

    pub use hjkl_engine::{
        Editor, Input, Key, KeybindingMode, LspIntent, Registers, SearchPrompt, Slot, VimMode,
    };
    pub mod ex {
        //! Ex command driver — `:s/pat/.../`, `:w`, `:q`, etc.
        pub use hjkl_engine::ex::*;
    }
}

pub mod spec {
    //! Planned 0.1.0 trait surface (per `crates/hjkl-engine/SPEC.md`).
    //!
    //! All types are additive — they coexist with [`crate::runtime`]
    //! during the churn phase. Trait impls are forthcoming; today the
    //! types support host-side prep (e.g., buffr-modal's `BuffrHost`).

    pub use hjkl_engine::types::{
        Attrs, BufferId, Color, CursorShape, Edit, EngineError, Highlight, HighlightKind, Host,
        Input, Mode, Modifiers, MouseEvent, MouseKind, Options, Pos, Selection, SelectionKind,
        SelectionSet, SpecialKey, Style, Viewport,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_editor_constructs() {
        let _ = runtime::Editor::new(runtime::KeybindingMode::Vim);
    }

    #[test]
    fn buffer_constructs() {
        let _ = buffer::Buffer::from_str("hello\nworld");
    }

    #[test]
    fn spec_options_default() {
        let opts = spec::Options::default();
        assert_eq!(opts.tabstop, 8);
    }

    #[test]
    fn spec_selection_set_default() {
        let set = spec::SelectionSet::default();
        assert_eq!(set.items.len(), 1);
    }
}
