//! Editor — the public sqeel-vim type, layered over `hjkl_buffer::Buffer`.
//!
//! This file owns the public Editor API — construction, content access,
//! mouse and goto helpers, the (buffer-level) undo stack, and insert-mode
//! session bookkeeping. All vim-specific keyboard handling lives in
//! [`vim`] and communicates with Editor through a small internal API
//! exposed via `pub(super)` fields and helper methods.

use crate::input::{Input, Key};
use crate::vim::{self, VimState};
use crate::{KeybindingMode, VimMode};
#[cfg(feature = "crossterm")]
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
#[cfg(feature = "ratatui")]
use ratatui::layout::Rect;
use std::sync::atomic::{AtomicU16, Ordering};

/// Convert a SPEC [`crate::types::Style`] to a [`ratatui::style::Style`].
///
/// Lossless within the styles each library represents. Lives behind the
/// `ratatui` feature so wasm / no_std consumers that opt out don't pay
/// for the dep. Use the engine-native [`crate::types::Style`] +
/// [`Editor::intern_engine_style`] surface from feature-disabled hosts.
#[cfg(feature = "ratatui")]
pub(crate) fn engine_style_to_ratatui(s: crate::types::Style) -> ratatui::style::Style {
    use crate::types::Attrs;
    use ratatui::style::{Color as RColor, Modifier as RMod, Style as RStyle};
    let mut out = RStyle::default();
    if let Some(c) = s.fg {
        out = out.fg(RColor::Rgb(c.0, c.1, c.2));
    }
    if let Some(c) = s.bg {
        out = out.bg(RColor::Rgb(c.0, c.1, c.2));
    }
    let mut m = RMod::empty();
    if s.attrs.contains(Attrs::BOLD) {
        m |= RMod::BOLD;
    }
    if s.attrs.contains(Attrs::ITALIC) {
        m |= RMod::ITALIC;
    }
    if s.attrs.contains(Attrs::UNDERLINE) {
        m |= RMod::UNDERLINED;
    }
    if s.attrs.contains(Attrs::REVERSE) {
        m |= RMod::REVERSED;
    }
    if s.attrs.contains(Attrs::DIM) {
        m |= RMod::DIM;
    }
    if s.attrs.contains(Attrs::STRIKE) {
        m |= RMod::CROSSED_OUT;
    }
    out.add_modifier(m)
}

/// Inverse of [`engine_style_to_ratatui`]. Lossy for ratatui colors
/// the engine doesn't model (Indexed, named ANSI) — flattens to
/// nearest RGB. Behind the `ratatui` feature.
#[cfg(feature = "ratatui")]
pub(crate) fn ratatui_style_to_engine(s: ratatui::style::Style) -> crate::types::Style {
    use crate::types::{Attrs, Color, Style};
    use ratatui::style::{Color as RColor, Modifier as RMod};
    fn c(rc: RColor) -> Color {
        match rc {
            RColor::Rgb(r, g, b) => Color(r, g, b),
            RColor::Black => Color(0, 0, 0),
            RColor::Red => Color(205, 49, 49),
            RColor::Green => Color(13, 188, 121),
            RColor::Yellow => Color(229, 229, 16),
            RColor::Blue => Color(36, 114, 200),
            RColor::Magenta => Color(188, 63, 188),
            RColor::Cyan => Color(17, 168, 205),
            RColor::Gray => Color(229, 229, 229),
            RColor::DarkGray => Color(102, 102, 102),
            RColor::LightRed => Color(241, 76, 76),
            RColor::LightGreen => Color(35, 209, 139),
            RColor::LightYellow => Color(245, 245, 67),
            RColor::LightBlue => Color(59, 142, 234),
            RColor::LightMagenta => Color(214, 112, 214),
            RColor::LightCyan => Color(41, 184, 219),
            RColor::White => Color(255, 255, 255),
            _ => Color(0, 0, 0),
        }
    }
    let mut attrs = Attrs::empty();
    if s.add_modifier.contains(RMod::BOLD) {
        attrs |= Attrs::BOLD;
    }
    if s.add_modifier.contains(RMod::ITALIC) {
        attrs |= Attrs::ITALIC;
    }
    if s.add_modifier.contains(RMod::UNDERLINED) {
        attrs |= Attrs::UNDERLINE;
    }
    if s.add_modifier.contains(RMod::REVERSED) {
        attrs |= Attrs::REVERSE;
    }
    if s.add_modifier.contains(RMod::DIM) {
        attrs |= Attrs::DIM;
    }
    if s.add_modifier.contains(RMod::CROSSED_OUT) {
        attrs |= Attrs::STRIKE;
    }
    Style {
        fg: s.fg.map(c),
        bg: s.bg.map(c),
        attrs,
    }
}

/// Map a [`hjkl_buffer::Edit`] to one or more SPEC
/// [`crate::types::Edit`] (`EditOp`) records.
///
/// Most buffer edits map to a single EditOp. Block ops
/// ([`hjkl_buffer::Edit::InsertBlock`] /
/// [`hjkl_buffer::Edit::DeleteBlockChunks`]) emit one EditOp per row
/// touched — they edit non-contiguous cells and a single
/// `range..range` can't represent the rectangle.
///
/// Returns an empty vec when the edit isn't representable (no buffer
/// variant currently fails this check).
fn edit_to_editops(edit: &hjkl_buffer::Edit) -> Vec<crate::types::Edit> {
    use crate::types::{Edit as Op, Pos};
    use hjkl_buffer::Edit as B;
    let to_pos = |p: hjkl_buffer::Position| Pos {
        line: p.row as u32,
        col: p.col as u32,
    };
    match edit {
        B::InsertChar { at, ch } => vec![Op {
            range: to_pos(*at)..to_pos(*at),
            replacement: ch.to_string(),
        }],
        B::InsertStr { at, text } => vec![Op {
            range: to_pos(*at)..to_pos(*at),
            replacement: text.clone(),
        }],
        B::DeleteRange { start, end, .. } => vec![Op {
            range: to_pos(*start)..to_pos(*end),
            replacement: String::new(),
        }],
        B::Replace { start, end, with } => vec![Op {
            range: to_pos(*start)..to_pos(*end),
            replacement: with.clone(),
        }],
        B::JoinLines {
            row,
            count,
            with_space,
        } => {
            // Joining `count` rows after `row` collapses
            // [(row+1, 0) .. (row+count, EOL)] into the joined
            // sentinel. The replacement is either an empty string
            // (gJ) or " " between segments (J).
            let start = Pos {
                line: *row as u32 + 1,
                col: 0,
            };
            let end = Pos {
                line: (*row + *count) as u32,
                col: u32::MAX, // covers to EOL of the last source row
            };
            vec![Op {
                range: start..end,
                replacement: if *with_space {
                    " ".into()
                } else {
                    String::new()
                },
            }]
        }
        B::SplitLines {
            row,
            cols,
            inserted_space: _,
        } => {
            // SplitLines reverses a JoinLines: insert a `\n`
            // (and optional dropped space) at each col on `row`.
            cols.iter()
                .map(|c| {
                    let p = Pos {
                        line: *row as u32,
                        col: *c as u32,
                    };
                    Op {
                        range: p..p,
                        replacement: "\n".into(),
                    }
                })
                .collect()
        }
        B::InsertBlock { at, chunks } => {
            // One EditOp per row in the block — non-contiguous edits.
            chunks
                .iter()
                .enumerate()
                .map(|(i, chunk)| {
                    let p = Pos {
                        line: at.row as u32 + i as u32,
                        col: at.col as u32,
                    };
                    Op {
                        range: p..p,
                        replacement: chunk.clone(),
                    }
                })
                .collect()
        }
        B::DeleteBlockChunks { at, widths } => {
            // One EditOp per row, deleting `widths[i]` chars at
            // `(at.row + i, at.col)`.
            widths
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    let start = Pos {
                        line: at.row as u32 + i as u32,
                        col: at.col as u32,
                    };
                    let end = Pos {
                        line: at.row as u32 + i as u32,
                        col: at.col as u32 + *w as u32,
                    };
                    Op {
                        range: start..end,
                        replacement: String::new(),
                    }
                })
                .collect()
        }
    }
}

/// Where the cursor should land in the viewport after a `z`-family
/// scroll (`zz` / `zt` / `zb`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CursorScrollTarget {
    Center,
    Top,
    Bottom,
}

pub struct Editor<'a> {
    pub keybinding_mode: KeybindingMode,
    /// Reserved for the lifetime parameter — Editor used to wrap a
    /// `TextArea<'a>` whose lifetime came from this slot. Phase 7f
    /// ripped the field but the lifetime stays so downstream
    /// `Editor<'a>` consumers don't have to churn.
    _marker: std::marker::PhantomData<&'a ()>,
    /// Set when the user yanks/cuts; caller drains this to write to OS clipboard.
    pub last_yank: Option<String>,
    /// All vim-specific state (mode, pending operator, count, dot-repeat, ...).
    /// Internal — exposed via Editor accessor methods
    /// ([`Editor::buffer_mark`], [`Editor::last_jump_back`],
    /// [`Editor::last_edit_pos`], [`Editor::take_lsp_intent`], …).
    pub(crate) vim: VimState,
    /// Undo history: each entry is (lines, cursor) before the edit.
    /// Internal — managed by [`Editor::push_undo`] / [`Editor::restore`]
    /// / [`Editor::pop_last_undo`].
    pub(crate) undo_stack: Vec<(Vec<String>, (usize, usize))>,
    /// Redo history: entries pushed when undoing.
    pub(super) redo_stack: Vec<(Vec<String>, (usize, usize))>,
    /// Set whenever the buffer content changes; cleared by `take_dirty`.
    pub(super) content_dirty: bool,
    /// Cached snapshot of `lines().join("\n") + "\n"` wrapped in an Arc
    /// so repeated `content_arc()` calls within the same un-mutated
    /// window are free (ref-count bump instead of a full-buffer join).
    /// Invalidated by every [`mark_content_dirty`] call.
    pub(super) cached_content: Option<std::sync::Arc<String>>,
    /// Last rendered viewport height (text rows only, no chrome). Written
    /// by the draw path via [`set_viewport_height`] so the scroll helpers
    /// can clamp the cursor to stay visible without plumbing the height
    /// through every call.
    pub(super) viewport_height: AtomicU16,
    /// Pending LSP intent set by a normal-mode chord (e.g. `gd` for
    /// goto-definition). The host app drains this each step and fires
    /// the matching request against its own LSP client.
    pub(super) pending_lsp: Option<LspIntent>,
    /// Mirror buffer for the in-flight migration off tui-textarea.
    /// Phase 7a: content syncs on every `set_content` so the rest of
    /// the engine can start reading from / writing to it in
    /// follow-up commits without behaviour changing today.
    pub(super) buffer: hjkl_buffer::Buffer,
    /// Style intern table for the migration buffer's opaque
    /// `Span::style` ids. Phase 7d-ii-a wiring — `apply_window_spans`
    /// produces `(start, end, Style)` tuples for the textarea; we
    /// translate those to `hjkl_buffer::Span` by interning the
    /// `Style` here and storing the table index. The render path's
    /// `StyleResolver` looks the style back up by id.
    ///
    /// Behind the `ratatui` feature; non-ratatui hosts use the
    /// engine-native [`crate::types::Style`] surface via
    /// [`Editor::intern_engine_style`] (which lives on a parallel
    /// engine-side table when ratatui is off).
    #[cfg(feature = "ratatui")]
    pub(super) style_table: Vec<ratatui::style::Style>,
    /// Engine-native style intern table. Used directly by
    /// [`Editor::intern_engine_style`] when the `ratatui` feature is
    /// off; when it's on, the table is derived from `style_table` via
    /// [`ratatui_style_to_engine`] / [`engine_style_to_ratatui`].
    #[cfg(not(feature = "ratatui"))]
    pub(super) engine_style_table: Vec<crate::types::Style>,
    /// Vim-style register bank — `"`, `"0`–`"9`, `"a`–`"z`. Sources
    /// every `p` / `P` via the active selector (default unnamed).
    /// Internal — read via [`Editor::registers`]; mutated by yank /
    /// delete / paste FSM paths and by [`Editor::seed_yank`].
    pub(crate) registers: crate::registers::Registers,
    /// Per-row syntax styling, kept here so the host can do
    /// incremental window updates (see `apply_window_spans` in
    /// the host). Same `(start_byte, end_byte, Style)` tuple shape
    /// the textarea used to host. The Buffer-side opaque-id spans are
    /// derived from this on every install. Behind the `ratatui`
    /// feature.
    #[cfg(feature = "ratatui")]
    pub styled_spans: Vec<Vec<(usize, usize, ratatui::style::Style)>>,
    /// Per-editor settings tweakable via `:set`. Exposed by reference
    /// so handlers (indent, search) read the live value rather than a
    /// snapshot taken at startup. Read via [`Editor::settings`];
    /// mutate via [`Editor::settings_mut`].
    pub(crate) settings: Settings,
    /// Vim's uppercase / "file" marks. Survive `set_content` calls so
    /// they persist across tab swaps within the same Editor — the
    /// closest the host can get to vim's per-file marks without
    /// host-side persistence. Lowercase marks stay buffer-local on
    /// `vim.marks`. Ex commands iterate via [`Editor::file_marks`];
    /// snapshot persistence goes through
    /// [`Editor::take_snapshot`] / [`Editor::restore_snapshot`].
    pub(crate) file_marks: std::collections::HashMap<char, (usize, usize)>,
    /// Block ranges (`(start_row, end_row)` inclusive) the host has
    /// extracted from a syntax tree. `:foldsyntax` reads these to
    /// populate folds. The host refreshes them on every re-parse via
    /// [`Editor::set_syntax_fold_ranges`]; ex commands read them via
    /// [`Editor::syntax_fold_ranges`].
    pub(crate) syntax_fold_ranges: Vec<(usize, usize)>,
    /// Pending edit log drained by [`Editor::take_changes`]. Each entry
    /// is a SPEC [`crate::types::Edit`] mapped from the underlying
    /// `hjkl_buffer::Edit` operation. Compound ops (JoinLines,
    /// SplitLines, InsertBlock, DeleteBlockChunks) emit a single
    /// best-effort EditOp covering the touched range; hosts wanting
    /// per-cell deltas should diff their own snapshot of `lines()`.
    /// Sealed at 0.1.0 trait extraction.
    /// Drained by [`Editor::take_changes`].
    pub(crate) change_log: Vec<crate::types::Edit>,
    /// Vim's "sticky column" (curswant). `None` before the first
    /// motion — the next vertical motion bootstraps from the live
    /// cursor column. Horizontal motions refresh this to the new
    /// column; vertical motions read it back so bouncing through a
    /// shorter row doesn't drag the cursor to col 0. Hoisted out of
    /// `hjkl_buffer::Buffer` (and `VimState`) in 0.0.28 — Editor is
    /// the single owner now. Buffer motion methods that need it
    /// take a `&mut Option<usize>` parameter.
    pub(crate) sticky_col: Option<usize>,
}

/// Vim-style options surfaced by `:set`. New fields land here as
/// individual ex commands gain `:set` plumbing.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Spaces per shift step for `>>` / `<<` / `Ctrl-T` / `Ctrl-D`.
    pub shiftwidth: usize,
    /// Visual width of a `\t` character. Stored for future render
    /// hookup; not yet consumed by the buffer renderer.
    pub tabstop: usize,
    /// When true, `/` / `?` patterns and `:s/.../.../` ignore case
    /// without an explicit `i` flag.
    pub ignore_case: bool,
    /// When true *and* `ignore_case` is true, an uppercase letter in
    /// the pattern flips that search back to case-sensitive. Matches
    /// vim's `:set smartcase`. Default `false`.
    pub smartcase: bool,
    /// Wrap searches past buffer ends. Matches vim's `:set wrapscan`.
    /// Default `true`.
    pub wrapscan: bool,
    /// Wrap column for `gq{motion}` text reflow. Vim's default is 79.
    pub textwidth: usize,
    /// When `true`, the Tab key in insert mode inserts `tabstop` spaces
    /// instead of a literal `\t`. Matches vim's `:set expandtab`.
    /// Default `false`.
    pub expandtab: bool,
    /// Soft-wrap mode the renderer + scroll math + `gj` / `gk` use.
    /// Default is [`hjkl_buffer::Wrap::None`] — long lines extend
    /// past the right edge and `top_col` clips the left side.
    /// `:set wrap` flips to char-break wrap; `:set linebreak` flips
    /// to word-break wrap; `:set nowrap` resets.
    pub wrap: hjkl_buffer::Wrap,
    /// When true, the engine drops every edit before it touches the
    /// buffer — undo, dirty flag, and change log all stay clean.
    /// Matches vim's `:set readonly` / `:set ro`. Default `false`.
    pub readonly: bool,
    /// When `true`, pressing Enter in insert mode copies the leading
    /// whitespace of the current line onto the new line. Matches vim's
    /// `:set autoindent`. Default `true` (vim parity).
    pub autoindent: bool,
    /// Cap on undo-stack length. Older entries are pruned past this
    /// bound. `0` means unlimited. Matches vim's `:set undolevels`.
    /// Default `1000`.
    pub undo_levels: u32,
    /// When `true`, cursor motions inside insert mode break the
    /// current undo group (so a single `u` only reverses the run of
    /// keystrokes that preceded the motion). Default `true`.
    /// Currently a no-op — engine doesn't yet break the undo group
    /// on insert-mode motions; field is wired through `:set
    /// undobreak` for forward compatibility.
    pub undo_break_on_motion: bool,
    /// Vim-flavoured "what counts as a word" character class.
    /// Comma-separated tokens: `@` = `is_alphabetic()`, `_` = literal
    /// `_`, `48-57` = decimal char range, bare integer = single char
    /// code, single ASCII punctuation = literal. Default
    /// `"@,48-57,_,192-255"` matches vim.
    pub iskeyword: String,
    /// Multi-key sequence timeout (e.g. `gg`, `dd`). When the user
    /// pauses longer than this between keys, any pending prefix is
    /// abandoned and the next key starts a fresh sequence. Matches
    /// vim's `:set timeoutlen` / `:set tm` (millis). Default 1000ms.
    pub timeout_len: core::time::Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shiftwidth: 2,
            tabstop: 8,
            ignore_case: false,
            smartcase: false,
            wrapscan: true,
            textwidth: 79,
            expandtab: false,
            wrap: hjkl_buffer::Wrap::None,
            readonly: false,
            autoindent: true,
            undo_levels: 1000,
            undo_break_on_motion: true,
            iskeyword: "@,48-57,_,192-255".to_string(),
            timeout_len: core::time::Duration::from_millis(1000),
        }
    }
}

/// Host-observable LSP requests triggered by editor bindings. The
/// hjkl-engine crate doesn't talk to an LSP itself — it just raises an
/// intent that the TUI layer picks up and routes to `sqls`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspIntent {
    /// `gd` — textDocument/definition at the cursor.
    GotoDefinition,
}

impl<'a> Editor<'a> {
    /// Update the active `iskeyword` spec for word motions
    /// (`w`/`b`/`e`/`ge` and engine-side `*`/`#` pickup). 0.0.28
    /// hoisted iskeyword storage out of `Buffer` — `Editor` is the
    /// single owner now. Equivalent to assigning
    /// `settings_mut().iskeyword` directly; the dedicated setter is
    /// retained for source-compatibility with 0.0.27 callers.
    pub fn set_iskeyword(&mut self, spec: impl Into<String>) {
        self.settings.iskeyword = spec.into();
    }

    pub fn new(keybinding_mode: KeybindingMode) -> Self {
        Self {
            _marker: std::marker::PhantomData,
            keybinding_mode,
            last_yank: None,
            vim: VimState::default(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            content_dirty: false,
            cached_content: None,
            viewport_height: AtomicU16::new(0),
            pending_lsp: None,
            buffer: hjkl_buffer::Buffer::new(),
            #[cfg(feature = "ratatui")]
            style_table: Vec::new(),
            #[cfg(not(feature = "ratatui"))]
            engine_style_table: Vec::new(),
            registers: crate::registers::Registers::default(),
            #[cfg(feature = "ratatui")]
            styled_spans: Vec::new(),
            settings: Settings::default(),
            file_marks: std::collections::HashMap::new(),
            syntax_fold_ranges: Vec::new(),
            change_log: Vec::new(),
            sticky_col: None,
        }
    }

    /// Vim's sticky column (curswant). `None` before the first motion;
    /// hosts shouldn't normally need to read this directly — it's
    /// surfaced for migration off `Buffer::sticky_col` and for
    /// snapshot tests.
    pub fn sticky_col(&self) -> Option<usize> {
        self.sticky_col
    }

    /// Replace the sticky column. Hosts should rarely touch this —
    /// motion code maintains it through the standard horizontal /
    /// vertical motion paths.
    pub fn set_sticky_col(&mut self, col: Option<usize>) {
        self.sticky_col = col;
    }

    /// Host hook: replace the cached syntax-derived block ranges that
    /// `:foldsyntax` consumes. the host calls this on every re-parse;
    /// the cost is just a `Vec` swap.
    /// Look up a buffer-local lowercase mark (`'a`–`'z`). Returns
    /// `(row, col)` if set; `None` otherwise. Uppercase / file marks
    /// live separately — read those via [`Editor::file_marks`].
    pub fn buffer_mark(&self, c: char) -> Option<(usize, usize)> {
        self.vim.marks.get(&c).copied()
    }

    /// Discard the most recent undo entry. Used by ex commands that
    /// pre-emptively pushed an undo state (`:s`, `:r`) but ended up
    /// matching nothing — popping prevents a no-op undo step from
    /// polluting the user's history.
    ///
    /// Returns `true` if an entry was discarded.
    pub fn pop_last_undo(&mut self) -> bool {
        self.undo_stack.pop().is_some()
    }

    /// Read all buffer-local marks set this session.
    pub fn buffer_marks(&self) -> impl Iterator<Item = (char, (usize, usize))> + '_ {
        self.vim.marks.iter().map(|(c, p)| (*c, *p))
    }

    /// Position the cursor was at when the user last jumped via
    /// `<C-o>` / `g;` / similar. `None` before any jump.
    pub fn last_jump_back(&self) -> Option<(usize, usize)> {
        self.vim.jump_back.last().copied()
    }

    /// Position of the last edit (where `.` would replay). `None` if
    /// no edit has happened yet in this session.
    pub fn last_edit_pos(&self) -> Option<(usize, usize)> {
        self.vim.last_edit_pos
    }

    /// Read-only view of the file-marks table — uppercase / "file"
    /// marks (`'A`–`'Z`) the host has set this session. Returns an
    /// iterator of `(mark_char, (row, col))` pairs.
    ///
    /// Mutate via the FSM (`m{A-Z}` keystroke) or via
    /// [`Editor::restore_snapshot`].
    pub fn file_marks(&self) -> impl Iterator<Item = (char, (usize, usize))> + '_ {
        self.file_marks.iter().map(|(c, p)| (*c, *p))
    }

    /// Read-only view of the cached syntax-derived block ranges that
    /// `:foldsyntax` consumes. Returns the slice the host last
    /// installed via [`Editor::set_syntax_fold_ranges`]; empty when
    /// no syntax integration is active.
    pub fn syntax_fold_ranges(&self) -> &[(usize, usize)] {
        &self.syntax_fold_ranges
    }

    pub fn set_syntax_fold_ranges(&mut self, ranges: Vec<(usize, usize)>) {
        self.syntax_fold_ranges = ranges;
    }

    /// Live settings (read-only). `:set` mutates these via
    /// [`Editor::settings_mut`].
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Live settings (mutable). `:set` flows through here to mutate
    /// shiftwidth / tabstop / textwidth / ignore_case / wrap. Hosts
    /// configuring at startup typically construct a [`Settings`]
    /// snapshot and overwrite via `*editor.settings_mut() = …`.
    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    /// Install styled syntax spans into both the host-visible cache
    /// (`styled_spans`) and the buffer's opaque-id span table. Drops
    /// zero-width runs and clamps `end` to the line's char length so
    /// the buffer cache doesn't see runaway ranges. Replaces the
    /// previous `set_syntax_spans` + `sync_buffer_spans_from_textarea`
    /// round-trip. Behind the `ratatui` feature; non-ratatui hosts use
    /// [`Editor::install_engine_syntax_spans`] (engine-native `Style`).
    #[cfg(feature = "ratatui")]
    pub fn install_syntax_spans(&mut self, spans: Vec<Vec<(usize, usize, ratatui::style::Style)>>) {
        let line_byte_lens: Vec<usize> = self.buffer.lines().iter().map(|l| l.len()).collect();
        let mut by_row: Vec<Vec<hjkl_buffer::Span>> = Vec::with_capacity(spans.len());
        for (row, row_spans) in spans.iter().enumerate() {
            let line_len = line_byte_lens.get(row).copied().unwrap_or(0);
            let mut translated = Vec::with_capacity(row_spans.len());
            for (start, end, style) in row_spans {
                let end_clamped = (*end).min(line_len);
                if end_clamped <= *start {
                    continue;
                }
                let id = self.intern_style(*style);
                translated.push(hjkl_buffer::Span::new(*start, end_clamped, id));
            }
            by_row.push(translated);
        }
        self.buffer.set_spans(by_row);
        self.styled_spans = spans;
    }

    /// Snapshot of the unnamed register (the default `p` / `P` source).
    pub fn yank(&self) -> &str {
        &self.registers.unnamed.text
    }

    /// Borrow the full register bank — `"`, `"0`–`"9`, `"a`–`"z`.
    pub fn registers(&self) -> &crate::registers::Registers {
        &self.registers
    }

    /// Host hook: load the OS clipboard's contents into the `"+` / `"*`
    /// register slot. the host calls this before letting vim consume a
    /// paste so `"*p` / `"+p` reflect the live clipboard rather than a
    /// stale snapshot from the last yank.
    pub fn sync_clipboard_register(&mut self, text: String, linewise: bool) {
        self.registers.set_clipboard(text, linewise);
    }

    /// True when the user's pending register selector is `+` or `*`.
    /// the host peeks this so it can refresh `sync_clipboard_register`
    /// only when a clipboard read is actually about to happen.
    pub fn pending_register_is_clipboard(&self) -> bool {
        matches!(self.vim.pending_register, Some('+') | Some('*'))
    }

    /// Replace the unnamed register without touching any other slot.
    /// For host-driven imports (e.g. system clipboard); operator
    /// code uses [`record_yank`] / [`record_delete`].
    pub fn set_yank(&mut self, text: impl Into<String>) {
        let text = text.into();
        let linewise = self.vim.yank_linewise;
        self.registers.unnamed = crate::registers::Slot { text, linewise };
    }

    /// Record a yank into `"` and `"0`, plus the named target if the
    /// user prefixed `"reg`. Updates `vim.yank_linewise` for the
    /// paste path.
    pub(crate) fn record_yank(&mut self, text: String, linewise: bool) {
        self.vim.yank_linewise = linewise;
        let target = self.vim.pending_register.take();
        self.registers.record_yank(text, linewise, target);
    }

    /// Direct write to a named register slot — bypasses the unnamed
    /// `"` and `"0` updates that `record_yank` does. Used by the
    /// macro recorder so finishing a `q{reg}` recording doesn't
    /// pollute the user's last yank.
    pub(crate) fn set_named_register_text(&mut self, reg: char, text: String) {
        if let Some(slot) = match reg {
            'a'..='z' => Some(&mut self.registers.named[(reg as u8 - b'a') as usize]),
            'A'..='Z' => {
                Some(&mut self.registers.named[(reg.to_ascii_lowercase() as u8 - b'a') as usize])
            }
            _ => None,
        } {
            slot.text = text;
            slot.linewise = false;
        }
    }

    /// Record a delete / change into `"` and the `"1`–`"9` ring.
    /// Honours the active named-register prefix.
    pub(crate) fn record_delete(&mut self, text: String, linewise: bool) {
        self.vim.yank_linewise = linewise;
        let target = self.vim.pending_register.take();
        self.registers.record_delete(text, linewise, target);
    }

    /// Install styled syntax spans using the engine-native
    /// [`crate::types::Style`]. The non-ratatui equivalent of
    /// [`Editor::install_syntax_spans`]. Always available, regardless
    /// of the `ratatui` feature.
    pub fn install_engine_syntax_spans(
        &mut self,
        spans: Vec<Vec<(usize, usize, crate::types::Style)>>,
    ) {
        let line_byte_lens: Vec<usize> = self.buffer.lines().iter().map(|l| l.len()).collect();
        let mut by_row: Vec<Vec<hjkl_buffer::Span>> = Vec::with_capacity(spans.len());
        #[cfg(feature = "ratatui")]
        let mut ratatui_spans: Vec<Vec<(usize, usize, ratatui::style::Style)>> =
            Vec::with_capacity(spans.len());
        for (row, row_spans) in spans.iter().enumerate() {
            let line_len = line_byte_lens.get(row).copied().unwrap_or(0);
            let mut translated = Vec::with_capacity(row_spans.len());
            #[cfg(feature = "ratatui")]
            let mut translated_r = Vec::with_capacity(row_spans.len());
            for (start, end, style) in row_spans {
                let end_clamped = (*end).min(line_len);
                if end_clamped <= *start {
                    continue;
                }
                let id = self.intern_engine_style(*style);
                translated.push(hjkl_buffer::Span::new(*start, end_clamped, id));
                #[cfg(feature = "ratatui")]
                translated_r.push((*start, end_clamped, engine_style_to_ratatui(*style)));
            }
            by_row.push(translated);
            #[cfg(feature = "ratatui")]
            ratatui_spans.push(translated_r);
        }
        self.buffer.set_spans(by_row);
        #[cfg(feature = "ratatui")]
        {
            self.styled_spans = ratatui_spans;
        }
    }

    /// Intern a `ratatui::style::Style` and return the opaque id used
    /// in `hjkl_buffer::Span::style`. The render-side `StyleResolver`
    /// closure (built by [`Editor::style_resolver`]) uses the id to
    /// look up the style back. Linear-scan dedup — the table grows
    /// only as new tree-sitter token kinds appear, so it stays tiny.
    /// Behind the `ratatui` feature.
    #[cfg(feature = "ratatui")]
    pub fn intern_style(&mut self, style: ratatui::style::Style) -> u32 {
        if let Some(idx) = self.style_table.iter().position(|s| *s == style) {
            return idx as u32;
        }
        self.style_table.push(style);
        (self.style_table.len() - 1) as u32
    }

    /// Read-only view of the style table — id `i` → `style_table[i]`.
    /// The render path passes a closure backed by this slice as the
    /// `StyleResolver` for `BufferView`. Behind the `ratatui` feature.
    #[cfg(feature = "ratatui")]
    pub fn style_table(&self) -> &[ratatui::style::Style] {
        &self.style_table
    }

    /// Intern a SPEC [`crate::types::Style`] and return its opaque id.
    /// With the `ratatui` feature on, the id matches the one
    /// [`intern_style`] would return for the equivalent
    /// `ratatui::Style` (both share the underlying table). With it off,
    /// the engine keeps a parallel `crate::types::Style`-keyed table
    /// — ids are still stable per-editor.
    ///
    /// Hosts that don't depend on ratatui (buffr, future GUI shells)
    /// reach this method to populate the table during syntax span
    /// installation.
    pub fn intern_engine_style(&mut self, style: crate::types::Style) -> u32 {
        #[cfg(feature = "ratatui")]
        {
            let r = engine_style_to_ratatui(style);
            self.intern_style(r)
        }
        #[cfg(not(feature = "ratatui"))]
        {
            if let Some(idx) = self.engine_style_table.iter().position(|s| *s == style) {
                return idx as u32;
            }
            self.engine_style_table.push(style);
            (self.engine_style_table.len() - 1) as u32
        }
    }

    /// Look up an interned style by id and return it as a SPEC
    /// [`crate::types::Style`]. Returns `None` for ids past the end
    /// of the table.
    pub fn engine_style_at(&self, id: u32) -> Option<crate::types::Style> {
        #[cfg(feature = "ratatui")]
        {
            let r = self.style_table.get(id as usize).copied()?;
            Some(ratatui_style_to_engine(r))
        }
        #[cfg(not(feature = "ratatui"))]
        {
            self.engine_style_table.get(id as usize).copied()
        }
    }

    /// Borrow the migration buffer. Host renders through this via
    /// `hjkl_buffer::BufferView`.
    pub fn buffer(&self) -> &hjkl_buffer::Buffer {
        &self.buffer
    }

    pub fn buffer_mut(&mut self) -> &mut hjkl_buffer::Buffer {
        &mut self.buffer
    }

    /// Historical reverse-sync hook from when the textarea mirrored
    /// the buffer. Now that Buffer is the cursor authority this is a
    /// no-op; call sites can remain in place during the migration.
    pub(crate) fn push_buffer_cursor_to_textarea(&mut self) {}

    /// Force the buffer viewport's top row without touching the
    /// cursor. Used by tests that simulate a scroll without the
    /// SCROLLOFF cursor adjustment that `scroll_down` / `scroll_up`
    /// apply. Note: does not touch the textarea — the migration
    /// buffer's viewport is what `BufferView` renders from, and the
    /// textarea's own scroll path would clamp the cursor into its
    /// (often-zero) visible window.
    pub fn set_viewport_top(&mut self, row: usize) {
        let last = self.buffer.row_count().saturating_sub(1);
        let target = row.min(last);
        self.buffer.viewport_mut().top_row = target;
    }

    /// Set the cursor to `(row, col)`, clamped to the buffer's
    /// content. Hosts use this for goto-line, jump-to-mark, and
    /// programmatic cursor placement.
    pub fn jump_cursor(&mut self, row: usize, col: usize) {
        self.buffer.set_cursor(hjkl_buffer::Position::new(row, col));
    }

    /// `(row, col)` cursor read sourced from the migration buffer.
    /// Equivalent to `self.textarea.cursor()` when the two are in
    /// sync — which is the steady state during Phase 7f because
    /// every step opens with `sync_buffer_content_from_textarea` and
    /// every ported motion pushes the result back. Prefer this over
    /// `self.textarea.cursor()` so call sites keep working unchanged
    /// once the textarea field is ripped.
    pub fn cursor(&self) -> (usize, usize) {
        let pos = self.buffer.cursor();
        (pos.row, pos.col)
    }

    /// Drain any pending LSP intent raised by the last key. Returns
    /// `None` when no intent is armed.
    pub fn take_lsp_intent(&mut self) -> Option<LspIntent> {
        self.pending_lsp.take()
    }

    /// Refresh the buffer's host-side state — viewport height.
    /// Called from the per-step boilerplate; was the textarea →
    /// buffer mirror before Phase 7f put Buffer in charge. 0.0.28
    /// hoisted sticky_col out of `Buffer` so this no longer touches
    /// it.
    pub(crate) fn sync_buffer_from_textarea(&mut self) {
        let height = self.viewport_height_value();
        self.buffer.viewport_mut().height = height;
    }

    /// Was the full textarea → buffer content sync. Buffer is the
    /// content authority now; this remains as a no-op so the per-step
    /// call sites don't have to be ripped in the same patch.
    pub(crate) fn sync_buffer_content_from_textarea(&mut self) {
        self.sync_buffer_from_textarea();
    }

    /// Push a `(row, col)` onto the back-jumplist so `Ctrl-o` returns
    /// to it later. Used by host-driven jumps (e.g. `gd`) that move
    /// the cursor without going through the vim engine's motion
    /// machinery, where push_jump fires automatically.
    pub fn record_jump(&mut self, pos: (usize, usize)) {
        const JUMPLIST_MAX: usize = 100;
        self.vim.jump_back.push(pos);
        if self.vim.jump_back.len() > JUMPLIST_MAX {
            self.vim.jump_back.remove(0);
        }
        self.vim.jump_fwd.clear();
    }

    /// Host apps call this each draw with the current text area height so
    /// scroll helpers can clamp the cursor without recomputing layout.
    pub fn set_viewport_height(&self, height: u16) {
        self.viewport_height.store(height, Ordering::Relaxed);
    }

    /// Last height published by `set_viewport_height` (in rows).
    pub fn viewport_height_value(&self) -> u16 {
        self.viewport_height.load(Ordering::Relaxed)
    }

    /// Apply `edit` against the buffer and return the inverse so the
    /// host can push it onto an undo stack. Side effects: dirty
    /// flag, change-list ring, mark / jump-list shifts, change_log
    /// append, fold invalidation around the touched rows.
    ///
    /// The primary edit funnel — both FSM operators and ex commands
    /// route mutations through here so the side effects fire
    /// uniformly.
    pub fn mutate_edit(&mut self, edit: hjkl_buffer::Edit) -> hjkl_buffer::Edit {
        // `:set readonly` short-circuits every mutation funnel: no
        // buffer change, no dirty flag, no undo entry, no change-log
        // emission. We swallow the requested `edit` and hand back a
        // self-inverse no-op (`InsertStr` of an empty string at the
        // current cursor) so callers that push the return value onto
        // an undo stack still get a structurally valid round trip.
        if self.settings.readonly {
            let _ = edit;
            return hjkl_buffer::Edit::InsertStr {
                at: self.buffer.cursor(),
                text: String::new(),
            };
        }
        let pre_row = self.buffer.cursor().row;
        let pre_rows = self.buffer.row_count();
        // Map the underlying buffer edit to a SPEC EditOp for
        // change-log emission before consuming it. Coarse — see
        // change_log field doc on the struct.
        self.change_log.extend(edit_to_editops(&edit));
        let inverse = self.buffer.apply_edit(edit);
        let pos = self.buffer.cursor();
        // Drop any folds the edit's range overlapped — vim opens the
        // surrounding fold automatically when you edit inside it. The
        // approximation here invalidates folds covering either the
        // pre-edit cursor row or the post-edit cursor row, which
        // catches the common single-line / multi-line edit shapes.
        let lo = pre_row.min(pos.row);
        let hi = pre_row.max(pos.row);
        self.buffer.invalidate_folds_in_range(lo, hi);
        self.vim.last_edit_pos = Some((pos.row, pos.col));
        // Append to the change-list ring (skip when the cursor sits on
        // the same cell as the last entry — back-to-back keystrokes on
        // one column shouldn't pollute the ring). A new edit while
        // walking the ring trims the forward half, vim style.
        let entry = (pos.row, pos.col);
        if self.vim.change_list.last() != Some(&entry) {
            if let Some(idx) = self.vim.change_list_cursor.take() {
                self.vim.change_list.truncate(idx + 1);
            }
            self.vim.change_list.push(entry);
            let len = self.vim.change_list.len();
            if len > crate::vim::CHANGE_LIST_MAX {
                self.vim
                    .change_list
                    .drain(0..len - crate::vim::CHANGE_LIST_MAX);
            }
        }
        self.vim.change_list_cursor = None;
        // Shift / drop marks + jump-list entries to track the row
        // delta the edit produced. Without this, every line-changing
        // edit silently invalidates `'a`-style positions.
        let post_rows = self.buffer.row_count();
        let delta = post_rows as isize - pre_rows as isize;
        if delta != 0 {
            self.shift_marks_after_edit(pre_row, delta);
        }
        self.push_buffer_content_to_textarea();
        self.mark_content_dirty();
        inverse
    }

    /// Migrate user marks + jumplist entries when an edit at row
    /// `edit_start` changes the buffer's row count by `delta` (positive
    /// for inserts, negative for deletes). Marks tied to a deleted row
    /// are dropped; marks past the affected band shift by `delta`.
    fn shift_marks_after_edit(&mut self, edit_start: usize, delta: isize) {
        if delta == 0 {
            return;
        }
        // Deleted-row band (only meaningful for delta < 0). Inclusive
        // start, exclusive end.
        let drop_end = if delta < 0 {
            edit_start.saturating_add((-delta) as usize)
        } else {
            edit_start
        };
        let shift_threshold = drop_end.max(edit_start.saturating_add(1));

        let mut to_drop: Vec<char> = Vec::new();
        for (c, (row, _col)) in self.vim.marks.iter_mut() {
            if (edit_start..drop_end).contains(row) {
                to_drop.push(*c);
            } else if *row >= shift_threshold {
                *row = ((*row as isize) + delta).max(0) as usize;
            }
        }
        for c in to_drop {
            self.vim.marks.remove(&c);
        }

        // File marks migrate the same way — only the storage differs.
        let mut to_drop: Vec<char> = Vec::new();
        for (c, (row, _col)) in self.file_marks.iter_mut() {
            if (edit_start..drop_end).contains(row) {
                to_drop.push(*c);
            } else if *row >= shift_threshold {
                *row = ((*row as isize) + delta).max(0) as usize;
            }
        }
        for c in to_drop {
            self.file_marks.remove(&c);
        }

        let shift_jumps = |entries: &mut Vec<(usize, usize)>| {
            entries.retain(|(row, _)| !(edit_start..drop_end).contains(row));
            for (row, _) in entries.iter_mut() {
                if *row >= shift_threshold {
                    *row = ((*row as isize) + delta).max(0) as usize;
                }
            }
        };
        shift_jumps(&mut self.vim.jump_back);
        shift_jumps(&mut self.vim.jump_fwd);
    }

    /// Reverse-sync helper paired with [`Editor::mutate_edit`]: rebuild
    /// the textarea from the buffer's lines + cursor, preserving yank
    /// text. Heavy (allocates a fresh `TextArea`) but correct; the
    /// textarea field disappears at the end of Phase 7f anyway.
    /// No-op since Buffer is the content authority. Retained as a
    /// shim so call sites in `mutate_edit` and friends don't have to
    /// be ripped in lockstep with the field removal.
    pub(crate) fn push_buffer_content_to_textarea(&mut self) {}

    /// Single choke-point for "the buffer just changed". Sets the
    /// dirty flag and drops the cached `content_arc` snapshot so
    /// subsequent reads rebuild from the live textarea. Callers
    /// mutating `textarea` directly (e.g. the TUI's bracketed-paste
    /// path) must invoke this to keep the cache honest.
    pub fn mark_content_dirty(&mut self) {
        self.content_dirty = true;
        self.cached_content = None;
    }

    /// Returns true if content changed since the last call, then clears the flag.
    pub fn take_dirty(&mut self) -> bool {
        let dirty = self.content_dirty;
        self.content_dirty = false;
        dirty
    }

    /// Pull-model coarse change observation. If content changed since
    /// the last call, returns `Some(Arc<String>)` with the new content
    /// and clears the dirty flag; otherwise returns `None`.
    ///
    /// Hosts that need fine-grained edit deltas (e.g., DOM patching at
    /// the character level) should diff against their own previous
    /// snapshot. The SPEC `take_changes() -> Vec<EditOp>` API lands
    /// once every edit path inside the engine is instrumented; this
    /// coarse form covers the pull-model use case in the meantime.
    pub fn take_content_change(&mut self) -> Option<std::sync::Arc<String>> {
        if !self.content_dirty {
            return None;
        }
        let arc = self.content_arc();
        self.content_dirty = false;
        Some(arc)
    }

    /// Returns the cursor's row within the visible textarea (0-based), updating
    /// the stored viewport top so subsequent calls remain accurate.
    pub fn cursor_screen_row(&mut self, height: u16) -> u16 {
        let cursor = self.buffer.cursor().row;
        let top = self.buffer.viewport().top_row;
        cursor.saturating_sub(top).min(height as usize - 1) as u16
    }

    /// Returns the cursor's screen position `(x, y)` for the textarea
    /// described by `(area_x, area_y, area_width, area_height)`.
    /// Accounts for line-number gutter and viewport scroll. Returns
    /// `None` if the cursor is outside the visible viewport.
    /// Ratatui-free equivalent of [`Editor::cursor_screen_pos`].
    pub fn cursor_screen_pos_xywh(
        &self,
        area_x: u16,
        area_y: u16,
        area_width: u16,
        area_height: u16,
    ) -> Option<(u16, u16)> {
        let pos = self.buffer.cursor();
        let v = self.buffer.viewport();
        if pos.row < v.top_row || pos.col < v.top_col {
            return None;
        }
        let lnum_width = self.buffer.row_count().to_string().len() as u16 + 2;
        let dy = (pos.row - v.top_row) as u16;
        let dx = (pos.col - v.top_col) as u16;
        if dy >= area_height || dx + lnum_width >= area_width {
            return None;
        }
        Some((area_x + lnum_width + dx, area_y + dy))
    }

    /// Ratatui [`Rect`]-flavoured wrapper around
    /// [`Editor::cursor_screen_pos_xywh`]. Behind the `ratatui`
    /// feature.
    #[cfg(feature = "ratatui")]
    pub fn cursor_screen_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.cursor_screen_pos_xywh(area.x, area.y, area.width, area.height)
    }

    pub fn vim_mode(&self) -> VimMode {
        self.vim.public_mode()
    }

    /// Bounds of the active visual-block rectangle as
    /// `(top_row, bot_row, left_col, right_col)` — all inclusive.
    /// `None` when we're not in VisualBlock mode.
    /// Read-only view of the live `/` or `?` prompt. `None` outside
    /// search-prompt mode.
    pub fn search_prompt(&self) -> Option<&crate::vim::SearchPrompt> {
        self.vim.search_prompt.as_ref()
    }

    /// Most recent committed search pattern (persists across `n` / `N`
    /// and across prompt exits). `None` before the first search.
    pub fn last_search(&self) -> Option<&str> {
        self.vim.last_search.as_deref()
    }

    /// Start/end `(row, col)` of the active char-wise Visual selection
    /// (inclusive on both ends, positionally ordered). `None` when not
    /// in Visual mode.
    pub fn char_highlight(&self) -> Option<((usize, usize), (usize, usize))> {
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
        Some((start, end))
    }

    /// Top/bottom rows of the active VisualLine selection (inclusive).
    /// `None` when we're not in VisualLine mode.
    pub fn line_highlight(&self) -> Option<(usize, usize)> {
        if self.vim_mode() != VimMode::VisualLine {
            return None;
        }
        let anchor = self.vim.visual_line_anchor;
        let cursor = self.buffer.cursor().row;
        Some((anchor.min(cursor), anchor.max(cursor)))
    }

    pub fn block_highlight(&self) -> Option<(usize, usize, usize, usize)> {
        if self.vim_mode() != VimMode::VisualBlock {
            return None;
        }
        let (ar, ac) = self.vim.block_anchor;
        let cr = self.buffer.cursor().row;
        let cc = self.vim.block_vcol;
        let top = ar.min(cr);
        let bot = ar.max(cr);
        let left = ac.min(cc);
        let right = ac.max(cc);
        Some((top, bot, left, right))
    }

    /// Active selection in `hjkl_buffer::Selection` shape. `None` when
    /// not in a Visual mode. Phase 7d-i wiring — the host hands this
    /// straight to `BufferView` once render flips off textarea
    /// (Phase 7d-ii drops the `paint_*_overlay` calls on the same
    /// switch).
    pub fn buffer_selection(&self) -> Option<hjkl_buffer::Selection> {
        use hjkl_buffer::{Position, Selection};
        match self.vim_mode() {
            VimMode::Visual => {
                let (ar, ac) = self.vim.visual_anchor;
                let head = self.buffer.cursor();
                Some(Selection::Char {
                    anchor: Position::new(ar, ac),
                    head,
                })
            }
            VimMode::VisualLine => {
                let anchor_row = self.vim.visual_line_anchor;
                let head_row = self.buffer.cursor().row;
                Some(Selection::Line {
                    anchor_row,
                    head_row,
                })
            }
            VimMode::VisualBlock => {
                let (ar, ac) = self.vim.block_anchor;
                let cr = self.buffer.cursor().row;
                let cc = self.vim.block_vcol;
                Some(Selection::Block {
                    anchor: Position::new(ar, ac),
                    head: Position::new(cr, cc),
                })
            }
            _ => None,
        }
    }

    /// Force back to normal mode (used when dismissing completions etc.)
    pub fn force_normal(&mut self) {
        self.vim.force_normal();
    }

    pub fn content(&self) -> String {
        let mut s = self.buffer.lines().join("\n");
        s.push('\n');
        s
    }

    /// Same logical output as [`content`], but returns a cached
    /// `Arc<String>` so back-to-back reads within an un-mutated window
    /// are ref-count bumps instead of multi-MB joins. The cache is
    /// invalidated by every [`mark_content_dirty`] call.
    pub fn content_arc(&mut self) -> std::sync::Arc<String> {
        if let Some(arc) = &self.cached_content {
            return std::sync::Arc::clone(arc);
        }
        let arc = std::sync::Arc::new(self.content());
        self.cached_content = Some(std::sync::Arc::clone(&arc));
        arc
    }

    pub fn set_content(&mut self, text: &str) {
        let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
        while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
            lines.pop();
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        let _ = lines;
        self.buffer = hjkl_buffer::Buffer::from_str(text);
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.mark_content_dirty();
    }

    /// Feed an SPEC [`crate::PlannedInput`] into the engine.
    ///
    /// Bridge for hosts that don't carry crossterm — buffr's CEF
    /// shell, future GUI frontends. Converts directly to the engine's
    /// internal [`Input`] type and dispatches through the vim FSM,
    /// bypassing crossterm entirely so this entry point is always
    /// available regardless of the `crossterm` feature.
    ///
    /// `Input::Mouse`, `Input::Paste`, `Input::FocusGained`,
    /// `Input::FocusLost`, and `Input::Resize` currently fall through
    /// without effect — the legacy FSM doesn't dispatch them. They're
    /// accepted so the host can pump them into the engine without
    /// special-casing.
    ///
    /// Returns `true` when the keystroke was consumed.
    pub fn feed_input(&mut self, input: crate::PlannedInput) -> bool {
        use crate::{PlannedInput, SpecialKey};
        let (key, mods) = match input {
            PlannedInput::Char(c, m) => (Key::Char(c), m),
            PlannedInput::Key(k, m) => {
                let key = match k {
                    SpecialKey::Esc => Key::Esc,
                    SpecialKey::Enter => Key::Enter,
                    SpecialKey::Backspace => Key::Backspace,
                    SpecialKey::Tab => Key::Tab,
                    // Engine's internal `Key` doesn't model BackTab as a
                    // distinct variant — fall through to the FSM as
                    // shift+Tab, matching crossterm semantics.
                    SpecialKey::BackTab => Key::Tab,
                    SpecialKey::Up => Key::Up,
                    SpecialKey::Down => Key::Down,
                    SpecialKey::Left => Key::Left,
                    SpecialKey::Right => Key::Right,
                    SpecialKey::Home => Key::Home,
                    SpecialKey::End => Key::End,
                    SpecialKey::PageUp => Key::PageUp,
                    SpecialKey::PageDown => Key::PageDown,
                    // Engine's `Key` has no Insert / F(n) — drop to Null
                    // (FSM ignores it) which matches the crossterm path
                    // (`crossterm_to_input` mapped these to Null too).
                    SpecialKey::Insert => Key::Null,
                    SpecialKey::Delete => Key::Delete,
                    SpecialKey::F(_) => Key::Null,
                };
                let m = if matches!(k, SpecialKey::BackTab) {
                    crate::Modifiers { shift: true, ..m }
                } else {
                    m
                };
                (key, m)
            }
            // Variants the legacy FSM doesn't consume yet.
            PlannedInput::Mouse(_)
            | PlannedInput::Paste(_)
            | PlannedInput::FocusGained
            | PlannedInput::FocusLost
            | PlannedInput::Resize(_, _) => return false,
        };
        if key == Key::Null {
            return false;
        }
        let event = Input {
            key,
            ctrl: mods.ctrl,
            alt: mods.alt,
            shift: mods.shift,
        };
        vim::step(self, event)
    }

    /// Drain the pending change log produced by buffer mutations.
    ///
    /// Returns a `Vec<EditOp>` covering edits applied since the last
    /// call. Empty when no edits ran. Pull-model, complementary to
    /// [`Editor::take_content_change`] which gives back the new full
    /// content.
    ///
    /// Mapping coverage:
    /// - InsertChar / InsertStr → exact `EditOp` with empty range +
    ///   replacement.
    /// - DeleteRange (`Char` kind) → exact range + empty replacement.
    /// - Replace → exact range + new replacement.
    /// - DeleteRange (`Line`/`Block`), JoinLines, SplitLines,
    ///   InsertBlock, DeleteBlockChunks → best-effort placeholder
    ///   covering the touched range. Hosts wanting per-cell deltas
    ///   should diff their own `lines()` snapshot.
    pub fn take_changes(&mut self) -> Vec<crate::types::Edit> {
        std::mem::take(&mut self.change_log)
    }

    /// Read the engine's current settings as a SPEC
    /// [`crate::types::Options`].
    ///
    /// Bridges between the legacy [`Settings`] (which carries fewer
    /// fields than SPEC) and the planned 0.1.0 trait surface. Fields
    /// not present in `Settings` fall back to vim defaults (e.g.,
    /// `expandtab=false`, `wrapscan=true`, `timeout_len=1000ms`).
    /// Once trait extraction lands, this becomes the canonical config
    /// reader and `Settings` retires.
    pub fn current_options(&self) -> crate::types::Options {
        crate::types::Options {
            shiftwidth: self.settings.shiftwidth as u32,
            tabstop: self.settings.tabstop as u32,
            textwidth: self.settings.textwidth as u32,
            expandtab: self.settings.expandtab,
            ignorecase: self.settings.ignore_case,
            smartcase: self.settings.smartcase,
            wrapscan: self.settings.wrapscan,
            wrap: match self.settings.wrap {
                hjkl_buffer::Wrap::None => crate::types::WrapMode::None,
                hjkl_buffer::Wrap::Char => crate::types::WrapMode::Char,
                hjkl_buffer::Wrap::Word => crate::types::WrapMode::Word,
            },
            readonly: self.settings.readonly,
            autoindent: self.settings.autoindent,
            undo_levels: self.settings.undo_levels,
            undo_break_on_motion: self.settings.undo_break_on_motion,
            iskeyword: self.settings.iskeyword.clone(),
            timeout_len: self.settings.timeout_len,
            ..crate::types::Options::default()
        }
    }

    /// Apply a SPEC [`crate::types::Options`] to the engine's settings.
    /// Only the fields backed by today's [`Settings`] take effect;
    /// remaining options become live once trait extraction wires them
    /// through.
    pub fn apply_options(&mut self, opts: &crate::types::Options) {
        self.settings.shiftwidth = opts.shiftwidth as usize;
        self.settings.tabstop = opts.tabstop as usize;
        self.settings.textwidth = opts.textwidth as usize;
        self.settings.expandtab = opts.expandtab;
        self.settings.ignore_case = opts.ignorecase;
        self.settings.smartcase = opts.smartcase;
        self.settings.wrapscan = opts.wrapscan;
        self.settings.wrap = match opts.wrap {
            crate::types::WrapMode::None => hjkl_buffer::Wrap::None,
            crate::types::WrapMode::Char => hjkl_buffer::Wrap::Char,
            crate::types::WrapMode::Word => hjkl_buffer::Wrap::Word,
        };
        self.settings.readonly = opts.readonly;
        self.settings.autoindent = opts.autoindent;
        self.settings.undo_levels = opts.undo_levels;
        self.settings.undo_break_on_motion = opts.undo_break_on_motion;
        self.set_iskeyword(opts.iskeyword.clone());
        self.settings.timeout_len = opts.timeout_len;
    }

    /// Active visual selection as a SPEC [`crate::types::Highlight`]
    /// with [`crate::types::HighlightKind::Selection`].
    ///
    /// Returns `None` when the editor isn't in a Visual mode.
    /// Visual-line and visual-block selections collapse to the
    /// bounding char range of the selection — the SPEC `Selection`
    /// kind doesn't carry sub-line info today; hosts that need full
    /// line / block geometry continue to read [`buffer_selection`]
    /// (the legacy [`hjkl_buffer::Selection`] shape).
    pub fn selection_highlight(&self) -> Option<crate::types::Highlight> {
        use crate::types::{Highlight, HighlightKind, Pos};
        let sel = self.buffer_selection()?;
        let (start, end) = match sel {
            hjkl_buffer::Selection::Char { anchor, head } => {
                let a = (anchor.row, anchor.col);
                let h = (head.row, head.col);
                if a <= h { (a, h) } else { (h, a) }
            }
            hjkl_buffer::Selection::Line {
                anchor_row,
                head_row,
            } => {
                let (top, bot) = if anchor_row <= head_row {
                    (anchor_row, head_row)
                } else {
                    (head_row, anchor_row)
                };
                let last_col = self.buffer.line(bot).map(|l| l.len()).unwrap_or(0);
                ((top, 0), (bot, last_col))
            }
            hjkl_buffer::Selection::Block { anchor, head } => {
                let (top, bot) = if anchor.row <= head.row {
                    (anchor.row, head.row)
                } else {
                    (head.row, anchor.row)
                };
                let (left, right) = if anchor.col <= head.col {
                    (anchor.col, head.col)
                } else {
                    (head.col, anchor.col)
                };
                ((top, left), (bot, right))
            }
        };
        Some(Highlight {
            range: Pos {
                line: start.0 as u32,
                col: start.1 as u32,
            }..Pos {
                line: end.0 as u32,
                col: end.1 as u32,
            },
            kind: HighlightKind::Selection,
        })
    }

    /// SPEC-typed highlights for `line`.
    ///
    /// Two emission modes:
    ///
    /// - **IncSearch**: the user is typing a `/` or `?` prompt and
    ///   `Editor::search_prompt` is `Some`. Live-preview matches of
    ///   the in-flight pattern surface as
    ///   [`crate::types::HighlightKind::IncSearch`].
    /// - **SearchMatch**: the prompt has been committed (or absent)
    ///   and the buffer's armed pattern is non-empty. Matches surface
    ///   as [`crate::types::HighlightKind::SearchMatch`].
    ///
    /// Selection / MatchParen / Syntax(id) variants land once the
    /// trait extraction routes the FSM's selection set + the host's
    /// syntax pipeline through the [`crate::types::Host`] trait.
    ///
    /// Returns an empty vec when there is nothing to highlight or
    /// `line` is out of bounds.
    pub fn highlights_for_line(&mut self, line: u32) -> Vec<crate::types::Highlight> {
        use crate::types::{Highlight, HighlightKind, Pos};
        let row = line as usize;
        if row >= self.buffer.lines().len() {
            return Vec::new();
        }

        // Live preview while the prompt is open beats the committed
        // pattern.
        if let Some(prompt) = self.search_prompt() {
            if prompt.text.is_empty() {
                return Vec::new();
            }
            let Ok(re) = regex::Regex::new(&prompt.text) else {
                return Vec::new();
            };
            let Some(haystack) = self.buffer.line(row) else {
                return Vec::new();
            };
            return re
                .find_iter(haystack)
                .map(|m| Highlight {
                    range: Pos {
                        line,
                        col: m.start() as u32,
                    }..Pos {
                        line,
                        col: m.end() as u32,
                    },
                    kind: HighlightKind::IncSearch,
                })
                .collect();
        }

        if self.buffer.search_pattern().is_none() {
            return Vec::new();
        }
        self.buffer
            .search_matches(row)
            .into_iter()
            .map(|(start, end)| Highlight {
                range: Pos {
                    line,
                    col: start as u32,
                }..Pos {
                    line,
                    col: end as u32,
                },
                kind: HighlightKind::SearchMatch,
            })
            .collect()
    }

    /// Build the engine's [`crate::types::RenderFrame`] for the
    /// current state. Hosts call this once per redraw and diff
    /// across frames.
    ///
    /// Coarse today — covers mode + cursor + cursor shape + viewport
    /// top + line count. SPEC-target fields (selections, highlights,
    /// command line, search prompt, status line) land once trait
    /// extraction routes them through `SelectionSet` and the
    /// `Highlight` pipeline.
    pub fn render_frame(&self) -> crate::types::RenderFrame {
        use crate::types::{CursorShape, RenderFrame, SnapshotMode};
        let (cursor_row, cursor_col) = self.cursor();
        let (mode, shape) = match self.vim_mode() {
            crate::VimMode::Normal => (SnapshotMode::Normal, CursorShape::Block),
            crate::VimMode::Insert => (SnapshotMode::Insert, CursorShape::Bar),
            crate::VimMode::Visual => (SnapshotMode::Visual, CursorShape::Block),
            crate::VimMode::VisualLine => (SnapshotMode::VisualLine, CursorShape::Block),
            crate::VimMode::VisualBlock => (SnapshotMode::VisualBlock, CursorShape::Block),
        };
        RenderFrame {
            mode,
            cursor_row: cursor_row as u32,
            cursor_col: cursor_col as u32,
            cursor_shape: shape,
            viewport_top: self.buffer.viewport().top_row as u32,
            line_count: self.buffer.lines().len() as u32,
        }
    }

    /// Capture the editor's coarse state into a serde-friendly
    /// [`crate::types::EditorSnapshot`].
    ///
    /// Today's snapshot covers mode, cursor, lines, viewport top.
    /// Registers, marks, jump list, undo tree, and full options arrive
    /// once phase 5 trait extraction lands the generic
    /// `Editor<B: Buffer, H: Host>` constructor — this method's surface
    /// stays stable; only the snapshot's internal fields grow.
    ///
    /// Distinct from the internal `snapshot` used by undo (which
    /// returns `(Vec<String>, (usize, usize))`); host-facing
    /// persistence goes through this one.
    pub fn take_snapshot(&self) -> crate::types::EditorSnapshot {
        use crate::types::{EditorSnapshot, SnapshotMode};
        let mode = match self.vim_mode() {
            crate::VimMode::Normal => SnapshotMode::Normal,
            crate::VimMode::Insert => SnapshotMode::Insert,
            crate::VimMode::Visual => SnapshotMode::Visual,
            crate::VimMode::VisualLine => SnapshotMode::VisualLine,
            crate::VimMode::VisualBlock => SnapshotMode::VisualBlock,
        };
        let cursor = self.cursor();
        let cursor = (cursor.0 as u32, cursor.1 as u32);
        let lines: Vec<String> = self.buffer.lines().to_vec();
        let viewport_top = self.buffer.viewport().top_row as u32;
        let file_marks = self
            .file_marks
            .iter()
            .map(|(c, (r, col))| (*c, (*r as u32, *col as u32)))
            .collect();
        EditorSnapshot {
            version: EditorSnapshot::VERSION,
            mode,
            cursor,
            lines,
            viewport_top,
            registers: self.registers.clone(),
            file_marks,
        }
    }

    /// Restore editor state from an [`EditorSnapshot`]. Returns
    /// [`crate::EngineError::SnapshotVersion`] if the snapshot's
    /// `version` doesn't match [`EditorSnapshot::VERSION`].
    ///
    /// Mode is best-effort: `SnapshotMode` only round-trips the
    /// status-line summary, not the full FSM state. Visual / Insert
    /// mode entry happens through synthetic key dispatch when needed.
    pub fn restore_snapshot(
        &mut self,
        snap: crate::types::EditorSnapshot,
    ) -> Result<(), crate::EngineError> {
        use crate::types::EditorSnapshot;
        if snap.version != EditorSnapshot::VERSION {
            return Err(crate::EngineError::SnapshotVersion(
                snap.version,
                EditorSnapshot::VERSION,
            ));
        }
        let text = snap.lines.join("\n");
        self.set_content(&text);
        self.jump_cursor(snap.cursor.0 as usize, snap.cursor.1 as usize);
        let mut vp = self.buffer.viewport();
        vp.top_row = snap.viewport_top as usize;
        *self.buffer.viewport_mut() = vp;
        self.registers = snap.registers;
        self.file_marks = snap
            .file_marks
            .into_iter()
            .map(|(c, (r, col))| (c, (r as usize, col as usize)))
            .collect();
        Ok(())
    }

    /// Install `text` as the pending yank buffer so the next `p`/`P` pastes
    /// it. Linewise is inferred from a trailing newline, matching how `yy`/`dd`
    /// shape their payload.
    pub fn seed_yank(&mut self, text: String) {
        let linewise = text.ends_with('\n');
        self.vim.yank_linewise = linewise;
        self.registers.unnamed = crate::registers::Slot { text, linewise };
    }

    /// Scroll the viewport down by `rows`. The cursor stays on its
    /// absolute line (vim convention) unless the scroll would take it
    /// off-screen — in that case it's clamped to the first row still
    /// visible.
    pub fn scroll_down(&mut self, rows: i16) {
        self.scroll_viewport(rows);
    }

    /// Scroll the viewport up by `rows`. Cursor stays unless it would
    /// fall off the bottom of the new viewport, then clamp to the
    /// bottom-most visible row.
    pub fn scroll_up(&mut self, rows: i16) {
        self.scroll_viewport(-rows);
    }

    /// Vim's `scrolloff` default — keep the cursor at least this many
    /// rows away from the top / bottom edge of the viewport while
    /// scrolling. Collapses to `height / 2` for tiny viewports.
    const SCROLLOFF: usize = 5;

    /// Scroll the viewport so the cursor stays at least `SCROLLOFF`
    /// rows from each edge. Replaces the bare
    /// `Buffer::ensure_cursor_visible` call at end-of-step so motions
    /// don't park the cursor on the very last visible row.
    pub(crate) fn ensure_cursor_in_scrolloff(&mut self) {
        let height = self.viewport_height.load(Ordering::Relaxed) as usize;
        if height == 0 {
            self.buffer.ensure_cursor_visible();
            return;
        }
        // Cap margin at (height - 1) / 2 so the upper + lower bands
        // can't overlap on tiny windows (margin=5 + height=10 would
        // otherwise produce contradictory clamp ranges).
        let margin = Self::SCROLLOFF.min(height.saturating_sub(1) / 2);
        // Soft-wrap path: scrolloff math runs in *screen rows*, not
        // doc rows, since a wrapped doc row spans many visual lines.
        if !matches!(self.buffer.viewport().wrap, hjkl_buffer::Wrap::None) {
            self.ensure_scrolloff_wrap(height, margin);
            return;
        }
        let cursor_row = self.buffer.cursor().row;
        let last_row = self.buffer.row_count().saturating_sub(1);
        let v = self.buffer.viewport_mut();
        // Top edge: cursor_row should sit at >= top_row + margin.
        if cursor_row < v.top_row + margin {
            v.top_row = cursor_row.saturating_sub(margin);
        }
        // Bottom edge: cursor_row should sit at <= top_row + height - 1 - margin.
        let max_bottom = height.saturating_sub(1).saturating_sub(margin);
        if cursor_row > v.top_row + max_bottom {
            v.top_row = cursor_row.saturating_sub(max_bottom);
        }
        // Clamp top_row so we never scroll past the buffer's bottom.
        let max_top = last_row.saturating_sub(height.saturating_sub(1));
        if v.top_row > max_top {
            v.top_row = max_top;
        }
        // Defer to Buffer for column-side scroll (no scrolloff for
        // horizontal scrolling — vim default `sidescrolloff = 0`).
        let cursor = self.buffer.cursor();
        self.buffer.viewport_mut().ensure_visible(cursor);
    }

    /// Soft-wrap-aware scrolloff. Walks `top_row` one visible doc row
    /// at a time so the cursor's *screen* row stays inside
    /// `[margin, height - 1 - margin]`, then clamps `top_row` so the
    /// buffer's bottom never leaves blank rows below it.
    fn ensure_scrolloff_wrap(&mut self, height: usize, margin: usize) {
        let cursor_row = self.buffer.cursor().row;
        // Step 1 — cursor above viewport: snap top to cursor row,
        // then we'll fix up the margin below.
        if cursor_row < self.buffer.viewport().top_row {
            self.buffer.viewport_mut().top_row = cursor_row;
            self.buffer.viewport_mut().top_col = 0;
        }
        // Step 2 — push top forward until cursor's screen row is
        // within the bottom margin (`csr <= height - 1 - margin`).
        let max_csr = height.saturating_sub(1).saturating_sub(margin);
        loop {
            let csr = self.buffer.cursor_screen_row().unwrap_or(0);
            if csr <= max_csr {
                break;
            }
            let top = self.buffer.viewport().top_row;
            let Some(next) = self.buffer.next_visible_row(top) else {
                break;
            };
            // Don't walk past the cursor's row.
            if next > cursor_row {
                self.buffer.viewport_mut().top_row = cursor_row;
                break;
            }
            self.buffer.viewport_mut().top_row = next;
        }
        // Step 3 — pull top backward until cursor's screen row is
        // past the top margin (`csr >= margin`).
        loop {
            let csr = self.buffer.cursor_screen_row().unwrap_or(0);
            if csr >= margin {
                break;
            }
            let top = self.buffer.viewport().top_row;
            let Some(prev) = self.buffer.prev_visible_row(top) else {
                break;
            };
            self.buffer.viewport_mut().top_row = prev;
        }
        // Step 4 — clamp top so the buffer's bottom doesn't leave
        // blank rows below it. `max_top_for_height` walks segments
        // backward from the last row until it accumulates `height`
        // screen rows.
        let max_top = self.buffer.max_top_for_height(height);
        if self.buffer.viewport().top_row > max_top {
            self.buffer.viewport_mut().top_row = max_top;
        }
        self.buffer.viewport_mut().top_col = 0;
    }

    fn scroll_viewport(&mut self, delta: i16) {
        if delta == 0 {
            return;
        }
        // Bump the buffer's viewport top within bounds.
        let total_rows = self.buffer.row_count() as isize;
        let height = self.viewport_height.load(Ordering::Relaxed) as usize;
        let cur_top = self.buffer.viewport().top_row as isize;
        let new_top = (cur_top + delta as isize)
            .max(0)
            .min((total_rows - 1).max(0)) as usize;
        self.buffer.viewport_mut().top_row = new_top;
        // Mirror to textarea so its viewport reads (still consumed by
        // a couple of helpers) stay accurate.
        let _ = cur_top;
        if height == 0 {
            return;
        }
        // Apply scrolloff: keep the cursor at least SCROLLOFF rows
        // from the visible viewport edges.
        let cursor = self.buffer.cursor();
        let margin = Self::SCROLLOFF.min(height / 2);
        let min_row = new_top + margin;
        let max_row = new_top + height.saturating_sub(1).saturating_sub(margin);
        let target_row = cursor.row.clamp(min_row, max_row.max(min_row));
        if target_row != cursor.row {
            let line_len = self
                .buffer
                .line(target_row)
                .map(|l| l.chars().count())
                .unwrap_or(0);
            let target_col = cursor.col.min(line_len.saturating_sub(1));
            self.buffer
                .set_cursor(hjkl_buffer::Position::new(target_row, target_col));
        }
    }

    pub fn goto_line(&mut self, line: usize) {
        let row = line.saturating_sub(1);
        let max = self.buffer.row_count().saturating_sub(1);
        let target = row.min(max);
        self.buffer
            .set_cursor(hjkl_buffer::Position::new(target, 0));
    }

    /// Scroll so the cursor row lands at the given viewport position:
    /// `Center` → middle row, `Top` → first row, `Bottom` → last row.
    /// Cursor stays on its absolute line; only the viewport moves.
    pub(super) fn scroll_cursor_to(&mut self, pos: CursorScrollTarget) {
        let height = self.viewport_height.load(Ordering::Relaxed) as usize;
        if height == 0 {
            return;
        }
        let cur_row = self.buffer.cursor().row;
        let cur_top = self.buffer.viewport().top_row;
        // Scrolloff awareness: `zt` lands the cursor at the top edge
        // of the viable area (top + margin), `zb` at the bottom edge
        // (top + height - 1 - margin). Match the cap used by
        // `ensure_cursor_in_scrolloff` so contradictory bounds are
        // impossible on tiny viewports.
        let margin = Self::SCROLLOFF.min(height.saturating_sub(1) / 2);
        let new_top = match pos {
            CursorScrollTarget::Center => cur_row.saturating_sub(height / 2),
            CursorScrollTarget::Top => cur_row.saturating_sub(margin),
            CursorScrollTarget::Bottom => {
                cur_row.saturating_sub(height.saturating_sub(1).saturating_sub(margin))
            }
        };
        if new_top == cur_top {
            return;
        }
        self.buffer.viewport_mut().top_row = new_top;
    }

    /// Translate a terminal mouse position into a (row, col) inside
    /// the document. The outer editor area is described by `(area_x,
    /// area_y, area_width)` (height is unused). 1-row tab bar at the
    /// top, then the textarea with 1 cell of horizontal pane padding
    /// on each side. Clicks past the line's last character clamp to
    /// the last char (Normal-mode invariant) — never past it.
    /// Char-counted, not byte-counted.
    ///
    /// Ratatui-free; [`Editor::mouse_to_doc_pos`] (behind the
    /// `ratatui` feature) is a thin `Rect`-flavoured wrapper.
    fn mouse_to_doc_pos_xy(&self, area_x: u16, area_y: u16, col: u16, row: u16) -> (usize, usize) {
        let lines = self.buffer.lines();
        let inner_top = area_y.saturating_add(1); // tab bar row
        let lnum_width = lines.len().to_string().len() as u16 + 2;
        let content_x = area_x.saturating_add(1).saturating_add(lnum_width);
        let rel_row = row.saturating_sub(inner_top) as usize;
        let top = self.buffer.viewport().top_row;
        let doc_row = (top + rel_row).min(lines.len().saturating_sub(1));
        let rel_col = col.saturating_sub(content_x) as usize;
        let line_chars = lines.get(doc_row).map(|l| l.chars().count()).unwrap_or(0);
        let last_col = line_chars.saturating_sub(1);
        (doc_row, rel_col.min(last_col))
    }

    /// Jump the cursor to the given 1-based line/column, clamped to the document.
    pub fn jump_to(&mut self, line: usize, col: usize) {
        let r = line.saturating_sub(1);
        let max_row = self.buffer.row_count().saturating_sub(1);
        let r = r.min(max_row);
        let line_len = self.buffer.line(r).map(|l| l.chars().count()).unwrap_or(0);
        let c = col.saturating_sub(1).min(line_len);
        self.buffer.set_cursor(hjkl_buffer::Position::new(r, c));
    }

    /// Jump cursor to the terminal-space mouse position; exits Visual
    /// modes if active. Ratatui-free coordinate flavour — pass the
    /// outer editor rect's `(x, y)` plus the click `(col, row)`.
    pub fn mouse_click_xy(&mut self, area_x: u16, area_y: u16, col: u16, row: u16) {
        if self.vim.is_visual() {
            self.vim.force_normal();
        }
        // Mouse-position click counts as a motion — break the active
        // insert-mode undo group when the toggle is on (vim parity).
        crate::vim::break_undo_group_in_insert(self);
        let (r, c) = self.mouse_to_doc_pos_xy(area_x, area_y, col, row);
        self.buffer.set_cursor(hjkl_buffer::Position::new(r, c));
    }

    /// Ratatui [`Rect`]-flavoured wrapper around
    /// [`Editor::mouse_click_xy`]. Behind the `ratatui` feature.
    #[cfg(feature = "ratatui")]
    pub fn mouse_click(&mut self, area: Rect, col: u16, row: u16) {
        self.mouse_click_xy(area.x, area.y, col, row);
    }

    /// Begin a mouse-drag selection: anchor at current cursor and enter Visual mode.
    pub fn mouse_begin_drag(&mut self) {
        if !self.vim.is_visual_char() {
            let cursor = self.cursor();
            self.vim.enter_visual(cursor);
        }
    }

    /// Extend an in-progress mouse drag to the given terminal-space
    /// position. Ratatui-free coordinate flavour.
    pub fn mouse_extend_drag_xy(&mut self, area_x: u16, area_y: u16, col: u16, row: u16) {
        let (r, c) = self.mouse_to_doc_pos_xy(area_x, area_y, col, row);
        self.buffer.set_cursor(hjkl_buffer::Position::new(r, c));
    }

    /// Ratatui [`Rect`]-flavoured wrapper around
    /// [`Editor::mouse_extend_drag_xy`]. Behind the `ratatui` feature.
    #[cfg(feature = "ratatui")]
    pub fn mouse_extend_drag(&mut self, area: Rect, col: u16, row: u16) {
        self.mouse_extend_drag_xy(area.x, area.y, col, row);
    }

    pub fn insert_str(&mut self, text: &str) {
        let pos = self.buffer.cursor();
        self.buffer.apply_edit(hjkl_buffer::Edit::InsertStr {
            at: pos,
            text: text.to_string(),
        });
        self.push_buffer_content_to_textarea();
        self.mark_content_dirty();
    }

    pub fn accept_completion(&mut self, completion: &str) {
        use hjkl_buffer::{Edit, MotionKind, Position};
        let cursor = self.buffer.cursor();
        let line = self.buffer.line(cursor.row).unwrap_or("").to_string();
        let chars: Vec<char> = line.chars().collect();
        let prefix_len = chars[..cursor.col.min(chars.len())]
            .iter()
            .rev()
            .take_while(|c| c.is_alphanumeric() || **c == '_')
            .count();
        if prefix_len > 0 {
            let start = Position::new(cursor.row, cursor.col - prefix_len);
            self.buffer.apply_edit(Edit::DeleteRange {
                start,
                end: cursor,
                kind: MotionKind::Char,
            });
        }
        let cursor = self.buffer.cursor();
        self.buffer.apply_edit(Edit::InsertStr {
            at: cursor,
            text: completion.to_string(),
        });
        self.push_buffer_content_to_textarea();
        self.mark_content_dirty();
    }

    pub(super) fn snapshot(&self) -> (Vec<String>, (usize, usize)) {
        let pos = self.buffer.cursor();
        (self.buffer.lines().to_vec(), (pos.row, pos.col))
    }

    /// Walk one step back through the undo history. Equivalent to the
    /// user pressing `u` in normal mode. Drains the most recent undo
    /// entry and pushes it onto the redo stack.
    pub fn undo(&mut self) {
        crate::vim::do_undo(self);
    }

    /// Walk one step forward through the redo history. Equivalent to
    /// `<C-r>` in normal mode.
    pub fn redo(&mut self) {
        crate::vim::do_redo(self);
    }

    /// Snapshot current buffer state onto the undo stack and clear
    /// the redo stack. Bounded by `settings.undo_levels` — older
    /// entries pruned. Call before any group of buffer mutations the
    /// user might want to undo as a single step.
    pub fn push_undo(&mut self) {
        let snap = self.snapshot();
        self.undo_stack.push(snap);
        self.cap_undo();
        self.redo_stack.clear();
    }

    /// Trim the undo stack down to `settings.undo_levels`, dropping
    /// the oldest entries. `undo_levels == 0` is treated as
    /// "unlimited" (vim's 0-means-no-undo semantics intentionally
    /// skipped — guarding with `> 0` is one line shorter than gating
    /// the cap path with an explicit zero-check above the call site).
    pub(crate) fn cap_undo(&mut self) {
        let cap = self.settings.undo_levels as usize;
        if cap > 0 && self.undo_stack.len() > cap {
            let diff = self.undo_stack.len() - cap;
            self.undo_stack.drain(..diff);
        }
    }

    /// Test-only accessor for the undo stack length.
    #[doc(hidden)]
    pub fn undo_stack_len(&self) -> usize {
        self.undo_stack.len()
    }

    /// Replace the buffer with `lines` joined by `\n` and set the
    /// cursor to `cursor`. Used by undo / `:e!` / snapshot restore
    /// paths. Marks the editor dirty.
    pub fn restore(&mut self, lines: Vec<String>, cursor: (usize, usize)) {
        let text = lines.join("\n");
        self.buffer.replace_all(&text);
        self.buffer
            .set_cursor(hjkl_buffer::Position::new(cursor.0, cursor.1));
        self.mark_content_dirty();
    }

    /// Returns true if the key was consumed by the editor.
    #[cfg(feature = "crossterm")]
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let input = crossterm_to_input(key);
        if input.key == Key::Null {
            return false;
        }
        vim::step(self, input)
    }
}

#[cfg(feature = "crossterm")]
impl From<KeyEvent> for Input {
    fn from(key: KeyEvent) -> Self {
        let k = match key.code {
            KeyCode::Char(c) => Key::Char(c),
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Delete => Key::Delete,
            KeyCode::Enter => Key::Enter,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            KeyCode::Tab => Key::Tab,
            KeyCode::Esc => Key::Esc,
            _ => Key::Null,
        };
        Input {
            key: k,
            ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
            alt: key.modifiers.contains(KeyModifiers::ALT),
            shift: key.modifiers.contains(KeyModifiers::SHIFT),
        }
    }
}

/// Crossterm `KeyEvent` → engine `Input`. Thin wrapper that delegates
/// to the [`From`] impl above; kept as a free fn for the in-tree
/// callers in the legacy ratatui-coupled paths.
#[cfg(feature = "crossterm")]
pub(super) fn crossterm_to_input(key: KeyEvent) -> Input {
    Input::from(key)
}

#[cfg(all(test, feature = "crossterm", feature = "ratatui"))]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn shift_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }
    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn vim_normal_to_insert() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.handle_key(key(KeyCode::Char('i')));
        assert_eq!(e.vim_mode(), VimMode::Insert);
    }

    #[test]
    fn feed_input_char_routes_through_handle_key() {
        use crate::{Modifiers, PlannedInput};
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("abc");
        // `i` enters insert mode via SPEC input.
        e.feed_input(PlannedInput::Char('i', Modifiers::default()));
        assert_eq!(e.vim_mode(), VimMode::Insert);
        // Type 'X' via SPEC input.
        e.feed_input(PlannedInput::Char('X', Modifiers::default()));
        assert!(e.content().contains('X'));
    }

    #[test]
    fn feed_input_special_key_routes() {
        use crate::{Modifiers, PlannedInput, SpecialKey};
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("abc");
        e.feed_input(PlannedInput::Char('i', Modifiers::default()));
        assert_eq!(e.vim_mode(), VimMode::Insert);
        e.feed_input(PlannedInput::Key(SpecialKey::Esc, Modifiers::default()));
        assert_eq!(e.vim_mode(), VimMode::Normal);
    }

    #[test]
    fn feed_input_mouse_paste_focus_resize_no_op() {
        use crate::{MouseEvent, MouseKind, PlannedInput, Pos};
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("abc");
        let mode_before = e.vim_mode();
        let consumed = e.feed_input(PlannedInput::Mouse(MouseEvent {
            kind: MouseKind::Press,
            pos: Pos::new(0, 0),
            mods: Default::default(),
        }));
        assert!(!consumed);
        assert_eq!(e.vim_mode(), mode_before);
        assert!(!e.feed_input(PlannedInput::Paste("xx".into())));
        assert!(!e.feed_input(PlannedInput::FocusGained));
        assert!(!e.feed_input(PlannedInput::FocusLost));
        assert!(!e.feed_input(PlannedInput::Resize(80, 24)));
    }

    #[test]
    fn intern_engine_style_dedups_with_intern_style() {
        use crate::types::{Attrs, Color, Style};
        let mut e = Editor::new(KeybindingMode::Vim);
        let s = Style {
            fg: Some(Color(255, 0, 0)),
            bg: None,
            attrs: Attrs::BOLD,
        };
        let id_a = e.intern_engine_style(s);
        // Re-interning the same engine style returns the same id.
        let id_b = e.intern_engine_style(s);
        assert_eq!(id_a, id_b);
        // Engine accessor returns the same style back.
        let back = e.engine_style_at(id_a).expect("interned");
        assert_eq!(back, s);
    }

    #[test]
    fn engine_style_at_out_of_range_returns_none() {
        let e = Editor::new(KeybindingMode::Vim);
        assert!(e.engine_style_at(99).is_none());
    }

    #[test]
    fn take_changes_emits_per_row_for_block_insert() {
        // Visual-block insert (`Ctrl-V` then `I` then text then Esc)
        // produces an InsertBlock buffer edit with one chunk per
        // selected row. take_changes should surface N EditOps,
        // not a single placeholder.
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("aaa\nbbb\nccc\nddd");
        // Place cursor at (0, 0), enter visual-block, extend down 2.
        e.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
        e.handle_key(key(KeyCode::Char('j')));
        e.handle_key(key(KeyCode::Char('j')));
        // `I` to enter insert mode at the block left edge.
        e.handle_key(shift_key(KeyCode::Char('I')));
        e.handle_key(key(KeyCode::Char('X')));
        e.handle_key(key(KeyCode::Esc));

        let changes = e.take_changes();
        // Expect at least 3 entries — one per row in the 3-row block.
        // Vim's block-I inserts on Esc; the cleanup may add more
        // EditOps for cursor sync, hence >= rather than ==.
        assert!(
            changes.len() >= 3,
            "expected >=3 EditOps for 3-row block insert, got {}: {changes:?}",
            changes.len()
        );
    }

    #[test]
    fn take_changes_drains_after_insert() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("abc");
        // Empty initially.
        assert!(e.take_changes().is_empty());
        // Type a char in insert mode.
        e.handle_key(key(KeyCode::Char('i')));
        e.handle_key(key(KeyCode::Char('X')));
        let changes = e.take_changes();
        assert!(
            !changes.is_empty(),
            "insert mode keystroke should produce a change"
        );
        // Drained — second call empty.
        assert!(e.take_changes().is_empty());
    }

    #[test]
    fn options_bridge_roundtrip() {
        let mut e = Editor::new(KeybindingMode::Vim);
        let opts = e.current_options();
        assert_eq!(opts.shiftwidth, 2); // legacy Settings default
        assert_eq!(opts.tabstop, 8);

        let new_opts = crate::types::Options {
            shiftwidth: 4,
            tabstop: 2,
            ignorecase: true,
            ..crate::types::Options::default()
        };
        e.apply_options(&new_opts);

        let after = e.current_options();
        assert_eq!(after.shiftwidth, 4);
        assert_eq!(after.tabstop, 2);
        assert!(after.ignorecase);
    }

    #[test]
    fn selection_highlight_none_in_normal() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        assert!(e.selection_highlight().is_none());
    }

    #[test]
    fn selection_highlight_some_in_visual() {
        use crate::types::HighlightKind;
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello world");
        e.handle_key(key(KeyCode::Char('v')));
        e.handle_key(key(KeyCode::Char('l')));
        e.handle_key(key(KeyCode::Char('l')));
        let h = e
            .selection_highlight()
            .expect("visual mode should produce a highlight");
        assert_eq!(h.kind, HighlightKind::Selection);
        assert_eq!(h.range.start.line, 0);
        assert_eq!(h.range.end.line, 0);
    }

    #[test]
    fn highlights_emit_incsearch_during_active_prompt() {
        use crate::types::HighlightKind;
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("foo bar foo\nbaz\n");
        // Open the `/` prompt and type `f` `o` `o`.
        e.handle_key(key(KeyCode::Char('/')));
        e.handle_key(key(KeyCode::Char('f')));
        e.handle_key(key(KeyCode::Char('o')));
        e.handle_key(key(KeyCode::Char('o')));
        // Prompt should be active.
        assert!(e.search_prompt().is_some());
        let hs = e.highlights_for_line(0);
        assert_eq!(hs.len(), 2);
        for h in &hs {
            assert_eq!(h.kind, HighlightKind::IncSearch);
        }
    }

    #[test]
    fn highlights_empty_for_blank_prompt() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("foo");
        e.handle_key(key(KeyCode::Char('/')));
        // Nothing typed yet — prompt active but text empty.
        assert!(e.search_prompt().is_some());
        assert!(e.highlights_for_line(0).is_empty());
    }

    #[test]
    fn highlights_emit_search_matches() {
        use crate::types::HighlightKind;
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("foo bar foo\nbaz qux\n");
        // Arm a search via buffer's pattern setter.
        e.buffer_mut()
            .set_search_pattern(Some(regex::Regex::new("foo").unwrap()));
        let hs = e.highlights_for_line(0);
        assert_eq!(hs.len(), 2);
        for h in &hs {
            assert_eq!(h.kind, HighlightKind::SearchMatch);
            assert_eq!(h.range.start.line, 0);
            assert_eq!(h.range.end.line, 0);
        }
    }

    #[test]
    fn highlights_empty_without_pattern() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("foo bar");
        assert!(e.highlights_for_line(0).is_empty());
    }

    #[test]
    fn highlights_empty_for_out_of_range_line() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("foo");
        e.buffer_mut()
            .set_search_pattern(Some(regex::Regex::new("foo").unwrap()));
        assert!(e.highlights_for_line(99).is_empty());
    }

    #[test]
    fn render_frame_reflects_mode_and_cursor() {
        use crate::types::{CursorShape, SnapshotMode};
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("alpha\nbeta");
        let f = e.render_frame();
        assert_eq!(f.mode, SnapshotMode::Normal);
        assert_eq!(f.cursor_shape, CursorShape::Block);
        assert_eq!(f.line_count, 2);

        e.handle_key(key(KeyCode::Char('i')));
        let f = e.render_frame();
        assert_eq!(f.mode, SnapshotMode::Insert);
        assert_eq!(f.cursor_shape, CursorShape::Bar);
    }

    #[test]
    fn snapshot_roundtrips_through_restore() {
        use crate::types::SnapshotMode;
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("alpha\nbeta\ngamma");
        e.jump_cursor(2, 3);
        let snap = e.take_snapshot();
        assert_eq!(snap.mode, SnapshotMode::Normal);
        assert_eq!(snap.cursor, (2, 3));
        assert_eq!(snap.lines.len(), 3);

        let mut other = Editor::new(KeybindingMode::Vim);
        other.restore_snapshot(snap).expect("restore");
        assert_eq!(other.cursor(), (2, 3));
        assert_eq!(other.buffer().lines().len(), 3);
    }

    #[test]
    fn restore_snapshot_rejects_version_mismatch() {
        let mut e = Editor::new(KeybindingMode::Vim);
        let mut snap = e.take_snapshot();
        snap.version = 9999;
        match e.restore_snapshot(snap) {
            Err(crate::EngineError::SnapshotVersion(got, want)) => {
                assert_eq!(got, 9999);
                assert_eq!(want, crate::types::EditorSnapshot::VERSION);
            }
            other => panic!("expected SnapshotVersion err, got {other:?}"),
        }
    }

    #[test]
    fn take_content_change_returns_some_on_first_dirty() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        let first = e.take_content_change();
        assert!(first.is_some());
        let second = e.take_content_change();
        assert!(second.is_none());
    }

    #[test]
    fn take_content_change_none_until_mutation() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        // drain
        e.take_content_change();
        assert!(e.take_content_change().is_none());
        // mutate via insert mode
        e.handle_key(key(KeyCode::Char('i')));
        e.handle_key(key(KeyCode::Char('x')));
        let after = e.take_content_change();
        assert!(after.is_some());
        assert!(after.unwrap().contains('x'));
    }

    #[test]
    fn vim_insert_to_normal() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.handle_key(key(KeyCode::Char('i')));
        e.handle_key(key(KeyCode::Esc));
        assert_eq!(e.vim_mode(), VimMode::Normal);
    }

    #[test]
    fn vim_normal_to_visual() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.handle_key(key(KeyCode::Char('v')));
        assert_eq!(e.vim_mode(), VimMode::Visual);
    }

    #[test]
    fn vim_visual_to_normal() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.handle_key(key(KeyCode::Char('v')));
        e.handle_key(key(KeyCode::Esc));
        assert_eq!(e.vim_mode(), VimMode::Normal);
    }

    #[test]
    fn vim_shift_i_moves_to_first_non_whitespace() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("   hello");
        e.jump_cursor(0, 8);
        e.handle_key(shift_key(KeyCode::Char('I')));
        assert_eq!(e.vim_mode(), VimMode::Insert);
        assert_eq!(e.cursor(), (0, 3));
    }

    #[test]
    fn vim_shift_a_moves_to_end_and_insert() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        e.handle_key(shift_key(KeyCode::Char('A')));
        assert_eq!(e.vim_mode(), VimMode::Insert);
        assert_eq!(e.cursor().1, 5);
    }

    #[test]
    fn count_10j_moves_down_10() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content(
            (0..20)
                .map(|i| format!("line{i}"))
                .collect::<Vec<_>>()
                .join("\n")
                .as_str(),
        );
        for d in "10".chars() {
            e.handle_key(key(KeyCode::Char(d)));
        }
        e.handle_key(key(KeyCode::Char('j')));
        assert_eq!(e.cursor().0, 10);
    }

    #[test]
    fn count_o_repeats_insert_on_esc() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        for d in "3".chars() {
            e.handle_key(key(KeyCode::Char(d)));
        }
        e.handle_key(key(KeyCode::Char('o')));
        assert_eq!(e.vim_mode(), VimMode::Insert);
        for c in "world".chars() {
            e.handle_key(key(KeyCode::Char(c)));
        }
        e.handle_key(key(KeyCode::Esc));
        assert_eq!(e.vim_mode(), VimMode::Normal);
        assert_eq!(e.buffer().lines().len(), 4);
        assert!(e.buffer().lines().iter().skip(1).all(|l| l == "world"));
    }

    #[test]
    fn count_i_repeats_text_on_esc() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("");
        for d in "3".chars() {
            e.handle_key(key(KeyCode::Char(d)));
        }
        e.handle_key(key(KeyCode::Char('i')));
        for c in "ab".chars() {
            e.handle_key(key(KeyCode::Char(c)));
        }
        e.handle_key(key(KeyCode::Esc));
        assert_eq!(e.vim_mode(), VimMode::Normal);
        assert_eq!(e.buffer().lines()[0], "ababab");
    }

    #[test]
    fn vim_shift_o_opens_line_above() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        e.handle_key(shift_key(KeyCode::Char('O')));
        assert_eq!(e.vim_mode(), VimMode::Insert);
        assert_eq!(e.cursor(), (0, 0));
        assert_eq!(e.buffer().lines().len(), 2);
    }

    #[test]
    fn vim_gg_goes_to_top() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("a\nb\nc");
        e.jump_cursor(2, 0);
        e.handle_key(key(KeyCode::Char('g')));
        e.handle_key(key(KeyCode::Char('g')));
        assert_eq!(e.cursor().0, 0);
    }

    #[test]
    fn vim_shift_g_goes_to_bottom() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("a\nb\nc");
        e.handle_key(shift_key(KeyCode::Char('G')));
        assert_eq!(e.cursor().0, 2);
    }

    #[test]
    fn vim_dd_deletes_line() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("first\nsecond");
        e.handle_key(key(KeyCode::Char('d')));
        e.handle_key(key(KeyCode::Char('d')));
        assert_eq!(e.buffer().lines().len(), 1);
        assert_eq!(e.buffer().lines()[0], "second");
    }

    #[test]
    fn vim_dw_deletes_word() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello world");
        e.handle_key(key(KeyCode::Char('d')));
        e.handle_key(key(KeyCode::Char('w')));
        assert_eq!(e.vim_mode(), VimMode::Normal);
        assert!(!e.buffer().lines()[0].starts_with("hello"));
    }

    #[test]
    fn vim_yy_yanks_line() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello\nworld");
        e.handle_key(key(KeyCode::Char('y')));
        e.handle_key(key(KeyCode::Char('y')));
        assert!(e.last_yank.as_deref().unwrap_or("").starts_with("hello"));
    }

    #[test]
    fn vim_yy_does_not_move_cursor() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("first\nsecond\nthird");
        e.jump_cursor(1, 0);
        let before = e.cursor();
        e.handle_key(key(KeyCode::Char('y')));
        e.handle_key(key(KeyCode::Char('y')));
        assert_eq!(e.cursor(), before);
        assert_eq!(e.vim_mode(), VimMode::Normal);
    }

    #[test]
    fn vim_yw_yanks_word() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello world");
        e.handle_key(key(KeyCode::Char('y')));
        e.handle_key(key(KeyCode::Char('w')));
        assert_eq!(e.vim_mode(), VimMode::Normal);
        assert!(e.last_yank.is_some());
    }

    #[test]
    fn vim_cc_changes_line() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello\nworld");
        e.handle_key(key(KeyCode::Char('c')));
        e.handle_key(key(KeyCode::Char('c')));
        assert_eq!(e.vim_mode(), VimMode::Insert);
    }

    #[test]
    fn vim_u_undoes_insert_session_as_chunk() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        e.handle_key(key(KeyCode::Char('i')));
        e.handle_key(key(KeyCode::Enter));
        e.handle_key(key(KeyCode::Enter));
        e.handle_key(key(KeyCode::Esc));
        assert_eq!(e.buffer().lines().len(), 3);
        e.handle_key(key(KeyCode::Char('u')));
        assert_eq!(e.buffer().lines().len(), 1);
        assert_eq!(e.buffer().lines()[0], "hello");
    }

    #[test]
    fn vim_undo_redo_roundtrip() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        e.handle_key(key(KeyCode::Char('i')));
        for c in "world".chars() {
            e.handle_key(key(KeyCode::Char(c)));
        }
        e.handle_key(key(KeyCode::Esc));
        let after = e.buffer().lines()[0].clone();
        e.handle_key(key(KeyCode::Char('u')));
        assert_eq!(e.buffer().lines()[0], "hello");
        e.handle_key(ctrl_key(KeyCode::Char('r')));
        assert_eq!(e.buffer().lines()[0], after);
    }

    #[test]
    fn vim_u_undoes_dd() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("first\nsecond");
        e.handle_key(key(KeyCode::Char('d')));
        e.handle_key(key(KeyCode::Char('d')));
        assert_eq!(e.buffer().lines().len(), 1);
        e.handle_key(key(KeyCode::Char('u')));
        assert_eq!(e.buffer().lines().len(), 2);
        assert_eq!(e.buffer().lines()[0], "first");
    }

    #[test]
    fn vim_ctrl_r_redoes() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        e.handle_key(ctrl_key(KeyCode::Char('r')));
    }

    #[test]
    fn vim_r_replaces_char() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        e.handle_key(key(KeyCode::Char('r')));
        e.handle_key(key(KeyCode::Char('x')));
        assert_eq!(e.buffer().lines()[0].chars().next(), Some('x'));
    }

    #[test]
    fn vim_tilde_toggles_case() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        e.handle_key(key(KeyCode::Char('~')));
        assert_eq!(e.buffer().lines()[0].chars().next(), Some('H'));
    }

    #[test]
    fn vim_visual_d_cuts() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        e.handle_key(key(KeyCode::Char('v')));
        e.handle_key(key(KeyCode::Char('l')));
        e.handle_key(key(KeyCode::Char('l')));
        e.handle_key(key(KeyCode::Char('d')));
        assert_eq!(e.vim_mode(), VimMode::Normal);
        assert!(e.last_yank.is_some());
    }

    #[test]
    fn vim_visual_c_enters_insert() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        e.handle_key(key(KeyCode::Char('v')));
        e.handle_key(key(KeyCode::Char('l')));
        e.handle_key(key(KeyCode::Char('c')));
        assert_eq!(e.vim_mode(), VimMode::Insert);
    }

    #[test]
    fn vim_normal_unknown_key_consumed() {
        let mut e = Editor::new(KeybindingMode::Vim);
        // Unknown keys are consumed (swallowed) rather than returning false.
        let consumed = e.handle_key(key(KeyCode::Char('z')));
        assert!(consumed);
    }

    #[test]
    fn force_normal_clears_operator() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.handle_key(key(KeyCode::Char('d')));
        e.force_normal();
        assert_eq!(e.vim_mode(), VimMode::Normal);
    }

    fn many_lines(n: usize) -> String {
        (0..n)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn prime_viewport(e: &mut Editor<'_>, height: u16) {
        e.set_viewport_height(height);
    }

    #[test]
    fn zz_centers_cursor_in_viewport() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content(&many_lines(100));
        prime_viewport(&mut e, 20);
        e.jump_cursor(50, 0);
        e.handle_key(key(KeyCode::Char('z')));
        e.handle_key(key(KeyCode::Char('z')));
        assert_eq!(e.buffer().viewport().top_row, 40);
        assert_eq!(e.cursor().0, 50);
    }

    #[test]
    fn zt_puts_cursor_at_viewport_top_with_scrolloff() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content(&many_lines(100));
        prime_viewport(&mut e, 20);
        e.jump_cursor(50, 0);
        e.handle_key(key(KeyCode::Char('z')));
        e.handle_key(key(KeyCode::Char('t')));
        // Cursor lands at top of viable area = top + SCROLLOFF (5).
        // Viewport top therefore sits at cursor - 5.
        assert_eq!(e.buffer().viewport().top_row, 45);
        assert_eq!(e.cursor().0, 50);
    }

    #[test]
    fn ctrl_a_increments_number_at_cursor() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("x = 41");
        e.handle_key(ctrl_key(KeyCode::Char('a')));
        assert_eq!(e.buffer().lines()[0], "x = 42");
        assert_eq!(e.cursor(), (0, 5));
    }

    #[test]
    fn ctrl_a_finds_number_to_right_of_cursor() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("foo 99 bar");
        e.handle_key(ctrl_key(KeyCode::Char('a')));
        assert_eq!(e.buffer().lines()[0], "foo 100 bar");
        assert_eq!(e.cursor(), (0, 6));
    }

    #[test]
    fn ctrl_a_with_count_adds_count() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("x = 10");
        for d in "5".chars() {
            e.handle_key(key(KeyCode::Char(d)));
        }
        e.handle_key(ctrl_key(KeyCode::Char('a')));
        assert_eq!(e.buffer().lines()[0], "x = 15");
    }

    #[test]
    fn ctrl_x_decrements_number() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("n=5");
        e.handle_key(ctrl_key(KeyCode::Char('x')));
        assert_eq!(e.buffer().lines()[0], "n=4");
    }

    #[test]
    fn ctrl_x_crosses_zero_into_negative() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("v=0");
        e.handle_key(ctrl_key(KeyCode::Char('x')));
        assert_eq!(e.buffer().lines()[0], "v=-1");
    }

    #[test]
    fn ctrl_a_on_negative_number_increments_toward_zero() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("a = -5");
        e.handle_key(ctrl_key(KeyCode::Char('a')));
        assert_eq!(e.buffer().lines()[0], "a = -4");
    }

    #[test]
    fn ctrl_a_noop_when_no_digit_on_line() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("no digits here");
        e.handle_key(ctrl_key(KeyCode::Char('a')));
        assert_eq!(e.buffer().lines()[0], "no digits here");
    }

    #[test]
    fn zb_puts_cursor_at_viewport_bottom_with_scrolloff() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content(&many_lines(100));
        prime_viewport(&mut e, 20);
        e.jump_cursor(50, 0);
        e.handle_key(key(KeyCode::Char('z')));
        e.handle_key(key(KeyCode::Char('b')));
        // Cursor lands at bottom of viable area = top + height - 1 -
        // SCROLLOFF. For height 20, scrolloff 5: cursor at top + 14,
        // so top = cursor - 14 = 36.
        assert_eq!(e.buffer().viewport().top_row, 36);
        assert_eq!(e.cursor().0, 50);
    }

    /// Contract that the TUI drain relies on: `set_content` flags the
    /// editor dirty (so the next `take_dirty` call reports the change),
    /// and a second `take_dirty` returns `false` after consumption. The
    /// TUI drains this flag after every programmatic content load so
    /// opening a tab doesn't get mistaken for a user edit and mark the
    /// tab dirty (which would then trigger the quit-prompt on `:q`).
    #[test]
    fn set_content_dirties_then_take_dirty_clears() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        assert!(
            e.take_dirty(),
            "set_content should leave content_dirty=true"
        );
        assert!(!e.take_dirty(), "take_dirty should clear the flag");
    }

    #[test]
    fn content_arc_returns_same_arc_until_mutation() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        let a = e.content_arc();
        let b = e.content_arc();
        assert!(
            std::sync::Arc::ptr_eq(&a, &b),
            "repeated content_arc() should hit the cache"
        );

        // Any mutation must invalidate the cache.
        e.handle_key(key(KeyCode::Char('i')));
        e.handle_key(key(KeyCode::Char('!')));
        let c = e.content_arc();
        assert!(
            !std::sync::Arc::ptr_eq(&a, &c),
            "mutation should invalidate content_arc() cache"
        );
        assert!(c.contains('!'));
    }

    #[test]
    fn content_arc_cache_invalidated_by_set_content() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("one");
        let a = e.content_arc();
        e.set_content("two");
        let b = e.content_arc();
        assert!(!std::sync::Arc::ptr_eq(&a, &b));
        assert!(b.starts_with("two"));
    }

    /// Click past the last char of a line should land the cursor on
    /// the line's last char (Normal mode), not one past it. The
    /// previous bug clamped to the line's BYTE length and used `>=`
    /// past-end, so clicking deep into the trailing space parked the
    /// cursor at `chars().count()` — past where Normal mode lives.
    #[test]
    fn mouse_click_past_eol_lands_on_last_char() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello");
        // Outer editor area: x=0, y=0, width=80. mouse_to_doc_pos
        // reserves row 0 for the tab bar and adds gutter padding,
        // so click row 1, way past the line end.
        let area = ratatui::layout::Rect::new(0, 0, 80, 10);
        e.mouse_click(area, 78, 1);
        assert_eq!(e.cursor(), (0, 4));
    }

    #[test]
    fn mouse_click_past_eol_handles_multibyte_line() {
        let mut e = Editor::new(KeybindingMode::Vim);
        // 5 chars, 6 bytes — old code's `String::len()` clamp was
        // wrong here.
        e.set_content("héllo");
        let area = ratatui::layout::Rect::new(0, 0, 80, 10);
        e.mouse_click(area, 78, 1);
        assert_eq!(e.cursor(), (0, 4));
    }

    #[test]
    fn mouse_click_inside_line_lands_on_clicked_char() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello world");
        // Gutter is `lnum_width + 1` = (1-digit row count + 2) + 1
        // pane padding = 4 cells; click col 4 is the first char.
        let area = ratatui::layout::Rect::new(0, 0, 80, 10);
        e.mouse_click(area, 4, 1);
        assert_eq!(e.cursor(), (0, 0));
        e.mouse_click(area, 6, 1);
        assert_eq!(e.cursor(), (0, 2));
    }

    /// Vim parity: a mouse-position click during insert mode counts
    /// as a motion and breaks the active undo group (when
    /// `undo_break_on_motion` is on, the default). After clicking and
    /// typing more chars, `u` should reverse only the post-click run.
    #[test]
    fn mouse_click_breaks_insert_undo_group_when_undobreak_on() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello world");
        let area = ratatui::layout::Rect::new(0, 0, 80, 10);
        // Default settings.undo_break_on_motion = true.
        assert!(e.settings().undo_break_on_motion);
        // Enter insert mode and type "AAA" before the line content.
        e.handle_key(key(KeyCode::Char('i')));
        e.handle_key(key(KeyCode::Char('A')));
        e.handle_key(key(KeyCode::Char('A')));
        e.handle_key(key(KeyCode::Char('A')));
        // Mouse click somewhere else on the line (still insert mode).
        e.mouse_click(area, 10, 1);
        // Type more chars at the new cursor position.
        e.handle_key(key(KeyCode::Char('B')));
        e.handle_key(key(KeyCode::Char('B')));
        e.handle_key(key(KeyCode::Char('B')));
        // Leave insert and undo once.
        e.handle_key(key(KeyCode::Esc));
        e.handle_key(key(KeyCode::Char('u')));
        let line = e.buffer().line(0).unwrap_or("").to_string();
        assert!(
            line.contains("AAA"),
            "AAA must survive undo (separate group): {line:?}"
        );
        assert!(
            !line.contains("BBB"),
            "BBB must be undone (post-click group): {line:?}"
        );
    }

    /// With `:set noundobreak`, the entire insert run — including
    /// chars typed before AND after a mouse click — should collapse
    /// into one undo group, so `u` clears everything.
    #[test]
    fn mouse_click_keeps_one_undo_group_when_undobreak_off() {
        let mut e = Editor::new(KeybindingMode::Vim);
        e.set_content("hello world");
        e.settings_mut().undo_break_on_motion = false;
        let area = ratatui::layout::Rect::new(0, 0, 80, 10);
        e.handle_key(key(KeyCode::Char('i')));
        e.handle_key(key(KeyCode::Char('A')));
        e.handle_key(key(KeyCode::Char('A')));
        e.mouse_click(area, 10, 1);
        e.handle_key(key(KeyCode::Char('B')));
        e.handle_key(key(KeyCode::Char('B')));
        e.handle_key(key(KeyCode::Esc));
        e.handle_key(key(KeyCode::Char('u')));
        let line = e.buffer().line(0).unwrap_or("").to_string();
        assert!(
            !line.contains("AA") && !line.contains("BB"),
            "with undobreak off, single `u` must reverse whole insert: {line:?}"
        );
        assert_eq!(line, "hello world");
    }
}
