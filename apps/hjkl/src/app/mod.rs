//! `App` — owns the editor + host, drives the event loop.

use anyhow::Result;
use hjkl_buffer::Buffer;
use hjkl_engine::{BufferEdit, Host};
use hjkl_engine::{CursorShape, Editor, Options, VimMode};
use hjkl_form::TextFieldEditor;
use hjkl_keymap::Keymap;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crate::keymap_actions::AppAction;

use crate::git_worker::GitSignsWorker;
use crate::host::TuiHost;
use crate::syntax::{self, BufferId, SyntaxLayer};

mod buffer_ops;
mod event_loop;
mod ex_dispatch;
pub(crate) mod ex_host_cmds;
pub(crate) mod keymap;
pub mod lsp_glue;
pub(crate) mod mappings_dispatch;
mod picker_glue;
mod prompt;
mod syntax_glue;
#[cfg(test)]
mod tests;
pub mod window;

use crate::completion::Completion;

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
    Completion {
        buffer_id: hjkl_lsp::BufferId,
        /// 0-based cursor position when the request was sent.
        anchor_row: usize,
        anchor_col: usize,
    },
    /// `textDocument/codeAction` — Phase 5.
    CodeAction {
        buffer_id: hjkl_lsp::BufferId,
        anchor_row: usize,
        anchor_col: usize,
    },
    /// `textDocument/rename` — Phase 5.
    Rename {
        buffer_id: hjkl_lsp::BufferId,
        anchor_row: usize,
        anchor_col: usize,
        new_name: String,
    },
    /// `textDocument/formatting` — Phase 5.
    Format {
        buffer_id: hjkl_lsp::BufferId,
        /// `None` = full document; `Some((sr, sc, er, ec))` = range (Phase 5 always None).
        range: Option<(usize, usize, usize, usize)>,
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
    /// Active wildmenu state for the command-line prompt. `None` outside
    /// completion (no Tab pressed yet, or after acceptance/cancel).
    pub(crate) command_completion: Option<crate::app::prompt::CommandCompletion>,
    /// Active `/` (forward) / `?` (backward) search prompt.
    pub search_field: Option<TextFieldEditor>,
    /// Active picker overlay (file, buffer, grep, …).
    pub picker: Option<crate::picker::Picker>,
    /// Buffered digit-prefix count for an app-level count prefix (e.g. `5` in
    /// `5gt`). Accumulated in Normal mode when no chord prefix is active.
    /// Digits are replayed to the engine when the non-digit key is
    /// engine-handled, or consumed when the key is app-handled.
    pub pending_count: hjkl_vim::CountAccumulator,
    /// Direction of the active `search_field`.
    pub search_dir: SearchDir,
    /// Last cursor shape we emitted to the terminal.
    last_cursor_shape: CursorShape,
    /// Tree-sitter syntax highlighting layer. Owns the worker thread + the
    /// active theme. Multiplexed by BufferId.
    syntax: SyntaxLayer,
    /// Background worker for git diff-sign computation.
    git_worker: GitSignsWorker,
    /// Shared grammar resolver. `Arc` so the syntax layer and every picker
    /// source point at the same in-memory `Grammar` cache (one dlopen +
    /// query parse per language, app-wide).
    pub directory: std::sync::Arc<crate::lang::LanguageDirectory>,
    /// App-wide theme (UI chrome + syntax). Loaded once at startup from
    /// `themes/{ui,syntax}-dark.toml` baked via include_str!.
    pub theme: crate::theme::AppTheme,
    /// Per-language `Highlighter` cache used by the picker preview pane
    /// (computed via [`Self::preview_spans_for`]). Centralised here so
    /// every preview source — files, rg results, open buffers, git diff
    /// rows — shares one parser per language for the session. The
    /// editor's own syntax pipeline lives on `syntax`; this is for the
    /// preview-only highlight path.
    pub(crate) preview_highlighters:
        std::sync::Mutex<std::collections::HashMap<String, hjkl_bonsai::Highlighter>>,
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
    /// Active completion popup, if any.
    pub completion: Option<Completion>,
    /// Code actions from the most recent `textDocument/codeAction` response.
    /// The picker uses `ApplyCodeAction(i)` to index into this list.
    pub pending_code_actions: Vec<lsp_types::CodeActionOrCommand>,
    /// Tracks the first key of the `<C-x><C-o>` omni-completion chord.
    /// Set to `true` after `Ctrl-x`; cleared after the next key.
    pub pending_ctrl_x: bool,
    /// Monotonic instant at which the current prefix was set.
    /// `None` when no prefix is pending.
    pub pending_prefix_at: Option<std::time::Instant>,
    /// `true` when the which-key idle timeout has expired and the popup
    /// should be rendered.
    pub which_key_active: bool,
    /// `true` when the which-key popup is sticky-visible after a Backspace
    /// emptied the chord buffer. Stays open showing root entries until any
    /// non-Backspace key is pressed.
    pub(crate) which_key_sticky: bool,
    /// Whether the which-key feature is enabled (from config).
    pub which_key_enabled: bool,
    /// Idle delay before the which-key popup appears (from config).
    pub which_key_delay: std::time::Duration,
    /// Side-table of user-registered runtime key maps (for `:map` listing).
    /// The trie `app_keymap` owns the actual dispatch; this records what was
    /// registered so listing commands don't expose built-in bindings.
    pub(crate) user_keymap_records: Vec<keymap::UserKeymapRecord>,
    /// Active recursion depth of `AppAction::Replay { recursive: true }`
    /// dispatches. Used to bail out of cyclic user maps (`:nmap a a`) before
    /// stack overflow. The per-Replay-frame `steps` counter only catches
    /// horizontal cycles; this catches vertical (re-entrant) cycles too.
    pub(crate) replay_depth: usize,
    /// Mouse-capture state. Mirrors the terminal's
    /// EnableMouseCapture / DisableMouseCapture mode. Initialised from
    /// `config.editor.mouse`; runtime-togglable via `:set [no]mouse`.
    /// When false, wheel events fall through to the terminal as
    /// synthesised arrow keys.
    pub mouse_enabled: bool,
    /// Application-level chord dispatch. Holds Normal-mode bindings for all
    /// leader / g / ] / [ / <C-w> sequences.
    pub(crate) app_keymap: Keymap<AppAction, keymap::HjklMode>,
    /// Background install worker pool shared across all `:Anvil install` calls.
    pub anvil_pool: hjkl_anvil::InstallPool,
    /// In-flight install handles keyed by tool name.
    pub anvil_handles: HashMap<String, hjkl_anvil::InstallHandle>,
    /// Per-tool install log lines accumulated from status messages.
    pub anvil_log: HashMap<String, Vec<String>>,
    /// Embedded anvil tool registry (built once at startup from the baked-in
    /// `anvil.toml`; `None` only when the embedded catalog fails to parse).
    pub anvil_registry: Option<hjkl_anvil::Registry>,
    /// App-level pending chord state. `Some` while a second-key chord (e.g.
    /// `r<x>`) is in flight and being driven by `hjkl_vim::step`. Cleared
    /// when the reducer emits `Commit` or `Cancel`. When `Some`, the event
    /// loop routes the next key through `hjkl_vim::step` instead of the trie.
    pub(crate) pending_state: Option<hjkl_vim::PendingState>,
    /// Last successfully-dispatched ex command (text body, no leading `:`),
    /// used by `@:` to repeat. Phase 5d of kryptic-sh/hjkl#71.
    pub(crate) last_ex_command: Option<String>,
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
        let viewport_height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
        let vp = editor.host_mut().viewport_mut();
        vp.width = size.0;
        vp.height = viewport_height;
        // Publish the viewport height to the engine's atomic so any
        // pre-event-loop scroll math (e.g. ensure_cursor_in_scrolloff
        // after a +/pat startup search) takes the scrolloff path
        // instead of the no-margin fallback.
        editor.set_viewport_height(viewport_height);
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

/// Build the Normal-mode application keymap for the given leader character.
///
/// Every app-handled chord binding is registered here. The resulting
/// `Keymap<AppAction, keymap::HjklMode>` is stored on [`App`] and consulted by the event loop
/// before forwarding keys to the editor engine.
fn build_app_keymap(leader: char) -> Keymap<AppAction, keymap::HjklMode> {
    use keymap::HjklMode as Mode;
    let mut km = Keymap::new(leader);
    // Timeout matches the which-key delay default; overridden by `with_config`.
    km.set_timeout(Duration::from_millis(500));

    let bindings: &[(&str, AppAction, &str)] = &[
        // ── File / buffer / grep pickers ──────────────────────────────────
        ("<leader><leader>", AppAction::OpenFilePicker, "file picker"),
        ("<leader>f", AppAction::OpenFilePicker, "file picker"),
        ("<leader>b", AppAction::OpenBufferPicker, "buffer picker"),
        ("<leader>/", AppAction::OpenGrepPicker, "grep picker"),
        // ── Git sub-commands ───────────────────────────────────────────────
        ("<leader>gs", AppAction::GitStatus, "git status"),
        ("<leader>gl", AppAction::GitLog, "git log"),
        ("<leader>gb", AppAction::GitBranch, "git branches"),
        ("<leader>gB", AppAction::GitFileHistory, "git file history"),
        ("<leader>gS", AppAction::GitStashes, "git stashes"),
        ("<leader>gt", AppAction::GitTags, "git tags"),
        ("<leader>gr", AppAction::GitRemotes, "git remotes"),
        // ── LSP / diagnostics ─────────────────────────────────────────────
        ("<leader>d", AppAction::ShowDiagAtCursor, "show diagnostic"),
        ("<leader>ca", AppAction::LspCodeActions, "code actions"),
        ("<leader>rn", AppAction::LspRename, "rename symbol"),
        // ── g-prefix ──────────────────────────────────────────────────────
        // NOTE: bare `g` is bound separately below as BeginPendingAfterG.
        // The app-level g-chord actions (gt, gd, etc.) are dispatched from
        // the AfterGChord arm in event_loop.rs rather than the trie, so
        // that a bare `g` can immediately set pending state without waiting
        // for the trie timeout (Ambiguous resolution).
        // ── ] / [ bracket motions ─────────────────────────────────────────
        ("]b", AppAction::BufferNext, "next buffer"),
        ("[b", AppAction::BufferPrev, "prev buffer"),
        ("]d", AppAction::DiagNext, "next diagnostic"),
        ("[d", AppAction::DiagPrev, "prev diagnostic"),
        ("]D", AppAction::DiagNextError, "next error"),
        ("[D", AppAction::DiagPrevError, "prev error"),
        // ── <C-w> window motions ──────────────────────────────────────────
        ("<C-w>h", AppAction::FocusLeft, "focus left"),
        ("<C-w>j", AppAction::FocusBelow, "focus down"),
        ("<C-w>k", AppAction::FocusAbove, "focus up"),
        ("<C-w>l", AppAction::FocusRight, "focus right"),
        ("<C-w>w", AppAction::FocusNext, "focus next"),
        ("<C-w>W", AppAction::FocusPrev, "focus prev"),
        ("<C-w>c", AppAction::CloseFocusedWindow, "close window"),
        ("<C-w>q", AppAction::QuitOrClose, "quit/close"),
        ("<C-w>o", AppAction::OnlyFocusedWindow, "close others"),
        ("<C-w>x", AppAction::SwapWithSibling, "swap with sibling"),
        ("<C-w>r", AppAction::SwapWithSibling, "swap with sibling"),
        ("<C-w>R", AppAction::SwapWithSibling, "swap with sibling"),
        ("<C-w>T", AppAction::MoveWindowToNewTab, "move to new tab"),
        ("<C-w>n", AppAction::NewSplit, "new split"),
        ("<C-w>+", AppAction::ResizeHeight(1), "taller"),
        ("<C-w>-", AppAction::ResizeHeight(-1), "shorter"),
        ("<C-w><gt>", AppAction::ResizeWidth(1), "wider"),
        ("<C-w><lt>", AppAction::ResizeWidth(-1), "narrower"),
        ("<C-w>=", AppAction::EqualizeLayout, "equalize"),
        ("<C-w>_", AppAction::MaximizeHeight, "maximize height"),
        ("<C-w>|", AppAction::MaximizeWidth, "maximize width"),
    ];

    for (chord_str, action, desc) in bindings {
        if let Err(e) = km.add(Mode::Normal, chord_str, action.clone(), desc) {
            // Should never fail with our static strings, but log rather than panic.
            eprintln!("hjkl: keymap.add({chord_str:?}) failed: {e}");
        }
    }

    // ── pending-state chords ───────────────────────────────────────────────
    // `r<x>` — begin Replace pending state. Bound in both Normal and Visual so
    // the trie intercepts `r` before the engine FSM sees it.
    let replace_action = AppAction::BeginPendingReplace { count: 1 };
    for mode in [Mode::Normal, Mode::Visual] {
        if let Err(e) = km.add(mode, "r", replace_action.clone(), "replace char") {
            eprintln!("hjkl: keymap.add(r) failed: {e}");
        }
    }

    // `f<x>` / `F<x>` / `t<x>` / `T<x>` — bare find chords, migrated to
    // hjkl-vim's PendingState::Find reducer. Bound in Normal and Visual only.
    // Operator-pending find (`df<x>`, etc.) still goes through the engine FSM.
    for (key, forward, till, desc) in [
        ("f", true, false, "find char forward"),
        ("F", false, false, "find char backward"),
        ("t", true, true, "till char forward"),
        ("T", false, true, "till char backward"),
    ] {
        let action = AppAction::BeginPendingFind {
            forward,
            till,
            count: 1,
        };
        for mode in [Mode::Normal, Mode::Visual] {
            if let Err(e) = km.add(mode, key, action.clone(), desc) {
                eprintln!("hjkl: keymap.add({key}) failed: {e}");
            }
        }
    }

    // `g<x>` — bare g-prefix chord, migrated to hjkl-vim's
    // PendingState::AfterG reducer. Bound in Normal + all three visual
    // modes so `gg` (and other g-chords) work consistently in
    // visual/visual-line/visual-block. Operator-pending g (`dgU`, etc.)
    // and the engine's internal `Pending::G` / `Pending::OpG` still go
    // through the engine FSM.
    let after_g_action = AppAction::BeginPendingAfterG { count: 1 };
    for mode in [
        Mode::Normal,
        Mode::Visual,
        Mode::VisualLine,
        Mode::VisualBlock,
    ] {
        if let Err(e) = km.add(mode, "g", after_g_action.clone(), "g-prefix chord") {
            eprintln!("hjkl: keymap.add(g) failed: {e}");
        }
    }

    // `z<x>` — bare z-prefix chord, migrated to hjkl-vim's
    // PendingState::AfterZ reducer. Bound in Normal + all three visual
    // modes for parity with `g`. Operator-pending z (`zf{motion}`) and
    // the engine's internal `Pending::Z` still go through the engine
    // FSM for non-visual `zf`.
    let after_z_action = AppAction::BeginPendingAfterZ { count: 1 };
    for mode in [
        Mode::Normal,
        Mode::Visual,
        Mode::VisualLine,
        Mode::VisualBlock,
    ] {
        if let Err(e) = km.add(mode, "z", after_z_action.clone(), "z-prefix chord") {
            eprintln!("hjkl: keymap.add(z) failed: {e}");
        }
    }

    // `d` / `y` / `c` / `>` / `<` — bare op-pending entry from Normal mode,
    // migrated to hjkl-vim's PendingState::AfterOp reducer. Bound in Normal
    // mode only. Visual-mode `d`/`y`/`c`/`>`/`<` execute inline through the
    // engine FSM and are NOT intercepted here.
    //
    // The `>` and `<` chars need quoting in the chord string per hjkl-keymap
    // notation (`<gt>` and `<lt>`).
    for (key, op, desc) in [
        ("d", hjkl_vim::OperatorKind::Delete, "delete operator"),
        ("y", hjkl_vim::OperatorKind::Yank, "yank operator"),
        ("c", hjkl_vim::OperatorKind::Change, "change operator"),
        ("<gt>", hjkl_vim::OperatorKind::Indent, "indent operator"),
        ("<lt>", hjkl_vim::OperatorKind::Outdent, "outdent operator"),
    ] {
        let action = AppAction::BeginPendingAfterOp { op, count1: 1 };
        if let Err(e) = km.add(Mode::Normal, key, action, desc) {
            eprintln!("hjkl: keymap.add({key}) failed: {e}");
        }
    }

    // Visual-mode operators — fire inline against the current selection.
    // `d` / `y` / `c` / `>` / `<` bound in HjklMode::Visual (covers Visual,
    // VisualLine, and VisualBlock per the mode-collapse in keymap.rs:125).
    //
    // All three modes (Visual, VisualLine, VisualBlock) route through the
    // public range-mutation primitives. Phase 4e follow-ups closed the gaps:
    //   - pending_register() getter exposed (Visual register honors "a prefix)
    //   - run_operator_over_range linewise guard fixed (VisualLine single-row)
    //   - delete_block/yank_block/change_block/indent_block exposed (VisualBlock)
    for (key, op, desc) in [
        ("d", hjkl_vim::OperatorKind::Delete, "delete selection"),
        ("y", hjkl_vim::OperatorKind::Yank, "yank selection"),
        ("c", hjkl_vim::OperatorKind::Change, "change selection"),
        ("<gt>", hjkl_vim::OperatorKind::Indent, "indent selection"),
        ("<lt>", hjkl_vim::OperatorKind::Outdent, "outdent selection"),
    ] {
        let action = AppAction::VisualOp { op, count: 1 };
        if let Err(e) = km.add(Mode::Visual, key, action, desc) {
            eprintln!("hjkl: keymap.add({key} Visual) failed: {e}");
        }
    }

    // `"<reg>` — register-prefix chord in Normal mode only. Visual-mode `"`
    // is not intercepted here; the engine FSM handles any Visual-mode `"`
    // input directly (there is no visual-register-select path in the engine).
    // Bound Normal-only, matching how vim treats `"` in Normal vs Visual mode.
    if let Err(e) = km.add(
        Mode::Normal,
        "\"",
        AppAction::BeginPendingSelectRegister,
        "register-prefix chord",
    ) {
        eprintln!("hjkl: keymap.add(\\\") failed: {e}");
    }

    // `m<x>` — mark-set chord. Normal mode only (vim's `m` is not meaningful
    // in Visual mode). The engine FSM arms for `m` in Normal mode are kept
    // intact for macro-replay defensive coverage (deletion in Phase 6).
    if let Err(e) = km.add(
        Mode::Normal,
        "m",
        AppAction::BeginPendingSetMark,
        "set mark chord",
    ) {
        eprintln!("hjkl: keymap.add(m) failed: {e}");
    }

    // `'<x>` — mark-goto-line chord. Normal mode only.
    if let Err(e) = km.add(
        Mode::Normal,
        "'",
        AppAction::BeginPendingGotoMarkLine,
        "goto mark linewise chord",
    ) {
        eprintln!("hjkl: keymap.add(') failed: {e}");
    }

    // `` `<x> `` — mark-goto-char chord. Normal + all three Visual modes.
    // In Visual mode, `` ` `` jumps the cursor to a mark charwise while keeping
    // the selection active (matches engine's pre-existing vim.rs:2058-2066
    // behaviour). The engine FSM arms for `` ` `` are kept for macro-replay.
    for mode in [
        Mode::Normal,
        Mode::Visual,
        Mode::VisualLine,
        Mode::VisualBlock,
    ] {
        if let Err(e) = km.add(
            mode,
            "`",
            AppAction::BeginPendingGotoMarkChar,
            "goto mark charwise chord",
        ) {
            eprintln!("hjkl: keymap.add(`) failed: {e}");
        }
    }

    // ── Phase 3a: char + line motions via hjkl-vim keymap path ───────────
    // Bound in Normal, Visual, VisualLine, and VisualBlock. Engine FSM arms
    // for these keys are kept intact for macro-replay defensive coverage.
    for (chord, kind, desc) in [
        ("h", hjkl_vim::MotionKind::CharLeft, "char left"),
        ("<BS>", hjkl_vim::MotionKind::CharLeft, "char left"),
        ("l", hjkl_vim::MotionKind::CharRight, "char right"),
        ("<Space>", hjkl_vim::MotionKind::CharRight, "char right"),
        ("j", hjkl_vim::MotionKind::LineDown, "line down"),
        ("k", hjkl_vim::MotionKind::LineUp, "line up"),
        (
            "+",
            hjkl_vim::MotionKind::FirstNonBlankDown,
            "next line first non-blank",
        ),
        (
            "-",
            hjkl_vim::MotionKind::FirstNonBlankUp,
            "prev line first non-blank",
        ),
        ("w", hjkl_vim::MotionKind::WordForward, "word forward"),
        (
            "W",
            hjkl_vim::MotionKind::BigWordForward,
            "BIG word forward",
        ),
        ("b", hjkl_vim::MotionKind::WordBackward, "word back"),
        ("B", hjkl_vim::MotionKind::BigWordBackward, "BIG word back"),
        ("e", hjkl_vim::MotionKind::WordEnd, "word end"),
        ("E", hjkl_vim::MotionKind::BigWordEnd, "BIG word end"),
        // Phase 3c: line-anchor motions.
        ("0", hjkl_vim::MotionKind::LineStart, "line start"),
        ("<Home>", hjkl_vim::MotionKind::LineStart, "line start"),
        ("^", hjkl_vim::MotionKind::FirstNonBlank, "first non-blank"),
        ("$", hjkl_vim::MotionKind::LineEnd, "line end"),
        ("<End>", hjkl_vim::MotionKind::LineEnd, "line end"),
        // Phase 3d: doc-level motion.
        ("G", hjkl_vim::MotionKind::GotoLine, "goto line"),
        // Phase 3e: find-repeat motions.
        (";", hjkl_vim::MotionKind::FindRepeat, "find repeat"),
        (
            ",",
            hjkl_vim::MotionKind::FindRepeatReverse,
            "find repeat reverse",
        ),
        // Phase 3f: bracket-match motion.
        ("%", hjkl_vim::MotionKind::BracketMatch, "match bracket"),
        // Phase 3g: scroll / viewport motions.
        ("H", hjkl_vim::MotionKind::ViewportTop, "viewport top"),
        ("M", hjkl_vim::MotionKind::ViewportMiddle, "viewport middle"),
        ("L", hjkl_vim::MotionKind::ViewportBottom, "viewport bottom"),
        (
            "<C-d>",
            hjkl_vim::MotionKind::HalfPageDown,
            "half page down",
        ),
        ("<C-u>", hjkl_vim::MotionKind::HalfPageUp, "half page up"),
        (
            "<C-f>",
            hjkl_vim::MotionKind::FullPageDown,
            "full page down",
        ),
        ("<C-b>", hjkl_vim::MotionKind::FullPageUp, "full page up"),
    ] {
        let action = AppAction::Motion { kind, count: 1 };
        for mode in [
            Mode::Normal,
            Mode::Visual,
            Mode::VisualLine,
            Mode::VisualBlock,
        ] {
            if let Err(e) = km.add(mode, chord, action.clone(), desc) {
                eprintln!("hjkl: keymap.add({chord:?}) failed: {e}");
            }
        }
    }

    // ── Phase 5b: macro record / play chord entry points ─────────────────
    // `q` — record-macro or stop-recording gate (QChord handles the branch).
    // Normal-mode only: macros cannot be started or stopped in Visual mode.
    // Engine FSM arms for `q` are kept for macro-replay defensive coverage.
    if let Err(e) = km.add(
        Mode::Normal,
        "q",
        AppAction::QChord { count: 1 },
        "record macro / stop recording",
    ) {
        eprintln!("hjkl: keymap.add(q) failed: {e}");
    }

    // `@` — begin play-macro chord. Normal-mode only.
    // Engine FSM arms for `@` are kept for macro-replay defensive coverage.
    if let Err(e) = km.add(
        Mode::Normal,
        "@",
        AppAction::BeginPendingPlayMacro { count: 1 },
        "play macro chord",
    ) {
        eprintln!("hjkl: keymap.add(@) failed: {e}");
    }

    // ── Phase 5c: dot-repeat ─────────────────────────────────────────────
    // `.` replays the last buffered change. Normal-mode only.
    // Engine FSM `.` arm stays for macro-replay defensive coverage.
    if let Err(e) = km.add(
        Mode::Normal,
        ".",
        AppAction::DotRepeat { count: 1 },
        "repeat last change",
    ) {
        eprintln!("hjkl: keymap.add(.) failed: {e}");
    }

    // ── Phase 6.4: insert-mode entry ─────────────────────────────────────
    // Normal mode only. Engine FSM arms kept for macro-replay coverage.
    for (chord, action, desc) in [
        (
            "i",
            AppAction::EnterInsertI { count: 1 },
            "insert before cursor",
        ),
        (
            "I",
            AppAction::EnterInsertShiftI { count: 1 },
            "insert at line start",
        ),
        (
            "a",
            AppAction::EnterInsertA { count: 1 },
            "append after cursor",
        ),
        (
            "A",
            AppAction::EnterInsertShiftA { count: 1 },
            "append at line end",
        ),
        ("o", AppAction::EnterInsertO { count: 1 }, "open line below"),
        (
            "O",
            AppAction::EnterInsertShiftO { count: 1 },
            "open line above",
        ),
        (
            "R",
            AppAction::EnterReplace { count: 1 },
            "enter replace mode",
        ),
    ] {
        if let Err(e) = km.add(Mode::Normal, chord, action, desc) {
            eprintln!("hjkl: keymap.add({chord:?}) failed: {e}");
        }
    }

    // ── Phase 6.4: char / line mutation ops ──────────────────────────────
    // Normal mode only. Engine FSM arms kept for macro-replay coverage.
    for (chord, action, desc) in [
        (
            "x",
            AppAction::DeleteCharForward { count: 1 },
            "delete char forward",
        ),
        (
            "X",
            AppAction::DeleteCharBackward { count: 1 },
            "delete char backward",
        ),
        (
            "s",
            AppAction::SubstituteChar { count: 1 },
            "substitute char",
        ),
        (
            "S",
            AppAction::SubstituteLine { count: 1 },
            "substitute line",
        ),
        ("D", AppAction::DeleteToEol, "delete to end of line"),
        ("C", AppAction::ChangeToEol, "change to end of line"),
        (
            "Y",
            AppAction::YankToEol { count: 1 },
            "yank to end of line",
        ),
        ("J", AppAction::JoinLine { count: 1 }, "join lines"),
        ("~", AppAction::ToggleCase { count: 1 }, "toggle case"),
        (
            "p",
            AppAction::PasteAfter { count: 1 },
            "paste after cursor",
        ),
        (
            "P",
            AppAction::PasteBefore { count: 1 },
            "paste before cursor",
        ),
    ] {
        if let Err(e) = km.add(Mode::Normal, chord, action, desc) {
            eprintln!("hjkl: keymap.add({chord:?}) failed: {e}");
        }
    }

    // ── Phase 6.4: undo / redo ────────────────────────────────────────────
    // `u` undo in Normal mode. `<C-r>` redo in Normal mode only —
    // Insert-mode `<C-r>` goes through the engine FSM and is not intercepted.
    if let Err(e) = km.add(Mode::Normal, "u", AppAction::Undo, "undo") {
        eprintln!("hjkl: keymap.add(u) failed: {e}");
    }
    if let Err(e) = km.add(Mode::Normal, "<C-r>", AppAction::Redo, "redo") {
        eprintln!("hjkl: keymap.add(<C-r>) failed: {e}");
    }

    // ── Phase 6.4: jumplist ───────────────────────────────────────────────
    // `<C-o>` / `<C-i>` bound in Normal mode only.
    // Engine FSM arms kept for macro-replay coverage.
    if let Err(e) = km.add(
        Mode::Normal,
        "<C-o>",
        AppAction::JumpBack { count: 1 },
        "jump back",
    ) {
        eprintln!("hjkl: keymap.add(<C-o>) failed: {e}");
    }
    // Tab in Normal mode = <C-i> (vim aliases them). Crossterm delivers the
    // actual Tab key as KeyCode::Tab, not as Char('i')+CTRL, so we bind <Tab>
    // here. The engine FSM also handles the Tab code path for macro-replay
    // defensive coverage.
    if let Err(e) = km.add(
        Mode::Normal,
        "<Tab>",
        AppAction::JumpForward { count: 1 },
        "jump forward",
    ) {
        eprintln!("hjkl: keymap.add(<Tab>) failed: {e}");
    }

    // ── Phase 6.4: scroll-line ops ────────────────────────────────────────
    // `<C-e>` / `<C-y>` — scroll viewport without moving cursor.
    // Bound in Normal mode only. (Phase 3g already bound <C-d>/<C-u>/<C-f>/<C-b>
    // as Motion variants; those are kept intact — no conflict.)
    use hjkl_engine::ScrollDir;
    if let Err(e) = km.add(
        Mode::Normal,
        "<C-e>",
        AppAction::ScrollLine {
            dir: ScrollDir::Down,
            count: 1,
        },
        "scroll line down",
    ) {
        eprintln!("hjkl: keymap.add(<C-e>) failed: {e}");
    }
    if let Err(e) = km.add(
        Mode::Normal,
        "<C-y>",
        AppAction::ScrollLine {
            dir: ScrollDir::Up,
            count: 1,
        },
        "scroll line up",
    ) {
        eprintln!("hjkl: keymap.add(<C-y>) failed: {e}");
    }

    // ── Phase 6.4: search repeat ──────────────────────────────────────────
    // `n` / `N` — repeat last search. Normal + all Visual modes.
    // `*` / `#` / `g*` / `g#` — word-search. Normal mode only
    // (g* / g# are dispatched through AfterG reducer via BeginPendingAfterG).
    for (chord, forward, desc) in [
        ("n", true, "search forward repeat"),
        ("N", false, "search backward repeat"),
    ] {
        let action = AppAction::SearchRepeat { forward, count: 1 };
        for mode in [
            Mode::Normal,
            Mode::Visual,
            Mode::VisualLine,
            Mode::VisualBlock,
        ] {
            if let Err(e) = km.add(mode, chord, action.clone(), desc) {
                eprintln!("hjkl: keymap.add({chord:?}) failed: {e}");
            }
        }
    }
    // `*` / `#` whole-word search. Normal mode only.
    for (chord, forward, desc) in [
        ("*", true, "search word under cursor forward"),
        ("#", false, "search word under cursor backward"),
    ] {
        let action = AppAction::WordSearch {
            forward,
            whole_word: true,
            count: 1,
        };
        if let Err(e) = km.add(Mode::Normal, chord, action, desc) {
            eprintln!("hjkl: keymap.add({chord:?}) failed: {e}");
        }
    }

    // ── Phase 6.4: visual entry from Normal ──────────────────────────────
    // `v` / `V` / `<C-v>` — enter visual from Normal. `gv` is dispatched
    // through the AfterG reducer (BeginPendingAfterG) — not bound here.
    if let Err(e) = km.add(
        Mode::Normal,
        "v",
        AppAction::EnterVisualChar,
        "enter visual charwise",
    ) {
        eprintln!("hjkl: keymap.add(v) failed: {e}");
    }
    if let Err(e) = km.add(
        Mode::Normal,
        "V",
        AppAction::EnterVisualLine,
        "enter visual linewise",
    ) {
        eprintln!("hjkl: keymap.add(V) failed: {e}");
    }
    if let Err(e) = km.add(
        Mode::Normal,
        "<C-v>",
        AppAction::EnterVisualBlock,
        "enter visual block",
    ) {
        eprintln!("hjkl: keymap.add(<C-v>) failed: {e}");
    }

    // ── Phase 6.4: gv — reenter last visual ──────────────────────────────
    // `gv` is routed through AfterG → the AfterGChord arm in event_loop.rs
    // dispatches ReenterLastVisual. We do NOT bind `gv` directly in the trie
    // because `g` is already bound as BeginPendingAfterG (pending state chord).

    // ── Phase 6.4: visual-mode anchor toggle ─────────────────────────────
    // `o` in Visual / VisualLine / VisualBlock — toggle cursor/anchor.
    // Normal `o` is bound above as EnterInsertO. Mode discrimination is
    // handled automatically by the trie (different mode → different action).
    for mode in [Mode::Visual, Mode::VisualLine, Mode::VisualBlock] {
        if let Err(e) = km.add(
            mode,
            "o",
            AppAction::VisualToggleAnchor,
            "visual toggle anchor",
        ) {
            eprintln!("hjkl: keymap.add(o Visual) failed: {e}");
        }
    }

    km
}

/// Translate an `hjkl_engine::Input` back to a `crossterm::event::KeyEvent`
/// for re-feeding through `route_chord_key` during macro replay.
///
/// This is the inverse of `Editor::handle_key`'s `crossterm_to_input` path.
/// Modifier flags (ctrl, alt, shift) are preserved. Keys that have no
/// crossterm equivalent (e.g. `Key::Null`, `Key::PageUp` without a standard
/// mapping) produce a `KeyCode::Null` sentinel that the replay loop skips.
fn engine_input_to_key_event(input: hjkl_engine::Input) -> crossterm::event::KeyEvent {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use hjkl_engine::Key;

    let code = match input.key {
        Key::Char(c) => KeyCode::Char(c),
        Key::Backspace => KeyCode::Backspace,
        Key::Delete => KeyCode::Delete,
        Key::Enter => KeyCode::Enter,
        Key::Left => KeyCode::Left,
        Key::Right => KeyCode::Right,
        Key::Up => KeyCode::Up,
        Key::Down => KeyCode::Down,
        Key::Home => KeyCode::Home,
        Key::End => KeyCode::End,
        Key::Tab => KeyCode::Tab,
        Key::Esc => KeyCode::Esc,
        Key::PageUp => KeyCode::PageUp,
        Key::PageDown => KeyCode::PageDown,
        Key::Null => KeyCode::Null,
    };
    let mut mods = KeyModifiers::NONE;
    if input.ctrl {
        mods |= KeyModifiers::CONTROL;
    }
    if input.alt {
        mods |= KeyModifiers::ALT;
    }
    if input.shift {
        mods |= KeyModifiers::SHIFT;
    }
    KeyEvent::new(code, mods)
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

    /// Copy the focused window's stored scroll position and cursor into the
    /// active editor's host viewport. Call BEFORE input dispatch so the
    /// engine's scroll math starts from the right offset.
    pub fn sync_viewport_to_editor(&mut self) {
        let fw = self.focused_window();
        let win = self.windows[fw].as_ref().expect("focused_window open");
        let (top_row, top_col) = (win.top_row, win.top_col);
        let (cursor_row, cursor_col) = (win.cursor_row, win.cursor_col);
        let maybe_rect = win.last_rect;
        if let Some(rect) = maybe_rect {
            let vp = self.active_mut().editor.host_mut().viewport_mut();
            vp.top_row = top_row;
            vp.top_col = top_col;
            vp.width = rect.width;
            vp.height = rect.height;
        }
        self.active_mut().editor.jump_cursor(cursor_row, cursor_col);
    }

    /// Copy the active editor's host viewport scroll state and cursor back
    /// into the focused window. Call AFTER input dispatch so the engine's
    /// auto-scroll and cursor updates are persisted.
    pub fn sync_viewport_from_editor(&mut self) {
        let vp = self.active().editor.host().viewport();
        let (top_row, top_col) = (vp.top_row, vp.top_col);
        let (cursor_row, cursor_col) = self.active().editor.cursor();
        let fw = self.focused_window();
        let win = self.windows[fw].as_mut().expect("focused_window open");
        win.top_row = top_row;
        win.top_col = top_col;
        win.cursor_row = cursor_row;
        win.cursor_col = cursor_col;
    }

    /// Refresh window cursor cache, drain dirty flag + content edits, notify
    /// LSP, recompute syntax — call this after any code path that mutated
    /// engine state via `apply_motion` / `handle_key` / replay / etc.
    ///
    /// Bug class memo: any keymap-Match arm that triggers cursor motion via
    /// `apply_motion` must call this before `continue` — otherwise the window
    /// cursor cache goes stale and the render shows the cursor at its old
    /// position. This helper consolidates the three previously duplicated
    /// ~15-line sync blocks in `event_loop.rs` into a single call site.
    pub(crate) fn sync_after_engine_mutation(&mut self) {
        // Keymap-dispatched motions go through `apply_motion_kind` which
        // calls `execute_motion` but does NOT invoke `ensure_cursor_in_scrolloff`
        // (the engine FSM `step()` path does it explicitly). Without this call
        // the engine cursor advances off-screen and the viewport top_row
        // never updates — the user sees the cursor disappear. Mirror the FSM
        // behaviour from the app side so the keymap path stays viewport-coherent.
        // Idempotent for non-motion mutations (already-in-bounds = no-op).
        self.active_mut().editor.ensure_cursor_in_scrolloff();
        // Propagate any mode change (e.g. i/I/a/A/o/O enter-insert actions
        // dispatched through the app keymap) to the host cursor-shape so the
        // render loop picks it up on the next frame. Idempotent when mode
        // did not change.
        self.active_mut().editor.emit_cursor_shape_if_changed();
        self.sync_viewport_from_editor();
        if self.active_mut().editor.take_dirty() {
            let elapsed = self.active_mut().refresh_dirty_against_saved();
            self.last_signature_us = elapsed;
            if self.active().dirty {
                self.active_mut().is_new_file = false;
            }
        }
        let buffer_id = self.active().buffer_id;
        if self.active_mut().editor.take_content_reset() {
            self.syntax.reset(buffer_id);
        }
        let edits = self.active_mut().editor.take_content_edits();
        if !edits.is_empty() {
            self.syntax.apply_edits(buffer_id, &edits);
        }
        self.lsp_notify_change_active();
        self.recompute_and_install();
    }

    // ── Count-prefix helpers ──────────────────────────────────────────────

    /// Drain the pending digit count and replay each digit to the active
    /// editor as a bare `Char` key event.  No-ops when the count is empty
    /// (drain returns an empty string), so callers may omit an
    /// `is_empty` guard if they prefer — the existing guards are kept at
    /// call sites for clarity and symmetry with the surrounding flow.
    fn flush_pending_count_to_engine(&mut self) {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let digits = self.pending_count.drain_as_digits();
        for d in digits.chars() {
            hjkl_vim::handle_key(
                &mut self.active_mut().editor,
                KeyEvent::new(KeyCode::Char(d), KeyModifiers::NONE),
            );
        }
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
                    // search_advance_forward moves the cursor without
                    // going through vim::step's end-of-step scrolloff
                    // hook, so the editor's viewport stays at row 0.
                    // Reveal the cursor here so the focused window's
                    // initial top_row (read below) picks up the scroll.
                    slot.editor.ensure_cursor_in_scrolloff();
                    // Persist direction so a subsequent `n` repeats
                    // forward; without this, vim.last_search_forward
                    // stays at its bool default (false) and `n` jumps
                    // backward as if `?pat<CR>` had been typed.
                    slot.editor.set_last_search(Some(pat), true);
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

        // Single window pointing at slot 0. Seed top_row / top_col from
        // the slot's editor viewport so any pre-event-loop scroll (e.g.
        // +/pat search-on-open) is preserved through the first tick of
        // sync_viewport_to_editor.
        let (initial_top_row, initial_top_col) = {
            let vp = slot.editor.host().viewport();
            (vp.top_row, vp.top_col)
        };
        let initial_window = window::Window {
            slot: 0,
            top_row: initial_top_row,
            top_col: initial_top_col,
            cursor_row: 0,
            cursor_col: 0,
            last_rect: None,
        };

        let default_leader = crate::config::Config::default().editor.leader;
        Ok(Self {
            slots: vec![slot],
            windows: vec![Some(initial_window)],
            tabs: vec![window::Tab {
                layout: window::LayoutTree::Leaf(0),
                focused_window: 0,
            }],
            active_tab: 0,
            next_window_id: 1,
            next_buffer_id: 1,
            prev_active: None,
            exit_requested: false,
            status_message: None,
            info_popup: None,
            command_field: None,
            command_completion: None,
            search_field: None,
            picker: None,
            pending_count: hjkl_vim::CountAccumulator::new(),
            search_dir: SearchDir::Forward,
            last_cursor_shape: CursorShape::Block,
            syntax,
            git_worker: GitSignsWorker::new(),
            directory,
            theme,
            preview_highlighters: std::sync::Mutex::new(std::collections::HashMap::new()),
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
            completion: None,
            pending_code_actions: Vec::new(),
            pending_ctrl_x: false,
            pending_prefix_at: None,
            which_key_active: false,
            which_key_sticky: false,
            which_key_enabled: true,
            which_key_delay: std::time::Duration::from_millis(500),
            user_keymap_records: Vec::new(),
            replay_depth: 0,
            // Default to bundled config's value; main overrides via with_config
            // before crossterm capture is enabled.
            mouse_enabled: crate::config::Config::default().editor.mouse,
            app_keymap: build_app_keymap(default_leader),
            anvil_pool: hjkl_anvil::InstallPool::new(),
            anvil_handles: HashMap::new(),
            anvil_log: HashMap::new(),
            anvil_registry: hjkl_anvil::Registry::embedded().ok(),
            pending_state: None,
            last_ex_command: None,
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
    /// Toggle terminal mouse capture at runtime. Drives the corresponding
    /// crossterm Enable/DisableMouseCapture commands against stdout so
    /// the change takes effect on the next event poll. Idempotent —
    /// flipping to the current state is a no-op for the terminal but
    /// still updates `mouse_enabled` so the field remains the source of
    /// truth.
    pub fn set_mouse_capture(&mut self, on: bool) {
        if self.mouse_enabled == on {
            self.status_message = Some(if on { "mouse" } else { "nomouse" }.into());
            return;
        }
        let res = if on {
            crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)
        } else {
            crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture)
        };
        match res {
            Ok(()) => {
                self.mouse_enabled = on;
                self.status_message = Some(if on { "mouse" } else { "nomouse" }.into());
            }
            Err(e) => {
                self.status_message = Some(format!("E: failed to toggle mouse capture: {e}"));
            }
        }
    }

    pub fn with_config(mut self, config: crate::config::Config) -> Self {
        self.mouse_enabled = config.editor.mouse;
        self.which_key_enabled = config.which_key.enabled;
        self.which_key_delay = std::time::Duration::from_millis(config.which_key.delay_ms);
        // Rebuild the app keymap with the configured leader and timeout.
        let leader = config.editor.leader;
        let timeout = Duration::from_millis(config.which_key.delay_ms);
        self.app_keymap = build_app_keymap(leader);
        self.app_keymap.set_timeout(timeout);
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

    /// Attach an `LspManager` to the app. Call after `with_config`. Iterates
    /// the existing slots and attaches each one whose filename matches a
    /// known language and whose language has a configured server — fixes the
    /// startup case where slot 0 was built before `with_lsp` was wired and
    /// would otherwise miss its `didOpen`.
    pub fn with_lsp(mut self, lsp: hjkl_lsp::LspManager) -> Self {
        self.lsp = Some(lsp);
        for idx in 0..self.slots.len() {
            self.lsp_attach_buffer(idx);
        }
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

    /// Dismiss the active completion popup (if any).
    pub fn dismiss_completion(&mut self) {
        self.completion = None;
        self.pending_ctrl_x = false;
    }

    /// Call whenever a chord prefix first enters the `app_keymap` pending buffer.
    /// Records the timestamp used to drive the which-key idle timeout.
    pub fn note_prefix_set(&mut self) {
        self.pending_prefix_at = Some(std::time::Instant::now());
        self.which_key_active = false;
    }

    /// Call whenever a prefix is resolved or cleared (second key arrived,
    /// Escape pressed, mode change, etc.). Resets all which-key state.
    pub fn clear_prefix_state(&mut self) {
        self.pending_prefix_at = None;
        self.which_key_active = false;
    }

    /// Return the currently-pending chord buffer for Normal mode, or an empty
    /// `Vec` when no prefix is active.
    ///
    /// The caller uses this to drive `which_key::entries_for` directly —
    /// the static `Prefix` enum is no longer needed.
    pub fn active_which_key_prefix(&self) -> Vec<hjkl_keymap::KeyEvent> {
        self.app_keymap.pending(keymap::HjklMode::Normal).to_vec()
    }

    /// Dispatch an [`AppAction`] with an optional repeat count.
    ///
    /// This is the single authoritative dispatch site for all chord-triggered
    /// app actions. Count is applied where meaningful (resize, tab navigation).
    pub fn dispatch_action(&mut self, action: AppAction, count: u32) {
        let count = count.max(1) as usize;
        match action {
            AppAction::OpenFilePicker => self.open_picker(),
            AppAction::OpenBufferPicker => self.open_buffer_picker(),
            AppAction::OpenGrepPicker => self.open_grep_picker(None),
            AppAction::GitStatus => self.open_git_status_picker(),
            AppAction::GitLog => self.open_git_log_picker(),
            AppAction::GitBranch => self.open_git_branch_picker(),
            AppAction::GitFileHistory => self.open_git_file_history_picker(),
            AppAction::GitStashes => self.open_git_stash_picker(),
            AppAction::GitTags => self.open_git_tags_picker(),
            AppAction::GitRemotes => self.open_git_remotes_picker(),
            AppAction::ShowDiagAtCursor => self.show_diag_at_cursor(),
            AppAction::LspCodeActions => self.lsp_code_actions(),
            AppAction::LspRename => {
                // Phase 5 MVP: prompt user to use :Rename <newname>.
                self.status_message = Some("use :Rename <newname> to rename".into());
            }
            AppAction::LspGotoDef => self.lsp_goto_definition(),
            AppAction::LspGotoDecl => self.lsp_goto_declaration(),
            AppAction::LspGotoRef => self.lsp_goto_references(),
            AppAction::LspGotoImpl => self.lsp_goto_implementation(),
            AppAction::LspGotoTypeDef => self.lsp_goto_type_definition(),
            AppAction::Tabnext => {
                for _ in 0..count {
                    self.dispatch_ex("tabnext");
                }
            }
            AppAction::Tabprev => {
                for _ in 0..count {
                    self.dispatch_ex("tabprev");
                }
            }
            AppAction::BufferNext => self.buffer_next(),
            AppAction::BufferPrev => self.buffer_prev(),
            AppAction::DiagNext => self.dispatch_ex("lnext"),
            AppAction::DiagPrev => self.dispatch_ex("lprev"),
            AppAction::DiagNextError => self.lnext_severity(Some(DiagSeverity::Error)),
            AppAction::DiagPrevError => self.lprev_severity(Some(DiagSeverity::Error)),
            AppAction::FocusLeft => self.focus_left(),
            AppAction::FocusBelow => self.focus_below(),
            AppAction::FocusAbove => self.focus_above(),
            AppAction::FocusRight => self.focus_right(),
            AppAction::FocusNext => self.focus_next(),
            AppAction::FocusPrev => self.focus_previous(),
            AppAction::CloseFocusedWindow => self.close_focused_window(),
            AppAction::OnlyFocusedWindow => self.only_focused_window(),
            AppAction::SwapWithSibling => self.swap_with_sibling(),
            AppAction::MoveWindowToNewTab => match self.move_window_to_new_tab() {
                Ok(()) => self.status_message = Some("moved window to new tab".into()),
                Err(msg) => self.status_message = Some(msg.to_string()),
            },
            AppAction::NewSplit => self.dispatch_ex("new"),
            AppAction::ResizeHeight(delta) => self.resize_height(delta * count as i32),
            AppAction::ResizeWidth(delta) => self.resize_width(delta * count as i32),
            AppAction::EqualizeLayout => self.equalize_layout(),
            AppAction::MaximizeHeight => self.maximize_height(),
            AppAction::MaximizeWidth => self.maximize_width(),
            AppAction::QuitOrClose => {
                if self.layout().leaves().len() > 1 {
                    self.close_focused_window();
                } else {
                    self.exit_requested = true;
                }
            }
            AppAction::BeginPendingReplace {
                count: action_count,
            } => {
                // Use buffered count-prefix if present, otherwise the action count.
                let n = self.pending_count.take_or(action_count) as usize;
                self.pending_state = Some(hjkl_vim::PendingState::Replace { count: n });
            }
            AppAction::BeginPendingFind {
                forward,
                till,
                count: action_count,
            } => {
                // Use buffered count-prefix if present, otherwise the action count.
                let n = self.pending_count.take_or(action_count) as usize;
                self.pending_state = Some(hjkl_vim::PendingState::Find {
                    count: n,
                    forward,
                    till,
                });
            }
            AppAction::BeginPendingAfterG {
                count: action_count,
            } => {
                // Use buffered count-prefix if present, otherwise the action count.
                let n = self.pending_count.take_or(action_count) as usize;
                self.pending_state = Some(hjkl_vim::PendingState::AfterG { count: n });
            }
            AppAction::BeginPendingAfterZ {
                count: action_count,
            } => {
                // Use buffered count-prefix if present, otherwise the action count.
                let n = self.pending_count.take_or(action_count) as usize;
                self.pending_state = Some(hjkl_vim::PendingState::AfterZ { count: n });
            }
            AppAction::BeginPendingAfterOp {
                op,
                count1: action_count,
            } => {
                // Use buffered count-prefix if present, otherwise the action count.
                let n = self.pending_count.take_or(action_count) as usize;
                self.pending_state = Some(hjkl_vim::PendingState::AfterOp {
                    op,
                    count1: n,
                    inner_count: 0,
                });
            }
            AppAction::BeginPendingSelectRegister => {
                // `"<reg>` register-prefix chord. The register char is captured
                // by the second key. Do NOT reset pending_count here — a count
                // typed before `"` (e.g. `5"add`) must survive through register
                // selection so the subsequent operator (`d`) can consume it.
                // Example: `5"add` → pending_count=5, `"` → SelectRegister (count
                // preserved), `a` → SetPendingRegister, `dd` → delete 5 lines
                // into register `a`.
                self.pending_state = Some(hjkl_vim::PendingState::SelectRegister);
            }
            AppAction::BeginPendingSetMark => {
                // `m<x>` mark-set chord. No count consumed — char captured by
                // second key. Discard any buffered count (not meaningful here).
                self.pending_count.reset();
                self.pending_state = Some(hjkl_vim::PendingState::SetMark);
            }
            AppAction::BeginPendingGotoMarkLine => {
                // `'<x>` mark-goto-line chord. No count consumed.
                self.pending_count.reset();
                self.pending_state = Some(hjkl_vim::PendingState::GotoMarkLine);
            }
            AppAction::BeginPendingGotoMarkChar => {
                // `` `<x> `` mark-goto-char chord. No count consumed.
                self.pending_count.reset();
                self.pending_state = Some(hjkl_vim::PendingState::GotoMarkChar);
            }
            AppAction::QChord { .. } => {
                // `q` in Normal mode. Branch: stop recording if active, else
                // open the RecordMacroTarget chord to wait for the register char.
                self.pending_count.reset();
                if self.active().editor.is_recording_macro() {
                    // Bare `q` ends the active recording.
                    self.active_mut().editor.stop_macro_record();
                } else {
                    self.pending_state = Some(hjkl_vim::PendingState::RecordMacroTarget);
                }
            }
            AppAction::BeginPendingPlayMacro {
                count: action_count,
            } => {
                // `@` in Normal mode. Capture count and wait for register char.
                let n = self.pending_count.take_or(action_count) as usize;
                self.pending_state =
                    Some(hjkl_vim::PendingState::PlayMacroTarget { count: n.max(1) });
            }
            AppAction::DotRepeat {
                count: action_count,
            } => {
                // `.` dot-repeat. Combine pending count prefix with action count.
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.replay_last_change(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::Motion {
                kind,
                count: action_count,
            } => {
                // Use buffered count-prefix if present, otherwise the action count.
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.apply_motion(kind, n);
            }
            AppAction::VisualOp {
                op,
                count: action_count,
            } => {
                // Use buffered count-prefix if present, otherwise the action default.
                let n = self.pending_count.take_or(action_count) as usize;
                // Resolve the active visual range from the engine. The RangeKind
                // must match the visual mode so the range-mutation primitives apply
                // the correct inclusion semantics.
                //
                // Phase 4e follow-ups: all three visual modes now route through the
                // public range-mutation primitives rather than falling back to the
                // engine FSM:
                //   - Visual: pending_register now read from engine getter (gap fixed)
                //   - VisualLine: guard fix in run_operator_over_range allows single-
                //     row linewise, so FSM fallback for d/y/c is removed
                //   - VisualBlock: delete_block / yank_block / change_block / indent_block
                //     now exposed (gap fixed), FSM fallback removed
                use hjkl_engine::{RangeKind, VimMode};
                let vim_mode = self.active().editor.vim_mode();
                // Read the user's pending register selection BEFORE the match so all
                // three mode arms can use it. pending_register() does not clear the
                // selection — the engine clears it when the next operator fires.
                let register = self.active().editor.pending_register().unwrap_or('"');
                match vim_mode {
                    VimMode::VisualBlock => {
                        // Rectangular selection — use block-shape primitives.
                        let Some((top_row, bot_row, left_col, right_col)) =
                            self.active().editor.block_highlight()
                        else {
                            return;
                        };
                        match op {
                            hjkl_vim::OperatorKind::Delete => {
                                self.active_mut()
                                    .editor
                                    .delete_block(top_row, bot_row, left_col, right_col, register);
                            }
                            hjkl_vim::OperatorKind::Yank => {
                                self.active_mut()
                                    .editor
                                    .yank_block(top_row, bot_row, left_col, right_col, register);
                            }
                            hjkl_vim::OperatorKind::Change => {
                                self.active_mut()
                                    .editor
                                    .change_block(top_row, bot_row, left_col, right_col, register);
                                // change_block enters Insert (BlockChange reason);
                                // no Esc needed.
                                return;
                            }
                            hjkl_vim::OperatorKind::Indent => {
                                self.active_mut()
                                    .editor
                                    .indent_block(top_row, bot_row, left_col, right_col, n as i32);
                            }
                            hjkl_vim::OperatorKind::Outdent => {
                                self.active_mut().editor.indent_block(
                                    top_row,
                                    bot_row,
                                    left_col,
                                    right_col,
                                    -(n as i32),
                                );
                            }
                            _ => return,
                        }
                        // Exit visual mode after the op (except Change above).
                        use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
                        hjkl_vim::handle_key(
                            &mut self.active_mut().editor,
                            CtKeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                        );
                    }
                    VimMode::Visual => {
                        // Charwise visual selection — inclusive on both ends.
                        let Some((start, end)) = self.active().editor.char_highlight() else {
                            return;
                        };
                        let kind = RangeKind::Inclusive;
                        match op {
                            hjkl_vim::OperatorKind::Delete => {
                                self.active_mut()
                                    .editor
                                    .delete_range(start, end, kind, register);
                            }
                            hjkl_vim::OperatorKind::Yank => {
                                self.active_mut()
                                    .editor
                                    .yank_range(start, end, kind, register);
                            }
                            hjkl_vim::OperatorKind::Change => {
                                self.active_mut()
                                    .editor
                                    .change_range(start, end, kind, register);
                                // change_range transitions to Insert via
                                // begin_insert_noundo — no explicit mode-set needed.
                                return;
                            }
                            hjkl_vim::OperatorKind::Indent => {
                                self.active_mut()
                                    .editor
                                    .indent_range(start, end, n as i32, 0);
                            }
                            hjkl_vim::OperatorKind::Outdent => {
                                self.active_mut()
                                    .editor
                                    .indent_range(start, end, -(n as i32), 0);
                            }
                            _ => return,
                        }
                        // Exit visual mode after the op (except Change, which already
                        // transitioned to Insert above).
                        use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
                        hjkl_vim::handle_key(
                            &mut self.active_mut().editor,
                            CtKeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                        );
                    }
                    VimMode::VisualLine => {
                        // Linewise visual selection — full rows.
                        // Option (a): pass (top_row, 0) and (bot_row, usize::MAX)
                        // with RangeKind::Linewise. The engine's run_operator_over_range
                        // handles Linewise semantics; read_vim_range / cut_vim_range
                        // snap to full line boundaries regardless of the col values.
                        // The Phase 4e guard fix allows single-row (top==bot) Linewise
                        // ranges, so this path works for both single and multi-line.
                        let Some((top_row, bot_row)) = self.active().editor.line_highlight() else {
                            return;
                        };
                        let kind = RangeKind::Linewise;
                        match op {
                            hjkl_vim::OperatorKind::Delete => {
                                self.active_mut().editor.delete_range(
                                    (top_row, 0),
                                    (bot_row, usize::MAX),
                                    kind,
                                    register,
                                );
                            }
                            hjkl_vim::OperatorKind::Yank => {
                                self.active_mut().editor.yank_range(
                                    (top_row, 0),
                                    (bot_row, usize::MAX),
                                    kind,
                                    register,
                                );
                            }
                            hjkl_vim::OperatorKind::Change => {
                                self.active_mut().editor.change_range(
                                    (top_row, 0),
                                    (bot_row, usize::MAX),
                                    kind,
                                    register,
                                );
                                // change_range enters Insert mode.
                                return;
                            }
                            hjkl_vim::OperatorKind::Indent => {
                                self.active_mut().editor.indent_range(
                                    (top_row, 0),
                                    (bot_row, 0),
                                    n as i32,
                                    0,
                                );
                            }
                            hjkl_vim::OperatorKind::Outdent => {
                                self.active_mut().editor.indent_range(
                                    (top_row, 0),
                                    (bot_row, 0),
                                    -(n as i32),
                                    0,
                                );
                            }
                            _ => return,
                        }
                        // Exit visual mode after the op (except Change above).
                        use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
                        hjkl_vim::handle_key(
                            &mut self.active_mut().editor,
                            CtKeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                        );
                    }
                    _ => {
                        // Not in a visual mode — keymap bound VisualOp but
                        // engine is in Normal/Insert/etc. Shouldn't happen;
                        // bail silently.
                    }
                }
            }
            // ── Phase 6.4: insert-mode entry ──────────────────────────────
            AppAction::EnterInsertI {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.enter_insert_i(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::EnterInsertShiftI {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.enter_insert_shift_i(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::EnterInsertA {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.enter_insert_a(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::EnterInsertShiftA {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.enter_insert_shift_a(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::EnterInsertO {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.open_line_below(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::EnterInsertShiftO {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.open_line_above(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::EnterReplace {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.enter_replace_mode(n.max(1));
                self.sync_after_engine_mutation();
            }

            // ── Phase 6.4: char / line mutation ops ───────────────────────
            AppAction::DeleteCharForward {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.delete_char_forward(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::DeleteCharBackward {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.delete_char_backward(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::SubstituteChar {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.substitute_char(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::SubstituteLine {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.substitute_line(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::DeleteToEol => {
                self.pending_count.reset();
                self.active_mut().editor.delete_to_eol();
                self.sync_after_engine_mutation();
            }
            AppAction::ChangeToEol => {
                self.pending_count.reset();
                self.active_mut().editor.change_to_eol();
                self.sync_after_engine_mutation();
            }
            AppAction::YankToEol {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.yank_to_eol(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::JoinLine {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                // Vim join default is 2 (join current + 1 following line).
                self.active_mut().editor.join_line(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::ToggleCase {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.toggle_case_at_cursor(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::PasteAfter {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.paste_after(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::PasteBefore {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.paste_before(n.max(1));
                self.sync_after_engine_mutation();
            }

            // ── Phase 6.4: undo / redo ────────────────────────────────────
            AppAction::Undo => {
                self.pending_count.reset();
                self.active_mut().editor.undo();
                self.sync_after_engine_mutation();
            }
            AppAction::Redo => {
                self.pending_count.reset();
                self.active_mut().editor.redo();
                self.sync_after_engine_mutation();
            }

            // ── Phase 6.4: jumplist ───────────────────────────────────────
            AppAction::JumpBack {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.jump_back(n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::JumpForward {
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.jump_forward(n.max(1));
                self.sync_after_engine_mutation();
            }

            // ── Phase 6.4: scroll ops ──────────────────────────────────────
            AppAction::ScrollFullPage {
                dir,
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.scroll_full_page(dir, n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::ScrollHalfPage {
                dir,
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.scroll_half_page(dir, n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::ScrollLine {
                dir,
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.scroll_line(dir, n.max(1));
                self.sync_after_engine_mutation();
            }

            // ── Phase 6.4: search repeat ───────────────────────────────────
            AppAction::SearchRepeat {
                forward,
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut().editor.search_repeat(forward, n.max(1));
                self.sync_after_engine_mutation();
            }
            AppAction::WordSearch {
                forward,
                whole_word,
                count: action_count,
            } => {
                let n = self.pending_count.take_or(action_count) as usize;
                self.active_mut()
                    .editor
                    .word_search(forward, whole_word, n.max(1));
                self.sync_after_engine_mutation();
            }

            // ── Phase 6.4: visual entry / toggle ──────────────────────────
            AppAction::EnterVisualChar => {
                self.pending_count.reset();
                self.active_mut().editor.enter_visual_char();
            }
            AppAction::EnterVisualLine => {
                self.pending_count.reset();
                self.active_mut().editor.enter_visual_line();
            }
            AppAction::EnterVisualBlock => {
                self.pending_count.reset();
                self.active_mut().editor.enter_visual_block();
            }
            AppAction::ReenterLastVisual => {
                self.pending_count.reset();
                self.active_mut().editor.reenter_last_visual();
                self.sync_viewport_from_editor();
            }
            AppAction::VisualToggleAnchor => {
                self.pending_count.reset();
                self.active_mut().editor.visual_o_toggle();
                self.sync_viewport_from_editor();
            }

            AppAction::Replay { keys, recursive } => {
                if recursive {
                    // Re-feed each key through the chord FSM. The queue is
                    // processed FIFO so we use a VecDeque.
                    //
                    // Two guards against runaway recursion:
                    //   - `steps` caps the queue iteration count per frame —
                    //     catches horizontal cycles (`:nmap a bbbbb…` etc).
                    //   - `replay_depth` caps re-entrant dispatch_action stack
                    //     depth — catches vertical cycles (`:nmap a a`) which
                    //     would otherwise stack-overflow.
                    use std::collections::VecDeque;
                    const MAX_STEPS: usize = 1024;
                    // Vertical recursion depth cap. Sized to fit comfortably
                    // within macOS's 512 KB per-thread stack default (cargo
                    // nextest spawns tests on non-main threads): each frame
                    // of this arm carries a VecDeque, sub_replay Vec, and the
                    // recursive call into dispatch_action. 128 frames is far
                    // beyond any realistic nested-map depth and leaves plenty
                    // of stack headroom on all platforms.
                    const MAX_DEPTH: usize = 128;
                    if self.replay_depth >= MAX_DEPTH {
                        self.status_message = Some("E223: recursive mapping (depth limit)".into());
                        return;
                    }
                    self.replay_depth += 1;
                    let mut queue: VecDeque<hjkl_keymap::KeyEvent> = keys.into();
                    let mut steps = 0usize;
                    while let Some(ev) = queue.pop_front() {
                        steps += 1;
                        if steps > MAX_STEPS {
                            self.status_message =
                                Some("E223: recursive mapping (1024-step limit)".into());
                            break;
                        }
                        let mode = current_km_mode(self);
                        let Some(mode) = mode else {
                            continue;
                        };
                        let mut sub_replay = Vec::new();
                        let consumed = self.dispatch_keymap_in_mode(ev, 1, &mut sub_replay, mode);
                        if !consumed && sub_replay.len() <= 1 {
                            self.replay_km_events_to_engine(&sub_replay);
                        }
                    }
                    self.replay_depth -= 1;
                } else {
                    // Non-recursive: bypass the trie and go straight to the engine.
                    for ev in keys {
                        self.replay_km_events_to_engine(std::slice::from_ref(&ev));
                    }
                }
            }
        }
    }

    /// Replay a slice of `hjkl_keymap::KeyEvent`s straight to the engine,
    /// converting each one to a crossterm `KeyEvent` via the shared translator.
    pub(crate) fn replay_km_events_to_engine(&mut self, events: &[hjkl_keymap::KeyEvent]) {
        for km_ev in events {
            let ct_ev = crate::keymap_translate::to_crossterm(km_ev);
            hjkl_vim::handle_key(&mut self.active_mut().editor, ct_ev);
        }
    }

    /// Feed a crossterm key event through the app-level chord keymap and
    /// dispatch any resolved action. Returns `true` if the key was consumed
    /// (either resolved or still pending), `false` if the keymap returned
    /// `Unbound` and the caller should replay the events to the engine.
    ///
    /// Replayed events are stored in `out_replay` (never `None`-cleared).
    ///
    /// This is a thin shim over [`dispatch_keymap_in_mode`] fixed to Normal mode.
    pub fn dispatch_keymap(
        &mut self,
        km_ev: hjkl_keymap::KeyEvent,
        count: u32,
        out_replay: &mut Vec<hjkl_keymap::KeyEvent>,
    ) -> bool {
        self.dispatch_keymap_in_mode(km_ev, count, out_replay, keymap::HjklMode::Normal)
    }

    /// Mode-generalized chord dispatch. Feed `km_ev` into the trie for `mode`
    /// and dispatch any resolved action.
    ///
    /// Returns `true` if consumed (Pending / Ambiguous / Match),
    /// `false` if Unbound (events stored in `out_replay`).
    pub fn dispatch_keymap_in_mode(
        &mut self,
        km_ev: hjkl_keymap::KeyEvent,
        count: u32,
        out_replay: &mut Vec<hjkl_keymap::KeyEvent>,
        mode: keymap::HjklMode,
    ) -> bool {
        use hjkl_keymap::KeyResolve;
        let now = std::time::Instant::now();
        match self.app_keymap.feed(mode, km_ev, now) {
            KeyResolve::Pending => {
                self.note_prefix_set();
                true
            }
            KeyResolve::Ambiguous => {
                self.note_prefix_set();
                true
            }
            KeyResolve::Match(binding) => {
                self.clear_prefix_state();
                self.dispatch_action(binding.action, count);
                true
            }
            KeyResolve::Unbound(events) => {
                self.clear_prefix_state();
                out_replay.extend(events);
                false
            }
        }
    }

    /// Convert a `hjkl_keymap::KeyEvent` back to a `crossterm::event::KeyEvent`
    /// for replaying unbound sequences to the engine.
    ///
    /// Moved here from `event_loop.rs` (option A) so that both the event loop
    /// and tests can replay keymap events without touching file-local functions.
    pub(crate) fn km_to_crossterm(ev: &hjkl_keymap::KeyEvent) -> crossterm::event::KeyEvent {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use hjkl_keymap::{KeyCode as KmKeyCode, KeyModifiers as KmKeyMods};
        let code = match ev.code {
            KmKeyCode::Char(c) => KeyCode::Char(c),
            KmKeyCode::Enter => KeyCode::Enter,
            KmKeyCode::Esc => KeyCode::Esc,
            KmKeyCode::Tab => KeyCode::Tab,
            KmKeyCode::Backspace => KeyCode::Backspace,
            KmKeyCode::Delete => KeyCode::Delete,
            KmKeyCode::Insert => KeyCode::Insert,
            KmKeyCode::Up => KeyCode::Up,
            KmKeyCode::Down => KeyCode::Down,
            KmKeyCode::Left => KeyCode::Left,
            KmKeyCode::Right => KeyCode::Right,
            KmKeyCode::Home => KeyCode::Home,
            KmKeyCode::End => KeyCode::End,
            KmKeyCode::PageUp => KeyCode::PageUp,
            KmKeyCode::PageDown => KeyCode::PageDown,
            KmKeyCode::F(n) => KeyCode::F(n),
        };
        let mut mods = KeyModifiers::NONE;
        if ev.modifiers.contains(KmKeyMods::CTRL) {
            mods |= KeyModifiers::CONTROL;
        }
        if ev.modifiers.contains(KmKeyMods::SHIFT) {
            mods |= KeyModifiers::SHIFT;
        }
        if ev.modifiers.contains(KmKeyMods::ALT) {
            mods |= KeyModifiers::ALT;
        }
        KeyEvent::new(code, mods)
    }

    /// Replay a slice of `hjkl_keymap::KeyEvent`s to the engine via crossterm
    /// `KeyEvent`s. Each keymap event is converted back to a crossterm event
    /// and forwarded to `editor.handle_key`.
    ///
    /// Moved here from `event_loop.rs` (option A) for testability.
    pub(crate) fn replay_to_engine(&mut self, events: &[hjkl_keymap::KeyEvent]) {
        for km_ev in events {
            let ct_ev = Self::km_to_crossterm(km_ev);
            hjkl_vim::handle_key(&mut self.active_mut().editor, ct_ev);
        }
    }

    /// Single canonical chord-routing entry. Called by the event loop's key
    /// handler and by tests. Returns `true` if the key was consumed at any
    /// stage of the chord routing; `false` if it should fall through to the
    /// engine `handle_key` path.
    ///
    /// Order (matches production event loop exactly — this IS the production
    /// routing now, not a test mirror):
    ///   1. pending_state reducer (all modes, when `pending_state.is_some()`)
    ///   2. Non-Normal trie dispatch (mode != Normal AND pending_state.is_none())
    ///   3. Normal-mode keymap dispatch (mode == Normal AND pending_state.is_none())
    ///
    /// Out of scope (run BEFORE this method in event_loop.rs):
    ///   - command-field overlay (`self.command_field.is_some()`)
    ///   - search-field overlay (`self.search_field.is_some()`)
    ///   - picker overlay (`self.picker.is_some()`)
    ///   - info-popup dismissal
    ///   - Visual-mode `:` intercept (must precede pending_state reducer)
    ///   - LSP hover (`K`)
    ///   - `:` and `/` intercepts (Normal-mode only)
    ///   - Insert-mode completion handling
    ///   - tmux-navigator Ctrl-h/j/k/l
    ///   - count-prefix buffering (digits `0`–`9` in Normal mode)
    ///   - Ctrl-^/Ctrl-6 alt-buffer toggle
    ///   - Shift-H / Shift-L buffer cycle
    ///   - Esc chord-reset and which-key Backspace navigate-up
    pub(crate) fn route_chord_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        // Snapshot recording state BEFORE dispatch so we can detect the moment
        // a new recording starts (StartMacroRecord arm) — the register-name key
        // that triggered the start is a bookkeeping key and must NOT be recorded.
        // Similarly, if we were not recording before and are now, skip this key.
        //
        // The @{reg} register-name key (PlayMacro arm) also must not be recorded;
        // that arm returns early in route_chord_key_inner so is_recording_macro()
        // state doesn't change between before/after — BUT recording may be active
        // before the @{reg} key (recording a macro that includes a @a call). In
        // that case we ALSO skip the register name (pending_was_macro_chord logic).
        let was_recording_before = self.active().editor.is_recording_macro();
        let was_play_macro_pending = matches!(
            self.pending_state,
            Some(hjkl_vim::PendingState::PlayMacroTarget { .. })
        );
        let consumed = self.route_chord_key_inner(key);
        // Recorder hook: append the consumed key to the active macro recording
        // (if any) so replays reproduce the same sequence. Skip:
        //   1. When not consumed (key was not processed).
        //   2. When replaying (is_replaying_macro).
        //   3. When the key just started a new recording (was_recording_before
        //      was false but is_recording_macro() is now true — the `a` in `qa`
        //      is the register-name bookkeeping key).
        //   4. When the key was the second half of a @{reg} chord
        //      (was_play_macro_pending) — the register name is bookkeeping.
        let is_recording_now = self.active().editor.is_recording_macro();
        let is_replaying_now = self.active().editor.is_replaying_macro();
        let just_started_recording = !was_recording_before && is_recording_now;
        let register_name_of_play = was_play_macro_pending;
        if consumed
            && is_recording_now
            && !is_replaying_now
            && !just_started_recording
            && !register_name_of_play
        {
            let input = hjkl_engine::Input::from(key);
            if input.key != hjkl_engine::Key::Null {
                self.active_mut().editor.record_input(input);
            }
        }
        consumed
    }

    /// Inner implementation of `route_chord_key`. Returns `true` if the key
    /// was consumed. The public wrapper adds the recorder hook on top.
    fn route_chord_key_inner(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        use hjkl_vim::{Key as VimKey, Outcome};

        // (1) pending_state reducer — fires in all modes when state is Some.
        // Must precede the Non-Normal trie dispatch so the second key of a
        // chord (e.g. second `g` of `gg` in VisualLine) reaches the commit
        // arm instead of re-firing BeginPendingAfterG via the trie.
        if let Some(state) = self.pending_state {
            let vim_key = match key.code {
                KeyCode::Char(c) => Some(VimKey::Char(c)),
                KeyCode::Esc => Some(VimKey::Esc),
                KeyCode::Enter => Some(VimKey::Enter),
                KeyCode::Backspace => Some(VimKey::Backspace),
                KeyCode::Tab => Some(VimKey::Tab),
                _ => None,
            };
            if let Some(vk) = vim_key {
                match hjkl_vim::step(state, vk) {
                    Outcome::Wait(new_state) => {
                        self.pending_state = Some(new_state);
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::ReplaceChar { ch, count }) => {
                        self.pending_state = None;
                        self.active_mut().editor.replace_char_at(ch, count);
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::FindChar {
                        ch,
                        forward,
                        till,
                        count,
                    }) => {
                        self.pending_state = None;
                        self.active_mut().editor.find_char(ch, forward, till, count);
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::AfterGChord { ch, count }) => {
                        self.pending_state = None;
                        // App-level g-prefix actions dispatched before falling
                        // through to the engine.
                        match ch {
                            // Phase 6.4: `gv` — reenter last visual selection.
                            'v' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::ReenterLastVisual,
                                    count as u32,
                                );
                                return true;
                            }
                            // Phase 6.4: `g*` / `g#` — word search without whole-word anchors.
                            '*' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::WordSearch {
                                        forward: true,
                                        whole_word: false,
                                        count: count as u32,
                                    },
                                    count as u32,
                                );
                                return true;
                            }
                            '#' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::WordSearch {
                                        forward: false,
                                        whole_word: false,
                                        count: count as u32,
                                    },
                                    count as u32,
                                );
                                return true;
                            }
                            't' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::Tabnext,
                                    count as u32,
                                );
                                return true;
                            }
                            'T' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::Tabprev,
                                    count as u32,
                                );
                                return true;
                            }
                            'd' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::LspGotoDef,
                                    count as u32,
                                );
                                return true;
                            }
                            'D' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::LspGotoDecl,
                                    count as u32,
                                );
                                return true;
                            }
                            'r' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::LspGotoRef,
                                    count as u32,
                                );
                                return true;
                            }
                            'i' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::LspGotoImpl,
                                    count as u32,
                                );
                                return true;
                            }
                            'y' => {
                                self.dispatch_action(
                                    crate::keymap_actions::AppAction::LspGotoTypeDef,
                                    count as u32,
                                );
                                return true;
                            }
                            _ => {}
                        }
                        // Chord-init case-ops: intercept u/U/~/q and set
                        // reducer AfterOp instead of calling after_g (which
                        // would set engine Pending::Op). This keeps the full
                        // gU/gu/g~/gq op-pending path inside the reducer.
                        let case_op_kind = match ch {
                            'u' => Some(hjkl_vim::OperatorKind::Lowercase),
                            'U' => Some(hjkl_vim::OperatorKind::Uppercase),
                            '~' => Some(hjkl_vim::OperatorKind::ToggleCase),
                            'q' => Some(hjkl_vim::OperatorKind::Reflow),
                            _ => None,
                        };
                        if let Some(op) = case_op_kind {
                            self.pending_state = Some(hjkl_vim::PendingState::AfterOp {
                                op,
                                count1: count,
                                inner_count: 0,
                            });
                            return true;
                        }
                        // All other g-chords: delegate to engine.
                        self.active_mut().editor.after_g(ch, count);
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::AfterZChord { ch, count }) => {
                        self.pending_state = None;
                        // All z-chords delegate directly to the engine.
                        self.active_mut().editor.after_z(ch, count);
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpMotion {
                        op,
                        motion_key,
                        total_count,
                    }) => {
                        self.pending_state = None;
                        self.active_mut().editor.apply_op_motion(
                            event_loop::op_kind_to_operator(op),
                            motion_key,
                            total_count,
                        );
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpDouble { op, total_count }) => {
                        self.pending_state = None;
                        self.active_mut()
                            .editor
                            .apply_op_double(event_loop::op_kind_to_operator(op), total_count);
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpTextObj {
                        op,
                        ch,
                        inner,
                        total_count,
                    }) => {
                        self.pending_state = None;
                        self.active_mut().editor.apply_op_text_obj(
                            event_loop::op_kind_to_operator(op),
                            ch,
                            inner,
                            total_count,
                        );
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpG {
                        op,
                        ch,
                        total_count,
                    }) => {
                        self.pending_state = None;
                        self.active_mut().editor.apply_op_g(
                            event_loop::op_kind_to_operator(op),
                            ch,
                            total_count,
                        );
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpFind {
                        op,
                        ch,
                        forward,
                        till,
                        total_count,
                    }) => {
                        self.pending_state = None;
                        self.active_mut().editor.apply_op_find(
                            event_loop::op_kind_to_operator(op),
                            ch,
                            forward,
                            till,
                            total_count,
                        );
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::SetPendingRegister { reg }) => {
                        self.pending_state = None;
                        self.active_mut().editor.set_pending_register(reg);
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::SetMark { ch }) => {
                        self.pending_state = None;
                        self.active_mut().editor.set_mark_at_cursor(ch);
                        // No sync needed — set_mark_at_cursor does not move cursor.
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::GotoMarkLine { ch }) => {
                        self.pending_state = None;
                        self.active_mut().editor.goto_mark_line(ch);
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::GotoMarkChar { ch }) => {
                        self.pending_state = None;
                        self.active_mut().editor.goto_mark_char(ch);
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::StartMacroRecord { reg }) => {
                        // `q{reg}` chord completed — begin recording. The
                        // bookkeeping key (`q` itself) was already excluded from
                        // the recording by QChord's pending-count reset path;
                        // this register-char is also a bookkeeping key (it names
                        // the register, not a replay action), so the recorder hook
                        // below must skip it. We set pending_state = None before
                        // returning so the hook sees None and skips naturally.
                        self.pending_state = None;
                        self.active_mut().editor.start_macro_record(reg);
                        // Do NOT call the recorder hook here — the register char is
                        // bookkeeping, not a recorded keystroke. Return immediately.
                        return true;
                    }
                    Outcome::Commit(hjkl_vim::EngineCmd::PlayMacro { reg, count }) => {
                        self.pending_state = None;
                        if reg == ':' {
                            // `@:` — repeat last ex command. App-side storage,
                            // NOT routed through engine.play_macro (which would
                            // look in a register). count > 1 → repeat N times
                            // (vim semantics). Phase 5d of kryptic-sh/hjkl#71.
                            for _ in 0..count.max(1) {
                                self.replay_last_ex();
                            }
                            return true;
                        }
                        // `@{reg}` chord completed — decode and re-feed the macro.
                        let inputs = self.active_mut().editor.play_macro(reg, count);
                        // Re-feed each Input through route_chord_key by converting
                        // it back to a crossterm KeyEvent. During replay,
                        // is_replaying_macro() == true so the recorder hook skips
                        // the replayed inputs.
                        for input in inputs {
                            let ct_key = engine_input_to_key_event(input);
                            if ct_key.code != KeyCode::Null {
                                self.route_chord_key(ct_key);
                            }
                        }
                        self.active_mut().editor.end_macro_replay();
                        self.sync_after_engine_mutation();
                        return true;
                    }
                    Outcome::Cancel => {
                        self.pending_state = None;
                        return true;
                    }
                    Outcome::Forward => {
                        // State stays alive; fall through to step (2) below.
                    }
                }
            }
        }

        // (2) Non-Normal trie dispatch — gated on pending_state.is_none().
        // Step (1) above already returns early when pending_state.is_some(),
        // so this gate is logically redundant but documents intent: the second
        // key of a chord (e.g. second `g` of `gg` in VisualLine) must reach
        // the reducer's commit arm above, not re-fire the trie.
        if self.pending_state.is_none()
            && self.active().editor.vim_mode() != hjkl_engine::VimMode::Normal
            && let Some(km_ev) = crate::keymap_translate::from_crossterm(&key)
            && let Some(km_mode) = current_km_mode(self)
        {
            let mut replay: Vec<hjkl_keymap::KeyEvent> = Vec::new();
            let consumed = self.dispatch_keymap_in_mode(km_ev, 1, &mut replay, km_mode);
            if consumed {
                self.sync_after_engine_mutation();
                return true;
            }
            // Unbound — fall through to engine.
        }

        // (3) Normal-mode keymap dispatch — only the trie step; count-prefix
        // buffering and engine-pending bypass run in event_loop.rs before this
        // call and set up the correct state for dispatch_keymap to read.
        if self.pending_state.is_none()
            && self.active().editor.vim_mode() == hjkl_engine::VimMode::Normal
            && let Some(km_ev) = crate::keymap_translate::from_crossterm(&key)
        {
            let engine_pending = self.active().editor.is_chord_pending();
            if !engine_pending {
                let count = self.pending_count.peek().max(1);
                let mut replay: Vec<hjkl_keymap::KeyEvent> = Vec::new();
                let consumed = self.dispatch_keymap(km_ev, count, &mut replay);
                if !consumed {
                    if !self.pending_count.is_empty() {
                        self.flush_pending_count_to_engine();
                    }
                    self.replay_to_engine(&replay);
                }
                self.sync_after_engine_mutation();
                return true;
            }
        }

        false
    }

    /// `@:` — replay the last ex command. No-op when nothing has been
    /// dispatched yet. Phase 5d of kryptic-sh/hjkl#71.
    pub(crate) fn replay_last_ex(&mut self) {
        if let Some(cmd) = self.last_ex_command.clone() {
            self.dispatch_ex(&cmd);
        }
    }

    /// Force-resolve a pending chord buffer after the keymap timeout has
    /// elapsed. Called from the event loop's poll-timeout branch when a chord
    /// is pending (typically `Ambiguous`: e.g. both `g` and `gd` bound — the
    /// shorter binding fires after `timeoutlen`).
    ///
    /// Returns:
    /// - `Some(events)` to be replayed to the engine for `Unbound` with
    ///   drained events (real dead-end case).
    /// - `Some(empty)` after a `Match` (the action was already dispatched).
    /// - `None` when the buffer was empty OR when the buffer is a pure prefix
    ///   (user is mid-chord and `timeout_resolve` left the buffer in place —
    ///   needed so the which-key popup stays visible past the timeout).
    pub fn resolve_chord_timeout(
        &mut self,
        mode: keymap::HjklMode,
    ) -> Option<Vec<hjkl_keymap::KeyEvent>> {
        use hjkl_keymap::KeyResolve;
        if self.app_keymap.pending(mode).is_empty() {
            return None;
        }
        match self.app_keymap.timeout_resolve(mode) {
            KeyResolve::Match(binding) => {
                self.clear_prefix_state();
                self.dispatch_action(binding.action, 1);
                Some(Vec::new())
            }
            KeyResolve::Unbound(events) if events.is_empty() => {
                // Pure-prefix: timeout_resolve was a no-op. Keep prefix state
                // alive so the which-key popup stays visible.
                None
            }
            KeyResolve::Unbound(events) => {
                self.clear_prefix_state();
                Some(events)
            }
            // timeout_resolve only returns Match or Unbound; defensive fallthrough.
            _ => None,
        }
    }
}

/// Return the current `HjklMode` based on the active editor's vim mode.
/// Returns `None` for modes with no keymap equivalent (currently none, but
/// Terminal mode would be `None` if ever added here).
pub(crate) fn current_km_mode(app: &App) -> Option<keymap::HjklMode> {
    keymap::map_mode_to_km_mode(keymap::map_mode_for_vim(app.active().editor.vim_mode())?)
}
