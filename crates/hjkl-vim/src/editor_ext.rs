//! [`VimEditorExt`] — vim-discipline accessor methods on the engine
//! [`Editor`], migrated out of `hjkl-engine` (#267 / #265 G3).
//!
//! These read the vim FSM state (`Editor::vim`) to answer render/selection
//! questions. They belong to the vim *discipline*, not the mode-agnostic
//! engine core, so they live here — a blanket trait impl on
//! `Editor<Buffer, H>`. As `VimState` finishes relocating into this crate,
//! more of the engine's vim accessors move onto this trait; call sites pick
//! them up with `use hjkl_vim::VimEditorExt`.

use hjkl_engine::types::Host;
use hjkl_engine::{Editor, VimMode};

/// Vim-discipline read accessors layered onto every `Editor<Buffer, H>`.
///
/// Blanket-implemented below; bring it into scope with
/// `use hjkl_vim::VimEditorExt` to call these on an `Editor`.
pub trait VimEditorExt {
    /// VisualBlock selection bounds as `(top, bot, left, right)` — inclusive
    /// rows and inclusive columns, derived from the block anchor and the
    /// cursor's sticky column. Meaningful only while in VisualBlock mode;
    /// callers that need the "are we in block mode?" guard use
    /// [`VimEditorExt::block_highlight`] instead.
    fn visual_block_bounds(&self) -> (usize, usize, usize, usize);

    /// The VisualBlock highlight rectangle `(top, bot, left, right)`, or
    /// `None` when the editor is not in VisualBlock mode.
    fn block_highlight(&self) -> Option<(usize, usize, usize, usize)>;
}

impl<H: Host> VimEditorExt for Editor<hjkl_buffer::Buffer, H> {
    fn visual_block_bounds(&self) -> (usize, usize, usize, usize) {
        let (ar, ac) = self.vim.block_anchor;
        let (cr, _) = self.cursor();
        let cc = self.vim.block_vcol;
        (ar.min(cr), ar.max(cr), ac.min(cc), ac.max(cc))
    }

    fn block_highlight(&self) -> Option<(usize, usize, usize, usize)> {
        if self.vim_mode() != VimMode::VisualBlock {
            return None;
        }
        let (ar, ac) = self.vim.block_anchor;
        let cr = self.cursor().0;
        let cc = self.vim.block_vcol;
        Some((ar.min(cr), ar.max(cr), ac.min(cc), ac.max(cc)))
    }
}
