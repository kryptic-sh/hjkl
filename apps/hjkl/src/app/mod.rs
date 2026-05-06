//! `App` — owns the editor + host, drives the event loop.

use anyhow::Result;
use hjkl_buffer::Buffer;
use hjkl_engine::{BufferEdit, Host};
use hjkl_engine::{CursorShape, Editor, Options, VimMode};
use hjkl_form::TextFieldEditor;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crate::host::TuiHost;
use crate::syntax::{self, BufferId, SyntaxLayer};

mod buffer_ops;
mod event_loop;
mod ex_dispatch;
pub mod lsp_glue;
mod picker_glue;
mod prompt;
mod syntax_glue;
#[cfg(test)]
mod tests;
pub mod window;

/// Height reserved for the status line at the bottom of the screen.
pub const STATUS_LINE_HEIGHT: u16 = 1;

/// How long a grammar-load failure stays visible in the status line before
/// auto-expiring.
const GRAMMAR_ERR_TTL: Duration = Duration::from_secs(5);

/// A grammar-load failure surfaced as a transient status message.
#[derive(Clone)]
pub(crate) struct GrammarLoadError {
    pub name: String,
    pub message: String,
    pub at: Instant,
}

impl GrammarLoadError {
    pub fn is_expired(&self) -> bool {
        self.at.elapsed() >= GRAMMAR_ERR_TTL
    }
}

/// Height of the buffer/tab line at the top of the screen, when shown.
pub const BUFFER_LINE_HEIGHT: u16 = 1;

/// Height of the vim-style tab bar at the top of the screen, when shown
/// (only when more than one tab is open).
pub const TAB_BAR_HEIGHT: u16 = 1;

/// Resolve a path for buffer-list matching. Two paths that point to
/// the same file should compare equal here even when one is relative
/// and the other absolute. We try `canonicalize` first (only works for
/// files that exist on disk) and fall back to lexical absolutization
/// for new-file paths.
fn canon_for_match(p: &std::path::Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    if p.is_absolute() {
        p.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(p)
    } else {
        p.to_path_buf()
    }
}

/// Hash + byte-length of the buffer's canonical line content (lines
/// joined by `\n` — same shape as what `:w` writes, modulo the trailing
/// newline). Used to detect "buffer matches the saved snapshot" so undo
/// back to the saved state clears the dirty flag.
fn buffer_signature(editor: &Editor<Buffer, TuiHost>) -> (u64, usize) {
    let mut hasher = DefaultHasher::new();
    let mut len = 0usize;
    let lines = editor.buffer().lines();
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            b'\n'.hash(&mut hasher);
            len += 1;
        }
        l.hash(&mut hasher);
        len += l.len();
    }
    (hasher.finish(), len)
}

/// Whether the on-disk file is in sync with what was last loaded/saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskState {
    /// File matches what we loaded/saved last.
    Synced,
    /// File changed on disk since last load/save (and buffer is dirty — no auto-reload).
    ChangedOnDisk,
    /// File no longer exists on disk.
    DeletedOnDisk,
}

/// Direction of an active host-driven search prompt. `/` opens a
/// forward prompt, `?` opens a backward one. The direction is recorded
/// alongside [`App::search_field`] so the commit path can call the
/// matching `Editor::search_advance_*` and persist the direction onto
/// the engine's `last_search_forward` for future `n` / `N` repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDir {
    Forward,
    Backward,
}

/// LSP diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagSeverity {
    Error = 1,
    Warning = 2,
    Info = 3,
    Hint = 4,
}

/// A single LSP diagnostic stored on a `BufferSlot`.
#[derive(Debug, Clone)]
pub struct LspDiag {
    /// 0-based start row.
    pub start_row: usize,
    /// 0-based start char-column.
    pub start_col: usize,
    /// 0-based end row.
    pub end_row: usize,
    /// 0-based end char-column.
    pub end_col: usize,
    pub severity: DiagSeverity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
}

/// Snapshot of a running LSP server's state, tracked by the app.
pub struct LspServerInfo {
    pub initialized: bool,
    pub capabilities: serde_json::Value,
}

/// Tracks an in-flight LSP request so the response handler knows what to do.
/// Each variant carries the buffer context and cursor origin so the result can
/// be acted on (jump, picker, popup) without re-reading app state at response
/// time (the active buffer may have changed by then).
#[derive(Debug, Clone)]
pub enum LspPendingRequest {
    GotoDefinition {
        buffer_id: hjkl_lsp::BufferId,
        /// 0-based (row, col) of the cursor when the request was sent.
        origin: (usize, usize),
    },
    GotoDeclaration {
        buffer_id: hjkl_lsp::BufferId,
        origin: (usize, usize),
    },
    GotoTypeDefinition {
        buffer_id: hjkl_lsp::BufferId,
        origin: (usize, usize),
    },
    GotoImplementation {
        buffer_id: hjkl_lsp::BufferId,
        origin: (usize, usize),
    },
    GotoReferences {
        buffer_id: hjkl_lsp::BufferId,
        origin: (usize, usize),
    },
    Hover {
        buffer_id: hjkl_lsp::BufferId,
        origin: (usize, usize),
    },
}

/// Per-buffer state. Phase B: App holds `Vec<BufferSlot>` + `active: usize`.
/// Phase C will add bnext / bdelete / switch-or-create.
pub struct BufferSlot {
    /// Stable id used to multiplex the SyntaxLayer / Worker.
    pub buffer_id: BufferId,
    /// The live editor — buffer + FSM + host, all in one.
    pub editor: Editor<Buffer, TuiHost>,
    /// File path shown in status line and used for `:w` saves.
    pub filename: Option<PathBuf>,
    /// Persistent dirty flag. Set when `editor.take_dirty()` returns `true`;
    /// cleared after a successful `:w` save.
    pub dirty: bool,
    /// True when a file was requested but not found on disk — shows
    /// "[New File]" annotation in the status line until the first edit
    /// or successful `:w`.
    pub is_new_file: bool,
    /// `true` when the current file is in a git repo but not in HEAD —
    /// drives the `[Untracked]` status-line tag. Refreshed alongside
    /// `git_signs`.
    pub is_untracked: bool,
    /// Diagnostic gutter signs (tree-sitter ERROR / MISSING) for the
    /// current viewport. Refreshed by `recompute_and_install`; read by
    /// `render::buffer_pane`.
    pub diag_signs: Vec<hjkl_buffer::Sign>,
    /// LSP diagnostic gutter signs. Separate from `diag_signs` so the
    /// oracle/syntax source can be cleared independently of LSP.
    pub diag_signs_lsp: Vec<hjkl_buffer::Sign>,
    /// Full LSP diagnostic list for the buffer. Replaced wholesale each
    /// time `textDocument/publishDiagnostics` arrives (server is source
    /// of truth — empty notification clears all diags).
    pub lsp_diags: Vec<LspDiag>,
    /// `dirty_gen` of the buffer the last time we sent `textDocument/didChange`
    /// to the LSP. `None` = never sent.
    pub(crate) last_lsp_dirty_gen: Option<u64>,
    /// Git diff signs (`+` / `~` / `_`) against HEAD. Recomputed
    /// whenever the buffer's `dirty_gen` advances so unsaved edits
    /// show in the gutter live. Filtered to the viewport per-frame
    /// in the renderer.
    pub git_signs: Vec<hjkl_buffer::Sign>,
    /// `dirty_gen` of the buffer when `git_signs` was last rebuilt.
    /// `None` = stale, force recompute on next render.
    last_git_dirty_gen: Option<u64>,
    /// Wall-clock time of the last successful git_signs refresh — used
    /// to throttle the libgit2 diff to ~4 Hz during active typing on
    /// large files.
    last_git_refresh_at: Instant,
    /// Wall-clock time of the last syntax recompute+install.
    last_recompute_at: Instant,
    /// `(dirty_gen, vp_top, vp_height)` snapshot of the last call to
    /// `recompute_and_install`. When the next call has identical
    /// inputs, the syntax span recompute + install is skipped.
    last_recompute_key: Option<(u64, usize, usize)>,
    /// Hash + byte-length of the buffer content as it was at the most
    /// recent save (or load).
    saved_hash: u64,
    saved_len: usize,
    /// mtime of the file on disk at the most recent load or save.
    pub disk_mtime: Option<SystemTime>,
    /// Byte length of the file on disk at the most recent load or save.
    pub disk_len: Option<u64>,
    /// Whether the on-disk file is in sync, changed, or deleted.
    pub disk_state: DiskState,
}

impl BufferSlot {
    /// Snapshot the loaded content so undo-to-saved clears dirty.
    fn snapshot_saved(&mut self) {
        let (h, l) = buffer_signature(&self.editor);
        self.saved_hash = h;
        self.saved_len = l;
        self.dirty = false;
    }

    /// Sync `self.dirty` against a fresh content comparison.
    fn refresh_dirty_against_saved(&mut self) -> u128 {
        let t = std::time::Instant::now();
        let (h, l) = buffer_signature(&self.editor);
        let elapsed = t.elapsed().as_micros();
        self.dirty = h != self.saved_hash || l != self.saved_len;
        elapsed
    }
}

/// Top-level application state. Everything the event loop and renderer need.
pub struct App {
    /// All open buffer slots. Never empty — always at least one slot.
    slots: Vec<BufferSlot>,
    /// Window list. Indexed by `WindowId`. Entries are `Option<Window>`;
    /// closed windows are set to `None` so ids stay stable.
    pub windows: Vec<Option<window::Window>>,
    /// All open tabs. Each tab owns its own layout tree + focused window.
    /// Never empty — always at least one tab.
    pub tabs: Vec<window::Tab>,
    /// Index of the currently active tab into `tabs`.
    pub active_tab: usize,
    /// Counter for the next fresh `WindowId`.
    next_window_id: window::WindowId,
    /// `true` while waiting for the second key of a `Ctrl-w` chord.
    pub pending_window_motion: bool,
    /// Monotonic counter for fresh `BufferId`s. Slot 0 takes id 0; new
    /// slots created via `:e <new-path>` or replacements after `:bd` on
    /// the last slot consume the next value.
    next_buffer_id: BufferId,
    /// The slot that was active just before the most recent `switch_to`
    /// call. Used by `<C-^>` / `:b#` to jump to the alternate buffer.
    pub prev_active: Option<usize>,
    /// Set to `true` when the FSM or Ctrl-C wants to quit.
    pub exit_requested: bool,
    /// Last ex-command result (Info / Error / write confirmation).
    /// Shown in the status line; cleared on next keypress.
    pub status_message: Option<String>,
    /// Multi-line info popup (e.g. from `:reg`, `:marks`, `:jumps`,
    /// `:changes`). When `Some`, rendered as a centered overlay; any
    /// keypress dismisses it without dispatching to the editor.
    pub info_popup: Option<String>,
    /// Active `:` command input. `Some` while the user is typing an ex
    /// command. Backed by a vim-grammar [`TextFieldEditor`] so motions
    /// (h/l/w/b/dw/diw/...) work inside the prompt.
    pub command_field: Option<TextFieldEditor>,
    /// Active `/` (forward) / `?` (backward) search prompt.
    pub search_field: Option<TextFieldEditor>,
    /// Active picker overlay (file, buffer, grep, …).
    pub picker: Option<crate::picker::Picker>,
    /// `true` after the user pressed `<Space>` in normal mode and we're
    /// waiting for the next key to resolve the leader sequence.
    pub pending_leader: bool,
    /// `true` after the user typed `<leader>g` — waiting for the next key
    /// to resolve the git sub-command (e.g. `s` → git status picker).
    pub pending_git: bool,
    /// Pending buffer-motion prefix key in normal mode. Set to `'g'`
    /// after pressing `g`, `']'` after `]`, `'['` after `[`. Cleared
    /// once the motion is resolved or forwarded to the engine.
    pub pending_buffer_motion: Option<char>,
    /// Direction of the active `search_field`.
    pub search_dir: SearchDir,
    /// Last cursor shape we emitted to the terminal.
    last_cursor_shape: CursorShape,
    /// Tree-sitter syntax highlighting layer. Owns the worker thread + the
    /// active theme. Multiplexed by BufferId.
    syntax: SyntaxLayer,
    /// Shared grammar resolver. `Arc` so the syntax layer and every picker
    /// source point at the same in-memory `Grammar` cache (one dlopen +
    /// query parse per language, app-wide).
    pub directory: std::sync::Arc<crate::lang::LanguageDirectory>,
    /// App-wide theme (UI chrome + syntax). Loaded once at startup from
    /// `themes/{ui,syntax}-dark.toml` baked via include_str!.
    pub theme: crate::theme::AppTheme,
    /// Toggled by `:perf`. When true, render shows last-frame timings.
    pub perf_overlay: bool,
    pub last_recompute_us: u128,
    pub last_install_us: u128,
    pub last_signature_us: u128,
    pub last_git_us: u128,
    pub last_perf: crate::syntax::PerfBreakdown,
    /// Counters surfaced in `:perf` so the user can verify cache ratios.
    pub recompute_hits: u64,
    pub recompute_throttled: u64,
    pub recompute_runs: u64,
    /// User config (bundled defaults + optional XDG overrides). Tests
    /// receive `Config::default()` (the bundled values); main wires the
    /// XDG-merged value via [`Self::with_config`] before entering the
    /// event loop.
    pub config: crate::config::Config,
    /// Animated start screen shown when no file argument was given.
    /// Cleared (set to `None`) on the first keypress.
    pub start_screen: Option<crate::start_screen::StartScreen>,
    /// Recent grammar-load failure surfaced as a transient status message.
    /// Auto-expires after `GRAMMAR_ERR_TTL` so a stale error doesn't stick.
    pub(crate) grammar_load_error: Option<GrammarLoadError>,
    /// LSP subsystem handle. `None` when `config.lsp.enabled = false` (default).
    pub lsp: Option<hjkl_lsp::LspManager>,
    /// Tracks the state of running LSP servers. Populated/updated by
    /// `drain_lsp_events` on `ServerInitialized` / `ServerExited`.
    pub lsp_state: HashMap<hjkl_lsp::ServerKey, LspServerInfo>,
    /// Monotonic counter for allocating request ids sent to the LSP runtime.
    pub lsp_next_request_id: i64,
    /// Maps app-allocated request id → what the request was for, so the
    /// response handler knows how to act on the result.
    pub lsp_pending: HashMap<i64, LspPendingRequest>,
}

/// Resolve the cursor shape for an active prompt field (`command_field` or
/// `search_field`). Insert mode → Bar; anything else → Block.
fn prompt_cursor_shape(field: &hjkl_form::TextFieldEditor) -> CursorShape {
    match field.vim_mode() {
        hjkl_form::VimMode::Insert => CursorShape::Bar,
        _ => CursorShape::Block,
    }
}

/// Build a [`BufferSlot`] from disk content.
///
/// - `path = None` → empty unnamed scratch buffer (used by `:bd` on the
///   last slot; today `open_new_slot`/`App::new` always pass `Some(path)`,
///   but accepting `None` lets future call sites converge here too).
/// - `path = Some(p)` and file missing → `is_new_file = true`,
///   buffer empty, filename retained.
/// - `path = Some(p)` and file unreadable → `Err`.
///
/// Both original call sites used `wait_for_initial_result(150ms)`; that
/// method is kept here as the single canonical timeout.
pub(super) fn build_slot(
    syntax: &mut SyntaxLayer,
    buffer_id: BufferId,
    path: Option<PathBuf>,
    config: &crate::config::Config,
) -> Result<BufferSlot, String> {
    let mut buffer = Buffer::new();
    let mut is_new_file = false;
    let mut disk_mtime: Option<SystemTime> = None;
    let mut disk_len: Option<u64> = None;
    if let Some(ref p) = path {
        match std::fs::read_to_string(p) {
            Ok(content) => {
                // Snapshot disk metadata right after a successful read.
                if let Ok(meta) = std::fs::metadata(p) {
                    disk_mtime = meta.modified().ok();
                    disk_len = Some(meta.len());
                }
                let content = content.strip_suffix('\n').unwrap_or(&content);
                BufferEdit::replace_all(&mut buffer, content);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                is_new_file = true;
            }
            Err(e) => return Err(format!("E484: Can't open file {}: {e}", p.display())),
        }
    }

    let host = TuiHost::new();
    // Seed Options from user config — editorconfig overlay (if any) takes
    // precedence over the user-config fallback values.
    let mut ec_opts = Options {
        expandtab: config.editor.expandtab,
        tabstop: config.editor.tab_width as u32,
        shiftwidth: config.editor.tab_width as u32,
        softtabstop: config.editor.tab_width as u32,
        ..Options::default()
    };
    if let Some(ref p) = path {
        crate::editorconfig::overlay_for_path(&mut ec_opts, p);
    }
    let mut editor = Editor::new(buffer, host, ec_opts);
    if let Ok(size) = crossterm::terminal::size() {
        let vp = editor.host_mut().viewport_mut();
        vp.width = size.0;
        vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
    }
    // Non-blocking: returns immediately; Loading case is handled by
    // poll_grammar_loads each tick.
    if let Some(ref p) = path {
        let outcome = syntax.set_language_for_path(buffer_id, p);
        let _ = outcome; // Outcome handled via poll_grammar_loads for Loading.
    }

    let (vp_top, vp_height) = {
        let vp = editor.host().viewport();
        (vp.top_row, vp.height as usize)
    };
    if let Some(out) = syntax.preview_render(buffer_id, editor.buffer(), vp_top, vp_height) {
        editor.install_ratatui_syntax_spans(out.spans);
    }
    syntax.submit_render(buffer_id, editor.buffer(), vp_top, vp_height);
    let initial_dg = editor.buffer().dirty_gen();
    let (key, signs) = if let Some(out) = syntax.wait_for_initial_result(Duration::from_millis(150))
    {
        let k = out.key;
        editor.install_ratatui_syntax_spans(out.spans);
        (Some(k), out.signs)
    } else {
        (Some((initial_dg, vp_top, vp_height)), Vec::new())
    };
    let _ = editor.take_content_edits();
    let _ = editor.take_content_reset();

    let mut slot = BufferSlot {
        buffer_id,
        editor,
        filename: path,
        dirty: false,
        is_new_file,
        is_untracked: false,
        diag_signs: signs,
        diag_signs_lsp: Vec::new(),
        lsp_diags: Vec::new(),
        last_lsp_dirty_gen: None,
        git_signs: Vec::new(),
        last_git_dirty_gen: None,
        last_git_refresh_at: Instant::now(),
        last_recompute_at: Instant::now() - Duration::from_secs(1),
        last_recompute_key: key,
        saved_hash: 0,
        saved_len: 0,
        disk_mtime,
        disk_len,
        disk_state: DiskState::Synced,
    };
    slot.snapshot_saved();
    Ok(slot)
}

impl App {
    // ── Tab accessors ──────────────────────────────────────────────────────

    /// Shared reference to the active tab's layout tree.
    pub fn layout(&self) -> &window::LayoutTree {
        &self.tabs[self.active_tab].layout
    }

    /// Mutable reference to the active tab's layout tree.
    pub fn layout_mut(&mut self) -> &mut window::LayoutTree {
        &mut self.tabs[self.active_tab].layout
    }

    /// The `WindowId` that has focus in the active tab.
    pub fn focused_window(&self) -> window::WindowId {
        self.tabs[self.active_tab].focused_window
    }

    /// Set the focused window in the active tab.
    pub fn set_focused_window(&mut self, id: window::WindowId) {
        self.tabs[self.active_tab].focused_window = id;
    }

    /// Temporarily take the active tab's layout, replacing it with a
    /// sentinel, so we can pass `&mut LayoutTree` to the renderer while
    /// still holding `&mut App`.
    pub fn take_layout(&mut self) -> window::LayoutTree {
        std::mem::replace(self.layout_mut(), window::LayoutTree::Leaf(usize::MAX))
    }

    /// Restore the layout after a [`take_layout`] call.
    pub fn restore_layout(&mut self, layout: window::LayoutTree) {
        *self.layout_mut() = layout;
    }

    // ── Core helpers ──────────────────────────────────────────────────────

    /// Slot index for the focused window.
    fn focused_slot_idx(&self) -> usize {
        self.windows[self.focused_window()]
            .as_ref()
            .expect("focused_window must point to an open window")
            .slot
    }

    /// Return a shared reference to the active buffer slot.
    pub fn active(&self) -> &BufferSlot {
        &self.slots[self.focused_slot_idx()]
    }

    /// Return a mutable reference to the active buffer slot.
    pub fn active_mut(&mut self) -> &mut BufferSlot {
        let slot_idx = self.focused_slot_idx();
        &mut self.slots[slot_idx]
    }

    /// The name of the grammar currently being loaded for the active buffer,
    /// if any. Used by the renderer to show the `loading grammar: <name>…`
    /// status-line indicator.
    pub fn pending_grammar_name_for_active(&self) -> Option<&str> {
        let id = self.slots[self.focused_slot_idx()].buffer_id;
        self.syntax.pending_load_name_for(id)
    }

    /// Return a shared slice of all buffer slots.
    pub fn slots(&self) -> &[BufferSlot] {
        &self.slots
    }

    /// Return a mutable slice of all buffer slots. Used by the renderer to
    /// publish viewport dimensions and set cursor positions per-window.
    pub fn slots_mut(&mut self) -> &mut [BufferSlot] {
        &mut self.slots
    }

    /// Return the slot index of the currently focused window (used by
    /// the buffer-line renderer to highlight the active buffer tab).
    pub fn active_index(&self) -> usize {
        self.focused_slot_idx()
    }

    // ── Viewport sync ─────────────────────────────────────────────────────

    /// Copy the focused window's stored scroll position into the active
    /// editor's host viewport. Call BEFORE input dispatch so the engine's
    /// scroll math starts from the right offset.
    pub fn sync_viewport_to_editor(&mut self) {
        let fw = self.focused_window();
        let win = self.windows[fw].as_ref().expect("focused_window open");
        let (top_row, top_col) = (win.top_row, win.top_col);
        let maybe_rect = win.last_rect;
        if let Some(rect) = maybe_rect {
            let vp = self.active_mut().editor.host_mut().viewport_mut();
            vp.top_row = top_row;
            vp.top_col = top_col;
            vp.width = rect.width;
            vp.height = rect.height;
        }
    }

    /// Copy the active editor's host viewport scroll state back into the
    /// focused window. Call AFTER input dispatch so the engine's
    /// auto-scroll updates are persisted.
    pub fn sync_viewport_from_editor(&mut self) {
        let vp = self.active().editor.host().viewport();
        let (top_row, top_col) = (vp.top_row, vp.top_col);
        let fw = self.focused_window();
        let win = self.windows[fw].as_mut().expect("focused_window open");
        win.top_row = top_row;
        win.top_col = top_col;
    }

    // ── Window focus navigation ───────────────────────────────────────────

    /// Move focus to the window below the current one (`Ctrl-w j`).
    pub fn focus_below(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_below(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Move focus to the window above the current one (`Ctrl-w k`).
    pub fn focus_above(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_above(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Move focus to the window left of the current one (`Ctrl-w h`).
    pub fn focus_left(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_left(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Move focus to the window right of the current one (`Ctrl-w l`).
    pub fn focus_right(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_right(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Move focus to the next window in pre-order traversal, wrapping around (`Ctrl-w w`).
    pub fn focus_next(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().next_leaf(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Move focus to the previous window in pre-order traversal, wrapping around (`Ctrl-w W`).
    pub fn focus_previous(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().prev_leaf(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Close all windows except the focused one. Replaces the layout with a
    /// single leaf and drops the `Option<Window>` entries for all other windows.
    pub fn only_focused_window(&mut self) {
        let focused = self.focused_window();
        let all_leaves = self.layout().leaves();
        for id in all_leaves {
            if id != focused {
                self.windows[id] = None;
            }
        }
        *self.layout_mut() = window::LayoutTree::Leaf(focused);
        self.status_message = Some("only".into());
    }

    /// Swap the focused leaf with its sibling in the immediately enclosing
    /// Split. No-op (with no message) when the focused window is the only one.
    pub fn swap_with_sibling(&mut self) {
        let focused = self.focused_window();
        if self.layout_mut().swap_with_sibling(focused) {
            self.status_message = Some("swap".into());
        }
    }

    /// Move the focused window to a new tab (`Ctrl-w T`).
    ///
    /// Fails if the current tab has only one window (vim's "E1: at last window").
    /// On success: the window is removed from the current tab's layout (the
    /// previous tab gets focus on its new top leaf), and a new tab is appended
    /// containing only the moved window.
    pub fn move_window_to_new_tab(&mut self) -> Result<(), &'static str> {
        let focused = self.focused_window();
        if self.layout().leaves().len() <= 1 {
            return Err("E1: only one window in this tab");
        }
        self.sync_viewport_from_editor();
        // Remove the focused leaf from the current tab's layout. The returned
        // value is the leaf that should receive focus in the current tab.
        let new_focus_in_old_tab = self
            .layout_mut()
            .remove_leaf(focused)
            .map_err(|_| "remove_leaf failed")?;
        // Update the old tab's focused window to the surviving sibling.
        self.tabs[self.active_tab].focused_window = new_focus_in_old_tab;

        // Create a new tab containing only the moved window.
        let new_tab = window::Tab {
            layout: window::LayoutTree::Leaf(focused),
            focused_window: focused,
        };
        self.tabs.push(new_tab);
        self.active_tab = self.tabs.len() - 1;
        self.sync_viewport_to_editor();
        Ok(())
    }

    /// Close the focused window.  Fails (with status message) when only one
    /// window remains.  On success the layout collapses and focus moves to the
    /// sibling that took over.
    pub fn close_focused_window(&mut self) {
        let focused = self.focused_window();
        match self.layout_mut().remove_leaf(focused) {
            Err(_) => {
                self.status_message = Some("E444: Cannot close last window".into());
            }
            Ok(new_focus) => {
                self.windows[focused] = None;
                self.set_focused_window(new_focus);
                self.sync_viewport_to_editor();
                self.status_message = Some("window closed".into());
            }
        }
    }

    // ── Window size manipulation ───────────────────────────────────────────

    /// Adjust the focused window's height by `delta` lines. Positive grows,
    /// negative shrinks. Clamps so neither sibling drops below 1 line.
    /// No-op when there is no enclosing Horizontal split or last_rect is None.
    pub fn resize_height(&mut self, delta: i32) {
        use window::SplitDir;
        let fw = self.focused_window();
        if let Some((ratio, Some(rect), in_a)) = self
            .layout_mut()
            .enclosing_split_mut(fw, SplitDir::Horizontal)
        {
            let parent_h = rect.height as i32;
            if parent_h < 2 {
                return;
            }
            let current_focused_height = if in_a {
                (parent_h as f32 * *ratio) as i32
            } else {
                (parent_h as f32 * (1.0 - *ratio)) as i32
            };
            let new_focused = (current_focused_height + delta).clamp(1, parent_h - 1);
            let new_ratio = if in_a {
                new_focused as f32 / parent_h as f32
            } else {
                (parent_h - new_focused) as f32 / parent_h as f32
            };
            *ratio = new_ratio.clamp(0.01, 0.99);
        }
    }

    /// Adjust the focused window's width by `delta` columns. Positive grows,
    /// negative shrinks. Clamps so neither sibling drops below 1 column.
    /// No-op when there is no enclosing Vertical split or last_rect is None.
    pub fn resize_width(&mut self, delta: i32) {
        use window::SplitDir;
        let fw = self.focused_window();
        if let Some((ratio, Some(rect), in_a)) = self
            .layout_mut()
            .enclosing_split_mut(fw, SplitDir::Vertical)
        {
            let parent_w = rect.width as i32;
            if parent_w < 2 {
                return;
            }
            let current_focused_width = if in_a {
                (parent_w as f32 * *ratio) as i32
            } else {
                (parent_w as f32 * (1.0 - *ratio)) as i32
            };
            let new_focused = (current_focused_width + delta).clamp(1, parent_w - 1);
            let new_ratio = if in_a {
                new_focused as f32 / parent_w as f32
            } else {
                (parent_w - new_focused) as f32 / parent_w as f32
            };
            *ratio = new_ratio.clamp(0.01, 0.99);
        }
    }

    /// Equalize all splits to 0.5 ratio.
    pub fn equalize_layout(&mut self) {
        self.layout_mut().equalize_all();
    }

    /// Maximize focused window's height — set every enclosing Horizontal
    /// split so the focused branch gets as much height as possible (siblings
    /// collapse to 1 line each).
    pub fn maximize_height(&mut self) {
        use window::SplitDir;
        let focused = self.focused_window();
        self.layout_mut()
            .for_each_ancestor(focused, &mut |dir, ratio, in_a, rect| {
                if dir != SplitDir::Horizontal {
                    return;
                }
                if let Some(r) = rect {
                    let h = r.height as f32;
                    if h < 2.0 {
                        return;
                    }
                    let max_branch = (h - 1.0) / h;
                    let min_branch = 1.0 / h;
                    *ratio = if in_a { max_branch } else { min_branch };
                }
            });
    }

    /// Maximize focused window's width — set every enclosing Vertical split
    /// so the focused branch gets as much width as possible (siblings collapse
    /// to 1 column each).
    pub fn maximize_width(&mut self) {
        use window::SplitDir;
        let focused = self.focused_window();
        self.layout_mut()
            .for_each_ancestor(focused, &mut |dir, ratio, in_a, rect| {
                if dir != SplitDir::Vertical {
                    return;
                }
                if let Some(r) = rect {
                    let w = r.width as f32;
                    if w < 2.0 {
                        return;
                    }
                    let max_branch = (w - 1.0) / w;
                    let min_branch = 1.0 / w;
                    *ratio = if in_a { max_branch } else { min_branch };
                }
            });
    }

    /// Build a fresh [`App`], optionally loading `filename` from disk.
    ///
    /// - File found → content seeded into buffer, dirty = false.
    /// - File not found → buffer empty, filename retained, `is_new_file = true`.
    /// - Other I/O error → returns `Err` so main can print to stderr before
    ///   entering alternate-screen mode.
    ///
    /// `readonly` sets `:set readonly` on the editor options.
    /// `goto_line` (1-based) moves the cursor after load when `Some`.
    /// `search_pattern` triggers an initial search when `Some`.
    pub fn new(
        filename: Option<PathBuf>,
        readonly: bool,
        goto_line: Option<usize>,
        search_pattern: Option<String>,
    ) -> Result<Self> {
        // Load the app theme up front and build the syntax layer with the
        // override theme — so apps/hjkl renders with the website palette
        // (hjkl-bonsai's bundled DotFallbackTheme is left untouched
        // for other consumers).
        let theme = crate::theme::AppTheme::default_dark();
        let directory = std::sync::Arc::new(crate::lang::LanguageDirectory::new()?);
        let mut syntax = syntax::layer_with_theme(theme.syntax.clone(), directory.clone());
        let buffer_id: BufferId = 0;
        // App::new uses bundled config defaults; main wires the XDG-merged
        // value via `with_config` after construction. For build_slot's
        // initial Options seed, the bundled defaults are correct because
        // tests never customize config and main re-applies overrides via
        // `apply_options` after `with_config`.
        let bootstrap_config = crate::config::Config::default();
        let no_file = filename.is_none();
        let mut slot = build_slot(&mut syntax, buffer_id, filename, &bootstrap_config)
            .map_err(|s| anyhow::anyhow!(s))?;

        // Apply readonly after the slot is built — build_slot always uses
        // Options::default(); override here when requested.
        if readonly {
            slot.editor.apply_options(&Options {
                readonly: true,
                ..Options::default()
            });
        }

        // +N line jump — 1-based, clamp to buffer.
        if let Some(n) = goto_line {
            slot.editor.goto_line(n);
        }

        // +/pattern initial search — compile the pattern and set it.
        if let Some(pat) = search_pattern {
            match regex::Regex::new(&pat) {
                Ok(re) => {
                    slot.editor.set_search_pattern(Some(re));
                    slot.editor.search_advance_forward(false);
                }
                Err(e) => {
                    eprintln!("hjkl: bad search pattern: {e}");
                }
            }
        }

        let start_screen = if no_file {
            Some(crate::start_screen::StartScreen::new())
        } else {
            None
        };

        // Single window pointing at slot 0.
        let initial_window = window::Window {
            slot: 0,
            top_row: 0,
            top_col: 0,
            last_rect: None,
        };

        Ok(Self {
            slots: vec![slot],
            windows: vec![Some(initial_window)],
            tabs: vec![window::Tab {
                layout: window::LayoutTree::Leaf(0),
                focused_window: 0,
            }],
            active_tab: 0,
            next_window_id: 1,
            pending_window_motion: false,
            next_buffer_id: 1,
            prev_active: None,
            exit_requested: false,
            status_message: None,
            info_popup: None,
            command_field: None,
            search_field: None,
            picker: None,
            pending_leader: false,
            pending_git: false,
            pending_buffer_motion: None,
            search_dir: SearchDir::Forward,
            last_cursor_shape: CursorShape::Block,
            syntax,
            directory,
            theme,
            perf_overlay: false,
            last_recompute_us: 0,
            last_install_us: 0,
            last_signature_us: 0,
            last_git_us: 0,
            last_perf: crate::syntax::PerfBreakdown::default(),
            recompute_hits: 0,
            recompute_throttled: 0,
            recompute_runs: 0,
            config: crate::config::Config::default(),
            start_screen,
            grammar_load_error: None,
            lsp: None,
            lsp_state: HashMap::new(),
            lsp_next_request_id: 0,
            lsp_pending: HashMap::new(),
        })
    }

    /// Replace the user config (typically loaded by `main` from the XDG
    /// path or `--config <PATH>`) and re-apply config-derived
    /// [`Options`] to every already-open slot.
    ///
    /// `App::new` constructs slot 0 with bootstrap defaults before any
    /// user config is wired, so without this re-application a user
    /// override of `editor.tab_width` / `editor.expandtab` would only
    /// affect *subsequent* slots (`:e`, `open_extra`). The re-applied
    /// `Options` seed is overlaid by `.editorconfig` per-path so project
    /// rules still take precedence over user-config fallbacks.
    ///
    /// Readonly state on each slot is preserved.
    pub fn with_config(mut self, config: crate::config::Config) -> Self {
        self.config = config;
        for slot in &mut self.slots {
            let was_readonly = slot.editor.is_readonly();
            let mut opts = Options {
                expandtab: self.config.editor.expandtab,
                tabstop: self.config.editor.tab_width as u32,
                shiftwidth: self.config.editor.tab_width as u32,
                softtabstop: self.config.editor.tab_width as u32,
                readonly: was_readonly,
                ..Options::default()
            };
            if let Some(p) = slot.filename.as_ref() {
                crate::editorconfig::overlay_for_path(&mut opts, p);
            }
            slot.editor.apply_options(&opts);
        }
        self
    }

    /// Attach an `LspManager` to the app. Call after `with_config`.
    pub fn with_lsp(mut self, lsp: hjkl_lsp::LspManager) -> Self {
        self.lsp = Some(lsp);
        self
    }

    /// Mode label for the status line.
    pub fn mode_label(&self) -> &'static str {
        if self.start_screen.is_some() {
            return "START";
        }
        match self.active().editor.vim_mode() {
            VimMode::Normal => "NORMAL",
            VimMode::Insert => "INSERT",
            VimMode::Visual => "VISUAL",
            VimMode::VisualLine => "VISUAL LINE",
            VimMode::VisualBlock => "VISUAL BLOCK",
        }
    }

    /// Public entry point for loading an extra file from the CLI into a new
    /// slot without switching the active buffer. Used by `main` to handle
    /// `hjkl a.rs b.rs c.rs` — slots 1…N are populated here after `App::new`
    /// opens slot 0.
    pub fn open_extra(&mut self, path: PathBuf) -> Result<(), String> {
        self.open_new_slot(path).map(|_| ())
    }
}
