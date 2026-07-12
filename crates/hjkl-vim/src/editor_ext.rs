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

    /// Start/end `(row, col)` of the active char-wise Visual selection,
    /// positionally ordered. `None` when not in Visual mode.
    ///
    /// When [`hjkl_engine::editor::Settings::selection_exclusive`] is `false`
    /// (default, vim behaviour): both endpoints are **inclusive** — the cells
    /// at `start` and `end` are both selected.
    ///
    /// When it is `true` (VSCode bar-cursor behaviour): the range is
    /// **half-open** — `start` is included but `end` is the first cell that is
    /// NOT selected (the caret sits before it). If the selection is empty
    /// (`anchor == cursor`) `None` is returned so callers do not need to check
    /// for zero-length ranges.
    fn char_highlight(&self) -> Option<((usize, usize), (usize, usize))>;

    /// Return the half-open exclusive char-visual range `(start, end)` where
    /// `end` is the first cell NOT selected (the caret position). `None`
    /// when not in Visual mode or the selection is empty.
    ///
    /// Convenience accessor for the VSCode dispatcher; avoids duplicating
    /// the anchor/cursor ordering logic at the call site.
    fn visual_char_range_exclusive(&self) -> Option<((usize, usize), (usize, usize))>;
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

    fn char_highlight(&self) -> Option<((usize, usize), (usize, usize))> {
        if self.vim_mode() != VimMode::Visual {
            return None;
        }
        let anchor = self.vim.visual_anchor;
        let cursor = self.cursor();
        let (start, end) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        if self.settings().selection_exclusive {
            // Half-open: start..end (end excluded). Empty when start == end.
            if start == end {
                return None;
            }
            Some((start, end))
        } else {
            // Inclusive (vim default): both endpoints are selected.
            Some((start, end))
        }
    }

    fn visual_char_range_exclusive(&self) -> Option<((usize, usize), (usize, usize))> {
        if self.vim_mode() != VimMode::Visual {
            return None;
        }
        let anchor = self.vim.visual_anchor;
        let cursor = self.cursor();
        if anchor == cursor {
            return None;
        }
        let (start, end) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        Some((start, end))
    }
}
