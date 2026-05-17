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
use std::collections::HashSet;

mod buffer_ops;
pub(crate) mod chord_routing;
pub(crate) mod count_prefix;
mod engine_actions;
mod event_loop;
mod ex_dispatch;
pub(crate) mod ex_host_cmds;
pub(crate) mod keymap;
pub(crate) mod keymap_build;
pub mod lsp_glue;
pub(crate) mod mappings_dispatch;
pub mod mouse;
mod pending_actions;
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

/// Height of the unified top bar (buffers left, tabs right) at the top of the
/// screen, when shown (either more than one slot or more than one tab).
pub const TOP_BAR_HEIGHT: u16 = 1;

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

/// Per-mode mouse-enable flags — mirrors Vim's `:set mouse=<flags>`.
///
/// Default (all enabled) corresponds to `mouse=a`.  Set individual fields to
/// `false` to disable mouse in that mode.  The event loop checks
/// [`App::mouse_flags`] via [`mouse_enabled_for`] before processing any mouse
/// event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseFlags {
    /// Mouse active in Normal mode (`n`).
    pub normal: bool,
    /// Mouse active in Visual / VisualLine / VisualBlock mode (`v`).
    pub visual: bool,
    /// Mouse active in Insert mode (`i`).
    pub insert: bool,
    /// Mouse active in Command-line / prompt mode (`c`).
    pub command: bool,
    /// Mouse active in Help buffers (`h`). Parsed for compatibility but unused today.
    pub help: bool,
}

impl MouseFlags {
    /// All modes enabled — equivalent to `mouse=a`.
    pub fn all() -> Self {
        Self {
            normal: true,
            visual: true,
            insert: true,
            command: true,
            help: true,
        }
    }

    /// All modes disabled — equivalent to `set nomouse` / `mouse=`.
    pub fn none() -> Self {
        Self {
            normal: false,
            visual: false,
            insert: false,
            command: false,
            help: false,
        }
    }

    /// Parse a Vim-style flags string (`"a"`, `"nvi"`, `""`, …).
    ///
    /// - `"a"` → all modes on.
    /// - Each char `n/v/i/c/h` enables the corresponding mode.
    /// - Unknown chars are silently ignored (forward-compat).
    /// - Empty string → all modes off.
    pub fn from_flags(s: &str) -> Self {
        if s == "a" {
            return Self::all();
        }
        let mut f = Self::none();
        for c in s.chars() {
            match c {
                'n' => f.normal = true,
                'v' => f.visual = true,
                'i' => f.insert = true,
                'c' => f.command = true,
                'h' => f.help = true,
                'a' => {
                    // 'a' anywhere in string still means all.
                    return Self::all();
                }
                _ => {}
            }
        }
        f
    }

    /// Return a canonical flags string suitable for `:set mouse?` display.
    pub fn as_flags_str(&self) -> String {
        if self.normal && self.visual && self.insert && self.command && self.help {
            return "a".into();
        }
        let mut s = String::new();
        if self.normal {
            s.push('n');
        }
        if self.visual {
            s.push('v');
        }
        if self.insert {
            s.push('i');
        }
        if self.command {
            s.push('c');
        }
        if self.help {
            s.push('h');
        }
        s
    }
}

impl Default for MouseFlags {
    fn default() -> Self {
        Self::all()
    }
}

/// Return `true` when mouse events should be processed for the given Vim mode.
///
/// Used by the event loop at the top of `Event::Mouse` to gate events by mode.
/// Extracted as a pure function so it can be unit-tested without a running App.
pub fn mouse_enabled_for(mode: VimMode, flags: &MouseFlags) -> bool {
    match mode {
        VimMode::Normal => flags.normal,
        VimMode::Visual | VimMode::VisualLine | VimMode::VisualBlock => flags.visual,
        VimMode::Insert => flags.insert,
    }
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

/// Cardinal direction for window navigation (`<C-h/j/k/l>` / `TmuxNavigate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavDir {
    Left,
    Down,
    Up,
    Right,
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
    /// Mouse-hover variant of `Hover` — result goes to the floating
    /// [`HoverPopup`] instead of `info_popup`. Phase 5 mouse support.
    HoverAtMouse {
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
    /// Most recent completed viewport-scoped `RenderOutput` for this buffer.
    /// Cached so a buffer switch can immediately re-install the last known
    /// spans while a fresh parse runs in the background (T3 — per-slot
    /// span cache). `None` until the first viewport parse result arrives.
    pub(crate) viewport_render_output: Option<crate::syntax::RenderOutput>,
    /// Pre-cached spans for the top of the file (`0..min(3*h, line_count)`).
    /// Populated after the first cold viewport parse so `gg` never flashes
    /// un-highlighted rows even on large files.
    pub(crate) top_render_output: Option<crate::syntax::RenderOutput>,
    /// Pre-cached spans for the bottom of the file
    /// (`line_count - min(3*h, line_count)..line_count`). Populated after
    /// the cold viewport parse so `G` never flashes un-highlighted rows.
    pub(crate) bottom_render_output: Option<crate::syntax::RenderOutput>,
    /// Per-row edit log: each entry is `(dirty_gen, row_range)` where
    /// `dirty_gen` is the buffer's `dirty_gen` AFTER the edit landed and
    /// `row_range` is the inclusive row range touched by that edit.
    ///
    /// Used by `merge_render_outputs` so rows untouched since a cache's
    /// parse are still painted from the cache, avoiding the "white flash"
    /// where ALL spans vanish until the background worker returns.
    ///
    /// Capped at 256 entries to bound memory on long sessions.
    pub(crate) dirty_rows_log: Vec<(u64, std::ops::RangeInclusive<usize>)>,
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
    /// Background worker for external formatter invocations (`=` / `==`).
    /// Moves blocking subprocess calls off the UI thread (#118).
    pub(crate) format_worker: hjkl_mangler::FormatWorker,
    /// Buffer ids for which a format job is currently in-flight.
    /// Used to show a "formatting…" status indicator and to skip redundant
    /// submits (the worker's per-buffer dedup is the hard guarantee; this
    /// set is advisory UI state).
    pub(crate) format_pending: HashSet<BufferId>,
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
    /// Count of async syntax results dropped because their tagged
    /// buffer_id no longer matches the active buffer (race: parse
    /// queued before a tab/buffer switch). Surfaced in `:perf` and
    /// asserted in the regression test on the install path.
    pub syntax_stale_drops: u64,
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
    /// Per-mode mouse flags (`:set mouse=<flags>`). Controls which vim modes
    /// process mouse events. Default: all modes enabled (`mouse=a`).
    pub mouse_flags: MouseFlags,
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
    /// Double/triple-click state for mouse support (Phase 1 — issue #114).
    pub(crate) mouse_click_tracker: mouse::MouseClickTracker,
    /// Active right-click context menu (Phase 2, Round A — issue #114).
    /// `None` when no menu is open. Floated above all other content by the
    /// renderer. Dismissed on Esc, click-outside, or action invocation.
    pub(crate) context_menu: Option<crate::menu::ContextMenu>,
    /// Floating LSP hover popup (Phase 5 mouse support).
    /// Shown after the mouse rests on a Code zone for [`HOVER_DELAY`].
    /// Dismissed by mouse move, any key press, or 8-second auto-fade.
    pub(crate) hover_popup: Option<crate::hover_popup::HoverPopup>,
    /// "Mouse has been resting at this cell since `started_at`" tracker.
    /// Reset on any cell change; fires the LSP hover RPC after [`HOVER_DELAY`].
    pub(crate) hover_timer: Option<HoverTimer>,
    /// Active split-border drag state (Phase 9). `Some` while the user is
    /// dragging a split border; `None` otherwise.
    pub(crate) border_drag: Option<BorderDrag>,
    /// Brief visual flash painted over rows touched by the most recent
    /// auto-indent (`=`) operator. `None` when no flash is pending or
    /// after [`INDENT_FLASH_DURATION`] has elapsed. Drained by
    /// [`Self::indent_flash_active`].
    pub(crate) indent_flash: Option<IndentFlash>,
}

/// Tracks how long the mouse has been stationary at a given terminal cell.
/// Used to fire the LSP `textDocument/hover` request after [`HOVER_DELAY`].
pub(crate) struct HoverTimer {
    /// Terminal cell (col, row) the mouse is resting on.
    pub cell: (u16, u16),
    /// When the mouse first arrived at this cell.
    pub started_at: Instant,
    /// `true` once we've fired the LSP hover RPC — prevents re-sending.
    pub request_sent: bool,
}

/// Auto-indent flash duration — single brief on-pulse, no fade, no
/// repeat. 75 ms keeps it snappy and out of the way of further input.
pub(crate) const INDENT_FLASH_DURATION: Duration = Duration::from_millis(75);

/// Visual flash state set immediately after an `=` / `==` / `=G` / Visual-`=`
/// auto-indent operation. The renderer paints a highlight bg over rows
/// `[top, bot]` (inclusive) while `started_at.elapsed() < INDENT_FLASH_DURATION`.
pub(crate) struct IndentFlash {
    pub top: usize,
    pub bot: usize,
    pub started_at: Instant,
}

/// Minimum cell size for each side of a split when drag-resizing (Phase 9).
/// VSplit: each pane must be at least this many columns wide.
/// HSplit: each pane must be at least this many rows tall.
pub(crate) const SPLIT_MIN_SIZE_COLS: u16 = 10;
pub(crate) const SPLIT_MIN_SIZE_ROWS: u16 = 3;

/// Active split-border drag state (Phase 9). Populated on `Down(Left)` when
/// the click lands on a border; cleared on `Up(Left)`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BorderDrag {
    /// Orientation of the split being resized.
    pub orientation: mouse::SplitOrientation,
    /// Origin of the split's rect (x for VSplit, y for HSplit).
    pub split_origin: u16,
    /// Total size of the split's rect (width for VSplit, height for HSplit).
    pub split_total: u16,
    /// Most recent mouse position (column for VSplit, row for HSplit).
    pub last_pos: u16,
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
    syntax.submit_render(
        buffer_id,
        editor.buffer(),
        vp_top,
        vp_height,
        crate::syntax::ParseKind::Viewport,
    );
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
        viewport_render_output: None,
        top_render_output: None,
        bottom_render_output: None,
        dirty_rows_log: Vec::new(),
    };
    slot.snapshot_saved();
    Ok(slot)
}

// build_app_keymap and engine_input_to_key_event moved to keymap_build.rs
// Re-exported here for backwards compatibility with the tests sub-module.
#[cfg(test)]
pub(crate) use keymap_build::engine_input_to_key_event;

impl App {
    /// Clear the LSP hover popup + its arming timer. Called by the
    /// event loop at the top of every mouse-button-down arm so a click
    /// obsoletes the rest-on-symbol state. Without this, a popup armed
    /// at the previous mouse position can leak its cells over the
    /// post-click render (e.g. clicking a menu's "Go to Definition"
    /// item leaves a stale popup floating over the destination buffer).
    pub(crate) fn dismiss_hover_popup_on_click(&mut self) {
        self.hover_popup = None;
        self.hover_timer = None;
    }

    /// Dispatch a middle mouse button down at terminal cell `(col, row)`
    /// based on the zone it lands in:
    ///
    /// - Code / Gutter → X11/Wayland primary-selection paste at the click
    ///   position (silent no-op on platforms without primary selection).
    /// - TabBar → close that tab (vim parity: `:tabclose` on the clicked tab).
    /// - BufferLine → close that buffer (`:bdelete` on the clicked slot —
    ///   refuses with a status message when the buffer is dirty).
    /// - None → no-op.
    pub(crate) fn middle_click(&mut self, col: u16, row: u16) {
        match mouse::hit_test_zone(self, col, row) {
            mouse::Zone::TabBar { tab_idx } => {
                // Switch to the clicked tab so do_tabclose targets it,
                // then close.
                if tab_idx != self.active_tab {
                    self.sync_viewport_from_editor();
                    self.active_tab = tab_idx;
                    self.sync_viewport_to_editor();
                }
                self.do_tabclose();
            }
            mouse::Zone::BufferLine { slot_idx } => {
                // Switch to the clicked slot so buffer_delete targets it.
                if slot_idx != self.focused_slot_idx() {
                    self.switch_to(slot_idx);
                }
                self.buffer_delete(false);
            }
            mouse::Zone::Code { .. } | mouse::Zone::Gutter { .. } => {
                self.middle_click_paste_primary(col, row);
            }
            mouse::Zone::None
            | mouse::Zone::StatusLine
            | mouse::Zone::SplitBorder { .. }
            | mouse::Zone::PickerRow { .. } => {}
        }
    }

    /// Primary-selection paste at terminal cell `(col, row)`. Pulled out
    /// of [`Self::middle_click`] so the Code-zone path is independently
    /// expressible (and so the X11/Wayland-only branch is grep-able).
    fn middle_click_paste_primary(&mut self, col: u16, row: u16) {
        use hjkl_clipboard::{Capabilities, MimeType, Selection};

        let Some(win_id) = mouse::hit_test_window(self, col, row) else {
            return;
        };
        let Some((doc_row, doc_col)) = mouse::cell_to_doc(self, win_id, col, row) else {
            return;
        };

        // Read primary selection BEFORE any mut borrows of self.
        let primary_text: Option<String> = {
            let cb = self.active().editor.host().clipboard();
            cb.filter(|cb| {
                cb.capabilities().contains(Capabilities::PRIMARY)
                    && cb.capabilities().contains(Capabilities::READ)
            })
            .and_then(|cb| {
                cb.get(Selection::Primary, MimeType::Text)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
            })
        };

        let current_focus = self.focused_window();
        if win_id != current_focus {
            self.sync_viewport_from_editor();
            self.set_focused_window(win_id);
            self.sync_viewport_to_editor();
        }

        self.active_mut().editor.mouse_click_doc(doc_row, doc_col);
        self.sync_after_engine_mutation();

        if let Some(text) = primary_text {
            self.active_mut().editor.set_yank(text);
            self.active_mut().editor.paste_after(1);
            self.sync_after_engine_mutation();
        }
    }

    /// Focus the window under `(col, row)` and move its cursor to the
    /// clicked doc-position. Used at the top of the right-click handler
    /// so menu actions (Go to Definition, Rename, etc.) operate on the
    /// symbol under the mouse — not on the keyboard cursor's previous
    /// position.
    ///
    /// Preserves an active visual selection: when the user has a visual
    /// range up and right-clicks, the selection stays intact so Cut /
    /// Copy work on it. Without a selection, the cursor moves to the
    /// clicked cell. Gutter clicks move to `(doc_row, 0)`.
    pub(crate) fn move_cursor_for_right_click(&mut self, col: u16, row: u16) {
        use hjkl_engine::VimMode;
        let has_sel = matches!(
            self.active().editor.vim_mode(),
            VimMode::Visual | VimMode::VisualLine | VimMode::VisualBlock
        );
        if has_sel {
            return;
        }
        let zone = mouse::hit_test_zone(self, col, row);
        let win_id = match mouse::hit_test_window(self, col, row) {
            Some(w) => w,
            None => return,
        };
        let current_focus = self.focused_window();
        if win_id != current_focus {
            self.sync_viewport_from_editor();
            self.set_focused_window(win_id);
            self.sync_viewport_to_editor();
        }
        let target = match zone {
            mouse::Zone::Code {
                doc_row, doc_col, ..
            } => Some((doc_row, doc_col)),
            mouse::Zone::Gutter { doc_row, .. } => Some((doc_row, 0)),
            _ => None,
        };
        if let Some((doc_row, doc_col)) = target {
            self.active_mut().editor.mouse_click_doc(doc_row, doc_col);
            self.sync_after_engine_mutation();
        }
    }

    /// `true` when a blocking overlay is on top of the editor — context
    /// menu, picker, command/search field, info popup. Used to gate
    /// background features that shouldn't fire while the user is
    /// interacting with the overlay (notably the LSP hover popup, which
    /// would otherwise show through the menu for whatever doc text the
    /// mouse cell happens to sit over).
    pub(crate) fn overlay_active(&self) -> bool {
        self.context_menu.is_some()
            || self.picker.is_some()
            || self.command_field.is_some()
            || self.search_field.is_some()
            || self.info_popup.is_some()
    }

    /// Full-screen rect for clamping popups / context menus to the
    /// terminal area. Matches the layout `render::frame` computes:
    /// optional top bar (tabs + buffer line, when multiple slots OR
    /// tabs are open) + editor viewport + bottom status line.
    ///
    /// MUST include the top bar when it's visible — otherwise this
    /// underestimates total height by 1 row and a popup anchored near
    /// the bottom flips one row too soon, putting the
    /// `Moved`-handler's row→item math out of sync with what
    /// `bounding_rect` produces at render time.
    pub(crate) fn screen_rect(&self) -> ratatui::layout::Rect {
        let vp = self.active().editor.host().viewport();
        let show_top_bar = self.tabs.len() > 1 || self.slots.len() > 1;
        let top_bar_h = if show_top_bar { TOP_BAR_HEIGHT } else { 0 };
        ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: vp.width,
            height: top_bar_h + vp.height + STATUS_LINE_HEIGHT,
        }
    }

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
            // Record which rows were touched by this batch of edits so the
            // merger can keep untouched cache rows visible while the worker
            // parses the new content.  The dirty_gen AFTER the edit is the
            // current one — any cache older than this gen is stale for these
            // rows and should show blank until the worker returns.
            let new_dg = self.active().editor.buffer().dirty_gen();
            for edit in &edits {
                let start_row = edit.start_position.0 as usize;
                let end_row =
                    (edit.old_end_position.0 as usize).max(edit.new_end_position.0 as usize);
                self.active_mut()
                    .dirty_rows_log
                    .push((new_dg, start_row..=end_row));
            }
            // Cap to 256 entries to avoid unbounded growth.
            const DIRTY_LOG_CAP: usize = 256;
            let log = &mut self.active_mut().dirty_rows_log;
            if log.len() > DIRTY_LOG_CAP {
                let drain_count = log.len() - DIRTY_LOG_CAP;
                log.drain(..drain_count);
            }
        }
        self.lsp_notify_change_active();
        self.recompute_and_install();
    }

    /// Return the active auto-indent flash row range `(top, bot)` while
    /// `started_at.elapsed() < INDENT_FLASH_DURATION`, otherwise clear
    /// the stored flash and return `None`.
    ///
    /// Renderer calls this every frame; event-loop tick also calls it to
    /// expire the flash even when no key is pressed.
    pub(crate) fn indent_flash_active(&mut self) -> Option<(usize, usize)> {
        let elapsed = self.indent_flash.as_ref().map(|f| f.started_at.elapsed())?;
        if elapsed >= INDENT_FLASH_DURATION {
            self.indent_flash = None;
            return None;
        }
        self.indent_flash.as_ref().map(|f| (f.top, f.bot))
    }

    // ── External formatter dispatch (hjkl-mangler) ───────────────────────

    /// Walk up from `start` looking for a project-root marker file.
    ///
    /// Markers: `.git`, `Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`,
    /// `setup.py`, `composer.json`, `.hg`.  Returns the first directory that
    /// contains one of these files, or `start` itself as a fallback.
    fn find_project_root(start: &std::path::Path) -> std::path::PathBuf {
        const MARKERS: &[&str] = &[
            ".git",
            "Cargo.toml",
            "package.json",
            "go.mod",
            "pyproject.toml",
            "setup.py",
            "composer.json",
            ".hg",
        ];
        let mut dir = start.to_owned();
        loop {
            for marker in MARKERS {
                if dir.join(marker).exists() {
                    return dir;
                }
            }
            match dir.parent() {
                Some(p) => dir = p.to_owned(),
                None => return start.to_owned(),
            }
        }
    }

    /// Try to format the active buffer using an external formatter.
    ///
    /// **BLOCKS the calling thread for up to 2 seconds.**  This is a
    /// synchronous subprocess invocation.  Async invocation is tracked in #118.
    ///
    /// Returns `true` if the formatter ran successfully and the buffer was
    /// Submit an async format job for the active buffer.
    ///
    /// Returns `true` when a formatter was found and the job was submitted
    /// (caller should skip the dumb `auto_indent_range` fallback and wait
    /// for `poll_format_results` to install the result).
    ///
    /// Returns `false` when no formatter is registered for the active
    /// buffer's extension — caller should run the dumb fallback immediately.
    pub(crate) fn submit_external_format(
        &mut self,
        range: Option<hjkl_mangler::RangeSpec>,
    ) -> bool {
        use hjkl_mangler::{formatter_for_path, probe_tool};

        let filename = self.active().filename.clone();
        let Some(ref path) = filename else {
            return false;
        };

        let Some(formatter) = formatter_for_path(path) else {
            return false;
        };

        // Probe binary availability up-front so a missing formatter
        // (prettier on a fresh box opening a .md, etc.) silently falls
        // through to the dumb-algo path instead of dispatching a worker
        // job that would surface as a noisy "prettier: not installed".
        let tool_name = formatter.tool_name().to_owned();
        if let Err(why) = probe_tool(&tool_name) {
            tracing::debug!(
                tool = %tool_name,
                reason = %why,
                "formatter probe failed; falling back to dumb algo"
            );
            // Surface the *real* reason so the user can tell apart
            // "binary not on PATH" from "wrapper script exits non-zero".
            self.status_message = Some(format!("{tool_name} probe: {why}"));
            return false;
        }

        let source = std::sync::Arc::new(self.active().editor.buffer().as_string());
        let dirty_gen = self.active().editor.buffer().dirty_gen();
        let buffer_id = self.active().buffer_id;

        let parent = path
            .parent()
            .map(|p| p.to_owned())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let project_root = Self::find_project_root(&parent);

        tracing::debug!(
            file = %path.display(),
            root = %project_root.display(),
            buffer_id,
            dirty_gen,
            "submitting async format job"
        );

        self.format_worker.submit(hjkl_mangler::FormatJob {
            buffer_id,
            source,
            project_root,
            formatter,
            dirty_gen,
            range,
        });

        self.format_pending.insert(buffer_id);
        self.status_message = Some(format!("{tool_name}: formatting\u{2026}"));

        // Arm the visual flash *immediately* on submit — the user sees
        // confirmation that `=` was accepted without waiting for the
        // (possibly multi-second) formatter to complete. Range is the
        // currently-visible viewport rows, so it covers whatever the
        // user is looking at.
        let vp = self.active().editor.host().viewport();
        let line_count = self.active().editor.buffer().row_count();
        let top = vp.top_row;
        let height = vp.height as usize;
        let bot = (top + height.saturating_sub(1)).min(line_count.saturating_sub(1));
        self.indent_flash = Some(IndentFlash {
            top,
            bot,
            started_at: Instant::now(),
        });
        true
    }

    /// Drain completed format results from the worker and install them.
    ///
    /// Called once per event-loop tick alongside `poll_git_signs` /
    /// `drain_lsp_events`. Returns `true` when at least one result was
    /// installed and a redraw is needed.
    pub(crate) fn poll_format_results(&mut self) -> bool {
        let mut redraw = false;
        while let Some(result) = self.format_worker.try_recv() {
            self.format_pending.remove(&result.buffer_id);

            // Find the slot — may have been closed since submit; drop if so.
            let Some(slot_idx) = self
                .slots
                .iter()
                .position(|s| s.buffer_id == result.buffer_id)
            else {
                tracing::debug!(
                    buffer_id = result.buffer_id,
                    "format result for closed buffer; dropping"
                );
                continue;
            };

            // Stale check: if the buffer was mutated after the job was
            // submitted, drop the result — the user will re-trigger `=`.
            let current_dg = self.slots[slot_idx].editor.buffer().dirty_gen();
            if current_dg != result.dirty_gen {
                tracing::debug!(
                    buffer_id = result.buffer_id,
                    submitted_gen = result.dirty_gen,
                    current_gen = current_dg,
                    "format result stale; dropping"
                );
                // Clear the "formatting…" status only if it's still ours.
                if self
                    .status_message
                    .as_deref()
                    .is_some_and(|m| m.ends_with("formatting\u{2026}"))
                {
                    self.status_message = None;
                }
                continue;
            }

            match result.result {
                Ok(formatted) => {
                    // Native-range formatters (prettier, stylua, ruff) return the whole
                    // file with only the in-range region reformatted. Whole-file formatters
                    // return the fully-reformatted file. Either way install directly — no
                    // diff-splice post-processing needed.
                    let content = formatted
                        .strip_suffix('\n')
                        .unwrap_or(&formatted)
                        .to_owned();
                    // set_content_undoable so the engine pushes the pre-format
                    // buffer state onto the undo stack first — the user can
                    // press `u` to revert the formatter's changes as a single
                    // undo step. pending_content_reset is set inside, which
                    // sync_after_engine_mutation picks up for the syntax layer.
                    self.slots[slot_idx].editor.set_content_undoable(&content);

                    // Note: the indent flash was armed at submit time in
                    // `submit_external_format` so the user gets immediate
                    // feedback. We don't re-arm here — that would push the
                    // flash window past the formatter latency on big files.

                    // Clear the "formatting…" status.
                    if self
                        .status_message
                        .as_deref()
                        .is_some_and(|m| m.ends_with("formatting\u{2026}"))
                    {
                        self.status_message = None;
                    }

                    // Propagate dirty/syntax/LSP state — same as the old sync path.
                    // Only do this when the formatted slot is the active one,
                    // otherwise we'd pollute the active editor's syntax state.
                    let active_bid = self.active().buffer_id;
                    if result.buffer_id == active_bid {
                        self.sync_after_engine_mutation();
                    }

                    redraw = true;
                    tracing::debug!(buffer_id = result.buffer_id, "format result installed");
                }
                Err(hjkl_mangler::FormatError::NotInstalled(name)) => {
                    self.status_message = Some(format!("{name}: not installed"));
                    redraw = true;
                }
                Err(e) => {
                    self.status_message = Some(format!("formatter: {e}"));
                    redraw = true;
                }
            }
        }
        redraw
    }

    // flush_pending_count_to_engine moved to count_prefix.rs

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

    /// Resize the split whose `last_rect` encompasses `split_origin` and
    /// `split_total` so the boundary sits at `split_pos` cells from the
    /// split origin. `split_pos` is clamped to leave at least
    /// `SPLIT_MIN_SIZE_COLS` / `SPLIT_MIN_SIZE_ROWS` on each side.
    ///
    /// Called by the border-drag handler in the event loop (Phase 9).
    /// `orientation` determines whether we're moving a column (VSplit) or
    /// a row (HSplit) boundary.
    pub(crate) fn resize_split_to(
        &mut self,
        orientation: mouse::SplitOrientation,
        split_origin: u16,
        split_total: u16,
        split_pos: u16,
    ) {
        use window::SplitDir;

        let min_size = match orientation {
            mouse::SplitOrientation::Vertical => SPLIT_MIN_SIZE_COLS,
            mouse::SplitOrientation::Horizontal => SPLIT_MIN_SIZE_ROWS,
        };

        if split_total < min_size * 2 + 1 {
            return; // too small to resize
        }

        // Clamp split_pos so both children stay at least min_size.
        let clamped = split_pos.clamp(min_size, split_total.saturating_sub(min_size + 1));
        let new_ratio = clamped as f32 / split_total as f32;
        let new_ratio = new_ratio.clamp(0.01, 0.99);

        // Find the matching split node by walking the layout tree and looking
        // for a Split whose last_rect matches the origin + total we recorded
        // when the drag started.
        let dir = match orientation {
            mouse::SplitOrientation::Vertical => SplitDir::Vertical,
            mouse::SplitOrientation::Horizontal => SplitDir::Horizontal,
        };
        fn update_matching(
            node: &mut window::LayoutTree,
            dir: window::SplitDir,
            origin: u16,
            total: u16,
            new_ratio: f32,
        ) {
            if let window::LayoutTree::Split {
                dir: my_dir,
                ratio,
                a,
                b,
                last_rect,
            } = node
            {
                if *my_dir == dir
                    && let Some(r) = last_rect
                {
                    let (rect_origin, rect_total) = match dir {
                        window::SplitDir::Vertical => (r.x, r.width),
                        window::SplitDir::Horizontal => (r.y, r.height),
                    };
                    if rect_origin == origin && rect_total == total {
                        *ratio = new_ratio;
                        return; // found the target; done
                    }
                }
                update_matching(a, dir, origin, total, new_ratio);
                update_matching(b, dir, origin, total, new_ratio);
            }
        }
        update_matching(self.layout_mut(), dir, split_origin, split_total, new_ratio);
    }

    /// Equalize all splits (set every ratio to 0.5). Used by double-click on a
    /// border (Phase 9). Delegates to the existing `equalize_layout`.
    pub(crate) fn equalize_split(&mut self) {
        self.equalize_layout();
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
            format_worker: hjkl_mangler::FormatWorker::spawn(),
            format_pending: HashSet::new(),
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
            syntax_stale_drops: 0,
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
            mouse_flags: MouseFlags::all(),
            app_keymap: keymap_build::build_app_keymap(default_leader),
            anvil_pool: hjkl_anvil::InstallPool::new(),
            anvil_handles: HashMap::new(),
            anvil_log: HashMap::new(),
            anvil_registry: hjkl_anvil::Registry::embedded().ok(),
            pending_state: None,
            last_ex_command: None,
            mouse_click_tracker: mouse::MouseClickTracker::new(),
            context_menu: None,
            hover_popup: None,
            hover_timer: None,
            border_drag: None,
            indent_flash: None,
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
        self.app_keymap = keymap_build::build_app_keymap(leader);
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

    // ── Context menu keyboard dispatch (Phase 2, Round A) ────────────────

    /// Handle a keypress while the context menu is open.
    ///
    /// Returns `true` if the key was consumed by the menu (caller should
    /// `continue` the event loop). Returns `false` when the key is not a
    /// menu-nav key — caller should then dismiss the menu and fall through
    /// to normal dispatch.
    pub(crate) fn handle_context_menu_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        match key.code {
            // Navigation.
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(ref mut m) = self.context_menu {
                    m.move_up();
                }
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(ref mut m) = self.context_menu {
                    m.move_down();
                }
                true
            }
            // Confirm.
            KeyCode::Enter => {
                let action = self.context_menu.as_ref().and_then(|m| m.selected_action());
                self.context_menu = None;
                if let Some(act) = action {
                    self.invoke_menu_action(act);
                }
                true
            }
            // Dismiss.
            KeyCode::Esc => {
                self.context_menu = None;
                true
            }
            // Any other key: caller dismisses and falls through.
            _ => false,
        }
    }

    /// Execute a [`crate::menu::MenuAction`] selected from the context menu.
    pub(crate) fn invoke_menu_action(&mut self, action: crate::menu::MenuAction) {
        use crate::menu::MenuAction;
        match action {
            MenuAction::Copy => self.menu_copy(),
            MenuAction::Cut => self.menu_cut(),
            MenuAction::Paste => self.menu_paste(),
            MenuAction::TabClose => self.dispatch_ex("tabclose"),
            MenuAction::TabCloseOthers => self.do_tabonly(),
            MenuAction::TabCloseRight => self.close_tabs_to_right(),
            MenuAction::TabCloseLeft => self.close_tabs_to_left(),
            // ── LSP actions (Phase 2, Round B) ───────────────────────────────
            MenuAction::LspGotoDefinition => self.lsp_goto_definition(),
            MenuAction::LspGotoReferences => self.lsp_goto_references(),
            MenuAction::LspHover => self.lsp_hover(),
            MenuAction::LspCodeActions => self.lsp_code_actions(),
            MenuAction::LspFormat => self.lsp_format(),
            // Rename needs a new name from the user.  The ex command
            // `:Rename <newname>` is the supported entry point — mirror the
            // same status-message prompt the `<leader>rn` keybind uses so the
            // user knows how to proceed.
            MenuAction::LspRename => {
                self.status_message = Some("use :Rename <newname> to rename".into());
            }
            // ── Phase 7: status-line menu actions ────────────────────────────
            MenuAction::LspRestart => self.restart_lsp(),
            MenuAction::OpenFilePicker => self.open_picker(),
            // ── Phase 7: split-border menu actions ───────────────────────────
            MenuAction::WindowEqualize => self.equalize_layout(),
            MenuAction::WindowClose => self.dispatch_ex("close"),
            // ── Phase 8: picker overlay menu actions ──────────────────────────
            MenuAction::PickerOpen => self.picker_accept(),
            MenuAction::PickerOpenSplit => self.picker_open_in_split(),
            MenuAction::PickerOpenVSplit => self.picker_open_in_vsplit(),
            MenuAction::PickerOpenTab => self.picker_open_in_tab(),
            MenuAction::PickerCopyPath => self.picker_copy_path(),
            MenuAction::Separator | MenuAction::Info => {} // no-op
        }
    }

    // ── Menu clipboard actions (Phase 2, Round A) ─────────────────────────

    /// Right-click Copy action.
    ///
    /// If a visual selection is active, yank the selection into the unnamed
    /// register (which the engine already mirrors to the system clipboard via
    /// `Host::write_clipboard`). If no selection is active, yank the current
    /// line (same as `yy` / `Y` line-yank semantics).
    pub(crate) fn menu_copy(&mut self) {
        use hjkl_engine::{RangeKind, VimMode};
        let vim_mode = self.active().editor.vim_mode();
        match vim_mode {
            VimMode::VisualBlock => {
                if let Some((top_row, bot_row, left_col, right_col)) =
                    self.active().editor.block_highlight()
                {
                    self.active_mut()
                        .editor
                        .yank_block(top_row, bot_row, left_col, right_col, '"');
                }
            }
            VimMode::Visual => {
                if let Some((start, end)) = self.active().editor.char_highlight() {
                    self.active_mut()
                        .editor
                        .yank_range(start, end, RangeKind::Inclusive, '"');
                }
            }
            VimMode::VisualLine => {
                if let Some((top_row, bot_row)) = self.active().editor.line_highlight() {
                    self.active_mut().editor.yank_range(
                        (top_row, 0),
                        (bot_row, usize::MAX),
                        RangeKind::Linewise,
                        '"',
                    );
                }
            }
            _ => {
                // No selection — yank current line (yy semantics).
                self.active_mut().editor.yank_to_eol(1);
            }
        }
        self.sync_after_engine_mutation();
    }

    /// Right-click Cut action.
    ///
    /// Identical to [`menu_copy`] but also deletes the yanked region.
    /// On a visual selection this calls the appropriate `delete_range` /
    /// `delete_block` path. Without a selection it yanks and deletes the
    /// current line (`dd` semantics).
    pub(crate) fn menu_cut(&mut self) {
        use hjkl_engine::{RangeKind, VimMode};
        let vim_mode = self.active().editor.vim_mode();
        match vim_mode {
            VimMode::VisualBlock => {
                if let Some((top_row, bot_row, left_col, right_col)) =
                    self.active().editor.block_highlight()
                {
                    self.active_mut()
                        .editor
                        .delete_block(top_row, bot_row, left_col, right_col, '"');
                    // Exit visual mode.
                    use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
                    hjkl_vim::handle_key(
                        &mut self.active_mut().editor,
                        CtKeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                    );
                }
            }
            VimMode::Visual => {
                if let Some((start, end)) = self.active().editor.char_highlight() {
                    self.active_mut()
                        .editor
                        .delete_range(start, end, RangeKind::Inclusive, '"');
                    use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
                    hjkl_vim::handle_key(
                        &mut self.active_mut().editor,
                        CtKeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                    );
                }
            }
            VimMode::VisualLine => {
                if let Some((top_row, bot_row)) = self.active().editor.line_highlight() {
                    self.active_mut().editor.delete_range(
                        (top_row, 0),
                        (bot_row, usize::MAX),
                        RangeKind::Linewise,
                        '"',
                    );
                    use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
                    hjkl_vim::handle_key(
                        &mut self.active_mut().editor,
                        CtKeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                    );
                }
            }
            _ => {
                // No selection — delete current line (dd semantics):
                // yank_to_eol then delete_to_eol is not quite right for full-line;
                // use the engine's delete_range for the full current row.
                let (row, _) = self.active().editor.cursor();
                self.active_mut().editor.delete_range(
                    (row, 0),
                    (row, usize::MAX),
                    hjkl_engine::RangeKind::Linewise,
                    '"',
                );
            }
        }
        self.sync_after_engine_mutation();
    }

    /// Right-click Paste action.
    ///
    /// Reads the system clipboard into the unnamed register (so the engine's
    /// `p` command sees fresh content) and then performs a `paste_after`.
    pub(crate) fn menu_paste(&mut self) {
        // Pull from system clipboard → unnamed register so paste_after uses it.
        if let Some(text) = self.active_mut().editor.host_mut().read_clipboard() {
            self.active_mut().editor.set_yank(text);
        }
        self.active_mut().editor.paste_after(1);
        self.sync_after_engine_mutation();
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
    /// app actions. Routing by domain — each cluster delegates to a focused
    /// sub-dispatcher that lives in the corresponding glue module:
    ///   - picker opens    → inline (3 one-liners)
    ///   - git actions     → `picker_glue::dispatch_git_action`
    ///   - LSP actions     → `lsp_glue::dispatch_lsp_action`
    ///   - window actions  → `window::dispatch_window_action` (incl. TmuxNavigate)
    ///   - buffer actions  → `buffer_ops::dispatch_buffer_action`
    ///   - prompt actions  → `prompt::dispatch_prompt_action`
    ///   - pending-state   → `pending_actions::dispatch_pending_state_action`
    ///   - engine actions  → `engine_actions::dispatch_engine_action`
    ///   - QuitOrClose     → inline (app-lifecycle, 5 LOC)
    pub fn dispatch_action(&mut self, action: AppAction, count: u32) {
        let count = count.max(1) as usize;
        match action {
            // ── File / buffer pickers (open) ───────────────────────────────
            AppAction::OpenFilePicker => self.open_picker(),
            AppAction::OpenBufferPicker => self.open_buffer_picker(),
            AppAction::OpenGrepPicker => self.open_grep_picker(None),

            // ── Git picker openers ─────────────────────────────────────────
            AppAction::GitStatus
            | AppAction::GitLog
            | AppAction::GitBranch
            | AppAction::GitFileHistory
            | AppAction::GitStashes
            | AppAction::GitTags
            | AppAction::GitRemotes => self.dispatch_git_action(action),

            // ── LSP + diagnostic navigation ────────────────────────────────
            AppAction::ShowDiagAtCursor
            | AppAction::LspCodeActions
            | AppAction::LspRename
            | AppAction::LspGotoDef
            | AppAction::LspGotoDecl
            | AppAction::LspGotoRef
            | AppAction::LspGotoImpl
            | AppAction::LspGotoTypeDef
            | AppAction::LspHover
            | AppAction::DiagNext
            | AppAction::DiagPrev
            | AppAction::DiagNextError
            | AppAction::DiagPrevError => self.dispatch_lsp_action(action),

            // ── Window / layout management ─────────────────────────────────
            AppAction::FocusLeft
            | AppAction::FocusBelow
            | AppAction::FocusAbove
            | AppAction::FocusRight
            | AppAction::FocusNext
            | AppAction::FocusPrev
            | AppAction::CloseFocusedWindow
            | AppAction::OnlyFocusedWindow
            | AppAction::SwapWithSibling
            | AppAction::MoveWindowToNewTab
            | AppAction::NewSplit
            | AppAction::ResizeHeight(_)
            | AppAction::ResizeWidth(_)
            | AppAction::EqualizeLayout
            | AppAction::MaximizeHeight
            | AppAction::MaximizeWidth
            | AppAction::TmuxNavigate(_) => self.dispatch_window_action(action, count),

            // ── Buffer / tab navigation ────────────────────────────────────
            AppAction::Tabnext
            | AppAction::Tabprev
            | AppAction::BufferNext
            | AppAction::BufferPrev
            | AppAction::BufferAlt
            | AppAction::BufferCycleH
            | AppAction::BufferCycleL => self.dispatch_buffer_action(action, count),

            // ── Prompt / overlay entry ─────────────────────────────────────
            AppAction::OpenCommandPrompt | AppAction::OpenSearchPrompt(_) => {
                self.dispatch_prompt_action(action)
            }

            // ── Pending-state chords ───────────────────────────────────────
            AppAction::BeginPendingReplace { .. }
            | AppAction::BeginPendingFind { .. }
            | AppAction::BeginPendingAfterG { .. }
            | AppAction::BeginPendingAfterZ { .. }
            | AppAction::BeginPendingAfterOp { .. }
            | AppAction::BeginPendingSelectRegister
            | AppAction::BeginPendingSetMark
            | AppAction::BeginPendingGotoMarkLine
            | AppAction::BeginPendingGotoMarkChar
            | AppAction::QChord { .. }
            | AppAction::BeginPendingPlayMacro { .. } => self.dispatch_pending_state_action(action),

            // ── App lifecycle ──────────────────────────────────────────────
            AppAction::QuitOrClose => {
                if self.layout().leaves().len() > 1 {
                    self.close_focused_window();
                } else {
                    self.exit_requested = true;
                }
            }

            // ── Engine-mutating actions ────────────────────────────────────
            _ => self.dispatch_engine_action(action, count),
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

    // km_to_crossterm, replay_to_engine, route_chord_key, route_chord_key_inner moved to chord_routing.rs

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
