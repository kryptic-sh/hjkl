//! Vim-mode engine.
//!
//! Implements a command grammar of the form
//!
//! ```text
//! Command := count? (operator count? (motion | text-object)
//!                   | motion
//!                   | insert-entry
//!                   | misc)
//! ```
//!
//! The parser is a small state machine driven by one `Input` at a time.
//! Motions and text objects produce a [`Range`] (with inclusive/exclusive
//! / linewise classification). A single [`Operator`] implementation
//! applies a range — so `dw`, `d$`, `daw`, and visual `d` all go through
//! the same code path.
//!
//! The most recent mutating command is stored in
//! [`VimState::last_change`] so `.` can replay it.
//!
//! # Roadmap
//!
//! Tracked in the original plan at
//! `~/.claude/plans/look-at-the-vim-curried-fern.md`. Phases still
//! outstanding — each one can land as an isolated PR.
//!
//! ## P3 — Registers & marks
//!
//! - TODO: `RegisterBank` indexed by char:
//!     - unnamed `""`, last-yank `"0`, small-delete `"-`
//!     - named `"a-"z` (uppercase `"A-"Z` appends instead of overwriting)
//!     - blackhole `"_`
//!     - system clipboard `"+` / `"*` (wire to `crate::clipboard::Clipboard`)
//!     - read-only `":`, `".`, `"%` — surface in `:reg` output
//! - TODO: route every yank / cut / paste through the bank. Parser needs
//!   a `"{reg}` prefix state that captures the target register before a
//!   count / operator.
//! - TODO: `m{a-z}` sets a mark in a `HashMap<char, (buffer_id, row, col)>`;
//!   `'x` jumps to the line (FirstNonBlank), `` `x `` to the exact cell.
//!   Uppercase marks are global across tabs; lowercase are per-buffer.
//! - TODO: `''` and `` `` `` jump to the last-jump position; `'[` `']`
//!   `'<` `'>` bound the last change / visual region.
//! - TODO: `:reg` and `:marks` ex commands.
//!
//! ## P4 — Macros
//!
//! - TODO: `q{a-z}` starts recording raw `Input`s into the register;
//!   next `q` stops.
//! - TODO: `@{a-z}` replays the register by re-feeding inputs through
//!   `step`. `@@` repeats the last macro. Nested macros need a sane
//!   depth cap (e.g. 100) to avoid runaway loops.
//! - TODO: ensure recording doesn't capture the initial `q{a-z}` itself.
//!
//! ## P6 — Polish (still outstanding)
//!
//! - TODO: indent operators `>` / `<` (with line + text-object targets).
//! - TODO: format operator `=` — map to whatever SQL formatter we wire
//!   up; for now stub that returns the range unchanged with a toast.
//! - TODO: case operators `gU` / `gu` / `g~` on a range (already have
//!   single-char `~`).
//! - TODO: screen motions `H` / `M` / `L` once we track the render
//!   viewport height inside Editor.
//! - TODO: scroll-to-cursor motions `zz` / `zt` / `zb`.
//!
//! ## Known substrate / divergence notes
//!
//! - TODO: insert-mode indent helpers — `Ctrl-t` / `Ctrl-d` (increase /
//!   decrease indent on current line) and `Ctrl-r <reg>` (paste from a
//!   register). `Ctrl-r` needs the `RegisterBank` from P3 to be useful.
//! - TODO: `/` and `?` search prompts still live in `the host/src/lib.rs`.
//!   The plan calls for moving them into the editor (so the editor owns
//!   `last_search_pattern` rather than the TUI loop). Safe to defer.

pub(crate) mod bridges;
pub(crate) mod command;
pub(crate) mod comment;
pub(crate) mod dot_repeat;
pub(crate) mod entry;
pub(crate) mod insert_bridges;
pub(crate) mod insert_ops;
pub(crate) mod jumplist;
pub(crate) mod linewise;
pub(crate) mod motion;
pub(crate) mod op_motion;
pub(crate) mod operator;
pub(crate) mod range_ops;
pub(crate) mod sneak;
pub(crate) mod state;
pub(crate) mod text_object;
pub(crate) mod text_object_ops;
pub(crate) mod visual;
pub(crate) mod visual_ops;

pub use hjkl_engine::abbrev::{Abbrev, AbbrevKind, AbbrevTrigger};
pub use hjkl_engine::search::SearchPrompt;
pub use hjkl_engine::types::{
    CHANGE_LIST_MAX, InsertDir, JUMPLIST_MAX, SEARCH_HISTORY_MAX, ScrollDir,
};
pub use hjkl_vim_types::{
    InsertEntry, InsertReason, InsertSession, LastChange, LastHorizontalMotion, LastVisual, Mode,
    Motion, Operator, Pending, RangeKind, TextObject,
};

// Flat intra-crate namespace: the FSM was one file until #267, and its
// helpers still call each other freely across these module lines.
pub(crate) use bridges::*;
pub(crate) use command::*;
pub(crate) use comment::*;
pub(crate) use dot_repeat::*;
pub(crate) use entry::*;
pub(crate) use insert_bridges::*;
pub(crate) use insert_ops::*;
pub(crate) use jumplist::*;
pub(crate) use linewise::*;
pub(crate) use motion::*;
pub(crate) use op_motion::*;
pub(crate) use operator::*;
pub(crate) use range_ops::*;
pub(crate) use sneak::*;
pub(crate) use state::*;
pub(crate) use text_object::*;
pub(crate) use text_object_ops::*;
pub(crate) use visual::*;
pub(crate) use visual_ops::*;

// The vim discipline's actual public API.
pub use motion::parse_motion;
pub use state::{MAX_COUNT, VimState};
// `matching_tag_pair` and the tag-matching group moved to `hjkl_engine::tag`
// (#265): a pure buffer query any discipline can use, not vim grammar.
pub use visual::{drop_blame_if_left_normal, op_is_change};

use hjkl_engine::Editor;

/// Install the vim discipline on `ed`, replacing whatever was there.
///
/// [`Editor::new`] leaves the discipline slot at
/// [`NoDiscipline`](hjkl_engine::NoDiscipline) — the engine cannot name a concrete
/// discipline. Every editor that should interpret keys as vim must be handed to
/// this once at construction; [`vim_editor`] does both in one call.
///
/// Dispatching vim input at an editor that skipped this panics on the first
/// state access (see `hjkl_vim::vim_state`) rather than silently behaving like
/// a keyboard-less buffer.
pub fn install<H: hjkl_engine::types::Host>(ed: &mut Editor<hjkl_buffer::Buffer, H>) {
    ed.set_discipline(Box::new(VimState::default()));
}
/// Build an [`Editor`] with the vim discipline already installed.
///
/// The vim-flavoured counterpart of [`Editor::new`], which yields an editor
/// with no discipline at all.
pub fn vim_editor<H: hjkl_engine::types::Host>(
    buffer: hjkl_buffer::Buffer,
    host: H,
    options: hjkl_engine::types::Options,
) -> Editor<hjkl_buffer::Buffer, H> {
    let mut ed = Editor::new(buffer, host, options);
    install(&mut ed);
    ed
}
