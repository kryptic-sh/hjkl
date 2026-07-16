//! `App` — owns the editor + host, drives the event loop.

use anyhow::Result;
use hjkl_buffer::View;
use hjkl_engine::{BufferEdit, Host};
use hjkl_engine::{CoarseMode, CursorShape, Editor, Options};
use hjkl_engine_tui::EditorRatatuiExt;
use hjkl_form::TextFieldEditor;
use hjkl_holler::HollerBus;
use hjkl_keymap::Keymap;
use hjkl_vim::VimEditorExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crate::keymap_actions::AppAction;

use crate::host::TuiHost;
use crate::syntax::{self, BufferId, SyntaxLayer};
use hjkl_app::git_worker::{BlameWorker, GitSignsWorker};
use std::collections::HashSet;

mod buffer_ops;
pub(crate) mod chord_routing;
mod confirm_substitute;
pub(crate) mod count_prefix;
mod diff;
pub(crate) mod diff_mode;
mod dispatch;
mod engine_actions;
mod event_loop;
mod ex_dispatch;
pub(crate) mod ex_host_cmds;
pub(crate) mod explorer;
pub(crate) mod explorer_reconcile;
mod fs_watch;
pub(crate) mod git_hunks;
pub(crate) mod hop;
pub(crate) mod keymap;
pub(crate) mod keymap_build;
pub mod lsp_glue;
pub(crate) mod mappings_dispatch;
pub mod mouse;
mod pending_actions;
mod picker_glue;
mod prompt;
pub(crate) mod quickfix;
mod syntax_glue;
#[cfg(test)]
mod tests;
mod types;
mod viewport_sync;
pub mod window;

use crate::completion::Completion;
use hjkl_info_popup::InfoPopup;

pub(crate) use types::BufferFeatures;
pub use types::{
    BufferSlot, DiagSeverity, DiskState, LspDiag, LspPendingRequest, LspServerInfo, MouseFlags,
    mouse_enabled_for,
};

/// Height reserved for the status line at the bottom of the screen.
pub const STATUS_LINE_HEIGHT: u16 = 1;

/// Which history ring feeds a command-line window (issue #37).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdLineKind {
    /// `q:` — ex command history.
    Ex,
    /// `q/` — forward-search history.
    SearchForward,
    /// `q?` — backward-search history.
    SearchBackward,
}

/// Transient state attached to an open command-line window.
/// Keeps the window-id and kind so `<CR>` can re-dispatch correctly.
#[derive(Debug, Clone)]
pub struct CmdLineWindow {
    /// The `WindowId` of the cmdline window in the active tab's layout.
    pub win_id: window::WindowId,
    /// The slot index that backs the transient buffer.
    pub slot_idx: usize,
    /// Which history ring this window shows.
    pub kind: CmdLineKind,
}

impl From<crate::keymap_actions::CmdLineWindowKind> for CmdLineKind {
    fn from(k: crate::keymap_actions::CmdLineWindowKind) -> Self {
        use crate::keymap_actions::CmdLineWindowKind as K;
        match k {
            K::Ex => Self::Ex,
            K::SearchForward => Self::SearchForward,
            K::SearchBackward => Self::SearchBackward,
        }
    }
}

/// Height of the unified top bar (buffers left, tabs right) at the top of the
/// screen, when shown (either more than one slot or more than one tab).
pub const TOP_BAR_HEIGHT: u16 = 1;

/// Close glyph appended to every tab and buffer-line entry. 1 display column,
/// 3 UTF-8 bytes — all width math must use `.chars().count()`.
pub(crate) const TAB_CLOSE_GLYPH: char = '✕';

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

/// Re-exported from `hjkl_app::keymap_actions` — source of truth moved there.
pub use hjkl_app::keymap_actions::NavDir;
/// Re-exported from `hjkl_app::keymap_actions` — source of truth moved there.
pub use hjkl_app::keymap_actions::SearchDir;

/// Rebase a document row against one `ContentEdit` (#151 Phase E). The edit
/// replaced rows `[start, old_end]` with `[start, new_end]`:
/// - above the change (`row <= start`) → unchanged;
/// - inside it (`start < row <= old_end`) → clamp to the new end;
/// - below it (`row > old_end`) → shift by the net row delta.
fn rebase_row_for_edit(row: usize, start: usize, old_end: usize, new_end: usize) -> usize {
    if row <= start {
        row
    } else if row <= old_end {
        row.min(new_end)
    } else {
        (row as isize + (new_end as isize - old_end as isize)).max(0) as usize
    }
}

/// Active smooth-scroll animation (#195). Render-only: the window editor's
/// real viewport top is already at `target_top`; this interpolates the
/// RENDERED top from `start_top` over `duration`.
pub(crate) struct ScrollAnim {
    pub win_id: window::WindowId,
    pub start_top: usize,
    pub target_top: usize,
    pub started_at: std::time::Instant,
    pub duration: std::time::Duration,
}

/// Top-level application state. Everything the event loop and renderer need.
pub struct App {
    /// All open buffer slots. Never empty — always at least one slot.
    slots: Vec<BufferSlot>,
    /// Window list. Indexed by `WindowId`. Entries are `Option<AppWindow>`;
    /// closed windows are set to `None` so ids stay stable.
    /// Each `AppWindow` stores the per-window cursor/scroll snapshot
    /// (authoritative at all times). The slot editor's cursor/scroll are only
    /// synced on focus changes, not before every keypress.
    pub windows: Vec<Option<window::AppWindow>>,
    /// Per-window fold open/closed state, keyed by `WindowId` (window-level
    /// folds). The shared slot buffer only ever holds the *focused* window's
    /// fold set — it is installed on focus-in and saved back after dispatch via
    /// `sync_viewport_to_editor` / `sync_viewport_from_editor`. Unfocused
    /// windows render against their snapshot here. Kept app-side (not on the
    /// renderer-agnostic layout `Window`) so `hjkl-layout` stays buffer-free.
    /// `WindowId`s are monotonic and never reused, so stale entries can't
    /// collide; closed windows are pruned on the main close paths.
    pub window_folds: std::collections::HashMap<window::WindowId, Vec<hjkl_buffer::Fold>>,
    /// Per-window editor, keyed by `WindowId` (#151 Phase D). Each is a
    /// [`View::new_view`] of its slot's shared `Buffer`, so it owns an
    /// independent cursor / viewport / vim FSM while editing the same document.
    /// Invariant: a key exists here iff `windows[id]` is `Some`. The slot's own
    /// editor is retained as a content bridge during the migration (Stage 2b
    /// removes it); content reads via either editor agree because they share
    /// the same `Arc<Mutex<Buffer>>`.
    pub(crate) window_editors: std::collections::HashMap<window::WindowId, Editor<View, TuiHost>>,
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
    /// Notification bus — collects all info/warn/error toasts pushed during
    /// this session. Replaces the old single-slot `status_message` field.
    /// Rendered as a floating stack in the top-right corner by
    /// `hjkl_holler_tui::render_active`.
    pub bus: HollerBus,
    /// Multi-line info popup (e.g. from `:reg`, `:marks`, `:jumps`,
    /// `:changes`, or the K-key LSP hover path). When `Some`, rendered as a
    /// centered overlay; any keypress dismisses it without dispatching to the
    /// editor.
    pub info_popup: Option<InfoPopup>,
    /// Active `:` command input. `Some` while the user is typing an ex
    /// command. Backed by a vim-grammar [`TextFieldEditor`] so motions
    /// (h/l/w/b/dw/diw/...) work inside the prompt.
    pub command_field: Option<TextFieldEditor>,
    /// Byte replace-range in the command field for the active `:` completion
    /// popup. Set whenever the popup is populated; cleared when the prompt
    /// closes or the popup is dismissed. The accept path uses this to know
    /// which token to replace in the field text.
    pub(crate) command_completion_range: Option<std::ops::Range<usize>>,
    /// Active `!` filter prompt. `Some` while the user is typing a shell
    /// command after a `!{motion}` or `!!` operator. Paired with
    /// `filter_pending_range` which holds the row range to filter.
    pub(crate) filter_field: Option<hjkl_form::TextFieldEditor>,
    /// Row range `(top, bot)` (inclusive) waiting for a shell command from
    /// the `!` filter prompt. Set when `filter_field` opens; cleared on
    /// submit or cancel.
    pub(crate) filter_pending_range: Option<(usize, usize)>,
    /// Active `/` (forward) / `?` (backward) search prompt.
    pub search_field: Option<TextFieldEditor>,
    /// Active picker overlay (file, buffer, grep, …).
    pub picker: Option<crate::picker::Picker>,
    /// Left file-explorer window (#55). `None` when closed; closed on launch.
    pub(crate) explorer: Option<explorer::ExplorerPane>,
    /// Pending explorer git-discard confirmation. `None` when not confirming.
    /// Carries the absolute path of the node whose worktree changes will be
    /// discarded when the user presses `y`.
    pub(crate) explorer_git_discard_confirm: Option<std::path::PathBuf>,
    /// Resolved icon set for the explorer (Nerd / Unicode / Ascii), from the
    /// `icons` config setting.
    pub(crate) icon_mode: hjkl_icons::IconMode,
    /// Buffered digit-prefix count for an app-level count prefix (e.g. `5` in
    /// `5gt`). Accumulated in Normal mode when no chord prefix is active.
    /// Digits are replayed to the engine when the non-digit key is
    /// engine-handled, or consumed when the key is app-handled.
    pub pending_count: hjkl_vim::CountAccumulator,
    /// True iff the in-flight `g<x>` chord was preceded by an explicit
    /// digit-prefix count (e.g. the `2` in `2gt`), as opposed to no count at
    /// all (bare `gt`). `{count}gt` in vim is an *absolute* tab-page jump
    /// while bare `gt` is *relative* (next, with wrap) — by the time the
    /// second chord key resolves, `pending_count` has already been consumed
    /// into `PendingState::AfterG`'s count field (which defaults explicit-1
    /// and implicit-none to the same `1`), so this flag is captured earlier,
    /// at `BeginPendingAfterG` time, to preserve the distinction (#audit-r2).
    pub(crate) g_chord_explicit_count: bool,
    /// Direction of the active `search_field`.
    pub search_dir: SearchDir,
    /// Last cursor shape we emitted to the terminal.
    last_cursor_shape: CursorShape,
    /// Tree-sitter syntax highlighting layer. Owns the worker thread + the
    /// active theme. Multiplexed by BufferId.
    syntax: SyntaxLayer,
    /// Background worker for git diff-sign computation.
    git_worker: GitSignsWorker,
    /// Background worker for git blame computation.
    pub(crate) blame_worker: BlameWorker,
    /// Background worker for external formatter invocations (`=` / `==`).
    /// Moves blocking subprocess calls off the UI thread (#118).
    pub(crate) format_worker: hjkl_mangler::FormatWorker,
    /// View ids for which a format job is currently in-flight.
    /// Used to show a "formatting…" status indicator and to skip redundant
    /// submits (the worker's per-buffer dedup is the hard guarantee; this
    /// set is advisory UI state).
    pub(crate) format_pending: HashSet<BufferId>,
    /// Shared grammar resolver. `Arc` so the syntax layer and every picker
    /// source point at the same in-memory `Grammar` cache (one dlopen +
    /// query parse per language, app-wide).
    pub directory: std::sync::Arc<hjkl_lang::LanguageDirectory>,
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
    /// Toggled by `:syntax on|off`. When false, the bonsai syntax pipeline
    /// is bypassed: spans stay empty, no render_viewport fires, and
    /// `recompute_and_install` returns immediately. Re-enabling re-attaches
    /// the language for every slot's path and triggers a fresh recompute.
    /// Default `true` — vim parity.
    pub syntax_enabled: bool,
    /// Cache for `render::search_count` — keyed by buffer id, dirty_gen,
    /// cursor, and pattern text so the same result is returned on every
    /// frame between input/edits. Without this the status line scans the
    /// whole document on every render — 50 %+ of CPU on big files with an
    /// active `/` pattern (per samply).
    pub(crate) search_count_cache: std::cell::RefCell<Option<SearchCountCache>>,
    /// Set when an event handler decided a `recompute_and_install` is
    /// needed but deferred it to coalesce. The main event loop runs the
    /// recompute once after the event-drain loop ends, so a burst of
    /// mouse-scroll events fires one sync query instead of N.
    pub(crate) pending_recompute: bool,
    pub last_signature_us: u128,
    /// `(buffer_id, viewport top_row, viewport height, content dirty_gen)` at
    /// the last syntax recompute driven by `sync_after_engine_mutation`. Lets
    /// that hot path skip the tree-sitter viewport query on a pure cursor move
    /// (e.g. a mouse selection drag), which changes none of these — re-querying
    /// dominated the profile during mouse drags. `None` forces a recompute.
    pub(crate) last_synced_syntax_view: Option<(hjkl_lsp::BufferId, usize, u16, u64)>,
    /// User config (bundled defaults + optional XDG overrides). Tests
    /// receive `Config::default()` (the bundled values); main wires the
    /// XDG-merged value via [`Self::with_config`] before entering the
    /// event loop.
    pub config: hjkl_app::config::Config,
    /// Animated start screen shown when no file argument was given.
    /// Cleared (set to `None`) on the first keypress.
    pub start_screen: Option<crate::start_screen::StartScreen>,
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
    /// First time each pending request id was observed by the timeout sweep.
    /// Lets `drain_lsp_events` drop requests whose server exited / never
    /// answered, so the "LSP:…" spinner can't hang forever.
    pub lsp_pending_seen_at: HashMap<i64, std::time::Instant>,
    /// Global yank/delete registers (#151). One shared bank: every `Editor`
    /// (slots + window views) points at this same `Arc<Mutex<_>>`, so `yy` in
    /// one buffer/window pastes with `p` in any other — no copy-on-switch.
    pub registers: std::sync::Arc<std::sync::Mutex<hjkl_engine::Registers>>,
    /// Global (uppercase) vim marks — `mA`..`mZ` / `'A`..`'Z` (#279 slice 1).
    /// One shared bank, wired into every `Editor` exactly like [`App::registers`]:
    /// vim's uppercase marks are session-global, so setting `mA` in one split and
    /// jumping `'A` from another must see the same map.
    pub global_marks: std::sync::Arc<std::sync::Mutex<hjkl_engine::GlobalMarks>>,
    /// Last `:s` command, for `:&` / `:&&` (#279 slice 2). One shared bank,
    /// wired into every `Editor` exactly like [`App::global_marks`]: vim's
    /// last substitute is session-global, so `:s` in one split and `:&` in
    /// another must see the same command.
    pub last_substitute: std::sync::Arc<std::sync::Mutex<Option<hjkl_engine::SubstituteCmd>>>,
    /// Vim abbreviations (`:iabbrev` / `:abbreviate`) (#279 slice 3). One
    /// shared bank, wired into every `Editor` exactly like
    /// [`App::last_substitute`]: vim's abbreviations are session-global, so
    /// `:iabbrev foo bar` defined in one split must expand in every other
    /// split.
    pub abbrevs: std::sync::Arc<std::sync::Mutex<Vec<hjkl_engine::Abbrev>>>,
    /// Last committed search pattern + direction + history — the `"/`
    /// register (audit B2). One shared bank, wired into every `Editor`
    /// exactly like [`App::abbrevs`]: vim's last search is session-global,
    /// so `/foo<Enter>` in one split and `n` in another must see the same
    /// pattern.
    pub search: std::sync::Arc<std::sync::Mutex<hjkl_engine::SearchBank>>,
    /// Per-buffer changelist banks — `g;`/`g,` history and the `'.`/`` `. ``
    /// last-change mark (audit B3), keyed by `buffer_id`. UNLIKE the five
    /// banks above (one Arc shared by every editor in the app session),
    /// vim's changelist is per-BUFFER: two windows/splits on the SAME
    /// buffer must share one changelist, but windows on DIFFERENT buffers
    /// must stay isolated. Fetch-or-create via [`App::change_bank_for`];
    /// wired into an editor via `Editor::set_change_bank_arc` at every point
    /// that also sets `Editor::set_current_buffer_id` (mirrors the other
    /// banks' wiring at slot/window-editor creation and
    /// `reconcile_window_editors`). Pruned on buffer close (`:bd`/`:bw`) so
    /// this map can't grow without bound across a long session.
    pub(crate) change_banks:
        std::collections::HashMap<u64, std::sync::Arc<std::sync::Mutex<hjkl_engine::ChangeBank>>>,
    /// Active completion popup, if any.
    pub completion: Option<Completion>,
    /// Code actions from the most recent `textDocument/codeAction` response.
    /// The picker uses `ApplyCodeAction(i)` to index into this list.
    pub pending_code_actions: Vec<lsp_types::CodeActionOrCommand>,
    /// The `positionEncoding` negotiated with the server that produced
    /// `pending_code_actions`, snapshotted alongside it — by the time the
    /// user picks an entry from the code-action picker, the response's
    /// positions have long since been received, so `apply_code_action_or_command`
    /// can't re-derive "which server answered this" from current state
    /// (audit R2, UTF-16 fix).
    pub pending_code_actions_encoding: hjkl_lsp::PositionEncoding,
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
    /// Raw keys of the chord currently being built in Normal mode, accumulated
    /// while any chord is pending (app keymap trie, app `pending_state`, or the
    /// engine's own pending). Cleared when the chord commits/cancels. Used by
    /// Backspace to pop one level: cancel all pending, then replay all-but-last.
    pub(crate) chord_history: Vec<crossterm::event::KeyEvent>,
    /// Suppresses the `"file" NL` open notice for the next `do_edit` call. Set
    /// by the explorer when opening a file under the cursor — the buffer visibly
    /// changes, so the toast is just noise.
    pub(crate) suppress_open_notice: bool,
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
    /// Active `@{reg}` macro-replay work queue. `Some` only while the
    /// top-level `PlayMacro` commit arm is draining a replay; nested
    /// `@{reg}` inputs splice into this queue instead of recursing
    /// (audit R2 — see `chord_routing::MacroReplayState`).
    pub(crate) macro_replay: Option<chord_routing::MacroReplayState>,
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
    /// Explorer-context chord dispatch. Holds Normal-mode bindings that are
    /// active only when the file-explorer sidebar window is focused. Consulted
    /// by `route_chord_key_inner` before `app_keymap` when `explorer_buf_focused()`.
    pub(crate) explorer_keymap: Keymap<AppAction, keymap::HjklMode>,
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
    /// Ex command history ring (Phase 1, issue #37). Capped at 100 entries
    /// (oldest dropped). Consecutive duplicates are collapsed (vim semantics).
    pub ex_history: Vec<String>,
    /// Forward-search (`/`) history ring. Capped at 100, deduplicated.
    pub search_history_forward: Vec<String>,
    /// Backward-search (`?`) history ring. Capped at 100, deduplicated.
    pub search_history_backward: Vec<String>,
    /// Index into the active prompt's history ring while `<C-p>`/`<C-n>`
    /// history recall is active. `None` = not scrolling history.
    pub(crate) prompt_history_index: Option<usize>,
    /// The text the user had typed before the first `<C-p>` press — restored
    /// on `<C-n>` past the most-recent entry.
    pub(crate) prompt_user_input: Option<String>,
    /// Cmdline-window state: when `Some`, the focused window is a `q:`/`q/`/`q?`
    /// transient buffer. Carries the kind so `<CR>` knows how to re-dispatch.
    pub(crate) cmdline_win: Option<CmdLineWindow>,
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
    /// Set by `:redraw!` to force `terminal.clear()` before the next draw.
    /// Cleared immediately after the clear is issued so subsequent frames
    /// draw normally. `:redraw` (no `!`) leaves this `false` — ratatui's
    /// diff-based renderer already issues a repaint on the next tick.
    pub(crate) force_clear_screen: bool,
    /// Global debug mode, toggled by the `:debug` ex command. A catch-all flag
    /// for diagnostic rendering across features. Currently: when on, the
    /// explorer renders its raw buffer text with NO glyph/color overlay so the
    /// actual on-disk buffer contents are visible. Off by default.
    pub(crate) debug_mode: bool,
    /// Active interactive substitute confirm session (`:s/pat/rep/c`).
    /// While `Some`, keypresses are routed to the confirm-substitute handler
    /// rather than the editor engine. Cleared when the session finishes
    /// (all matches processed, or the user pressed `q`/`Esc`).
    pub(crate) confirming_substitute: Option<ConfirmingSubstitute>,
    /// Pending crash-recovery prompt (issue #185).
    /// Set when a file open finds a swap file newer than the on-disk content.
    /// While `Some`, keypresses route to [`App::handle_recovery_key`] rather
    /// than the engine.
    pub(crate) pending_recovery: Option<PendingRecovery>,
    /// Pending dirty-buffer disk-change prompt (issue #241). Set when the
    /// focused, modified buffer's file changes on disk. While `Some`, keypresses
    /// route to [`App::handle_disk_change_key`] (keep / reload / diff) rather
    /// than the engine.
    pub(crate) pending_disk_change: Option<PendingDiskChange>,
    /// Window ids currently in diff mode (issue #208 Phase 2). Diff
    /// highlighting engages when at least two distinct-buffer windows are
    /// present. Managed by `:diffthis` / `:diffsplit` / `:diffoff`.
    pub(crate) diff_windows: Vec<window::WindowId>,
    /// Cached line alignment for the active diff pair, invalidated when either
    /// buffer's `dirty_gen` or the participating windows change. Recomputed by
    /// [`App::refresh_diff_alignment`].
    pub(crate) diff_cache: Option<DiffCacheEntry>,
    /// Instant of the last keystroke / input event.  Used together with the
    /// active slot's `dirty_gen` to decide when the `updatetime` idle deadline
    /// has elapsed for swap-file writes.
    pub(crate) last_input_at: std::time::Instant,
    /// `(focused slot, cursor)` seen at the previous draw, used to detect
    /// cursor movement from any source. `None` before the first draw.
    pub(crate) blame_prev_cursor: Option<(usize, (usize, usize))>,
    /// Instant the focused cursor last moved (any source: keyboard, mouse,
    /// macro, LSP jump). Drives the inline git-blame idle debounce so the
    /// blame ghost only engages once the cursor has been still for
    /// `BLAME_IDLE_DELAY` — independent of what moved it or the editor mode.
    pub(crate) blame_cursor_moved_at: std::time::Instant,
    /// Name of the active colorscheme (`"dark"` / `"light"`). Set by
    /// `:colorscheme {name}` and `:set background=`; reported by bare
    /// `:colorscheme` / `:colorscheme?`. Default `"dark"`.
    pub(crate) colorscheme: String,
    /// Event-driven autoreload watcher (#242). `None` until `enable_fs_watch`
    /// is called (the real binary wires it after config; tests drive
    /// `apply_fs_events` directly). When `None`, the poll path still autoreloads
    /// on focus-regain / `:checktime`.
    fs_watch: Option<fs_watch::FsWatch>,
    /// Active smooth-scroll animation (#195). `None` = instant (default).
    pub(crate) scroll_anim: Option<ScrollAnim>,
    /// Active hop/easymotion label-jump overlay (#197). `None` when not active.
    /// While `Some`, all keypresses are routed to the hop handler instead of
    /// the editor engine.
    pub(crate) hop: Option<hop::HopState>,
    /// Quickfix list (#184): file+line locations from `:grep` (and later `:make`
    /// / LSP). Navigated via `:cnext`/`:cprev`/`]q`/`[q`; shown by `:copen`.
    pub(crate) quickfix: hjkl_quickfix::QfList,
    /// `:copen` popup visibility.
    pub(crate) quickfix_open: bool,
    /// Location list (#184 phase 3): the `:l*` analogue of the quickfix list.
    /// Populated by `:lgrep` / `:lmake` and LSP references; navigated via
    /// `:lnext`/`:lprev`/`]l`/`[l`; shown by `:lopen`.
    pub(crate) loclist: hjkl_quickfix::QfList,
    /// `:lopen` popup visibility.
    pub(crate) loclist_open: bool,
    /// Older quickfix lists for `:colder` (#261 Phase 5b). Index 0 is the oldest
    /// kept, last element is the most-recently-pushed (popped first by `:colder`).
    /// Capped at 9 entries (together with the current list → vim's 10-list max).
    pub(crate) quickfix_older: Vec<hjkl_quickfix::QfList>,
    /// Newer quickfix lists for `:cnewer`. Populated when `:colder` moves the
    /// current list back; cleared whenever a fresh population replaces the list.
    pub(crate) quickfix_newer: Vec<hjkl_quickfix::QfList>,
    /// Older location lists for `:lolder` (#261 Phase 5b).
    pub(crate) loclist_older: Vec<hjkl_quickfix::QfList>,
    /// Newer location lists for `:lnewer`.
    pub(crate) loclist_newer: Vec<hjkl_quickfix::QfList>,
    /// Global variable store (`g:` namespace). Keyed by variable name.
    /// Populated by `nvim_set_var` / `nvim_get_var` / `nvim_del_var`.
    pub(crate) nvim_gvars: std::collections::HashMap<String, rmpv::Value>,
    /// View-local variable store (`b:` namespace). Keyed by `(buffer_id, name)`.
    /// Populated by `nvim_buf_set_var` / `nvim_buf_get_var` / `nvim_buf_del_var`.
    pub(crate) nvim_bvars: std::collections::HashMap<(u64, String), rmpv::Value>,
    /// Window-local variable store (`w:` namespace). Keyed by `(window_id, name)`.
    /// Populated by `nvim_win_set_var` / `nvim_win_get_var` / `nvim_win_del_var`.
    pub(crate) nvim_wvars: std::collections::HashMap<(u64, String), rmpv::Value>,
    /// Full terminal `Rect` from the most recent `render::frame` call
    /// (`frame.area()`). `None` before the first frame is drawn.
    ///
    /// `screen_rect()` used to derive this from the FOCUSED window's
    /// viewport, but the renderer sets each window's viewport to its PANE
    /// dims (not the terminal) — so with any split open, that derivation
    /// returned the focused pane's size instead of the real screen. This
    /// is the actual terminal geometry, recorded where it's known for free.
    pub(crate) last_frame_rect: Option<ratatui::layout::Rect>,
}

/// Pending crash-recovery prompt state (issue #185).
///
/// Set when a file is opened that has a swap file newer than the on-disk
/// content.  Key presses route to [`App::handle_recovery_key`] while this
/// is `Some`.
pub(crate) struct PendingRecovery {
    /// The loaded swap header.
    pub header: hjkl_app::swap::SwapHeader,
    /// The swap body text.
    pub body: String,
    /// Index of the slot whose content should be replaced on `y`.
    pub slot_idx: usize,
    /// Human-readable relative time string for the prompt ("42s ago", "3m ago", …).
    pub written_ago: String,
}

/// Pending dirty-buffer disk-change prompt state (issue #241).
///
/// Set when the focused, modified buffer's on-disk file changes underneath it.
/// Key presses route to [`App::handle_disk_change_key`] while this is `Some`:
/// `k` keeps the buffer, `r` reloads from disk (discarding edits), `d` opens a
/// `:DiffOrig` split of buffer vs disk.
pub(crate) struct PendingDiskChange {
    /// Index of the slot whose file changed (always the focused slot).
    pub slot_idx: usize,
    /// Path of the file that changed on disk.
    pub path: std::path::PathBuf,
}

/// Cached side-by-side alignment for the active diff pair (issue #208 Phase 2).
///
/// `a_win` / `b_win` are the two participating windows (insertion order); the
/// alignment's `a` columns map to `a_win`'s buffer, `b` columns to `b_win`'s.
/// Invalidated when either window changes or either buffer's `dirty_gen` moves.
pub(crate) struct DiffCacheEntry {
    pub a_win: window::WindowId,
    pub b_win: window::WindowId,
    pub a_gen: u64,
    pub b_gen: u64,
    pub diff: hjkl_app::diff::LineDiff,
}

/// State for an interactive `:s/pat/rep/c` confirm session.
///
/// The event loop routes `y`/`n`/`a`/`q`/`l`/`Esc` to
/// [`App::handle_confirm_substitute_key`] while this is `Some`.
pub(crate) struct ConfirmingSubstitute {
    /// All candidate matches in document order.
    pub matches: Vec<hjkl_engine::SubstituteMatch>,
    /// Which matches the user has accepted so far. Parallel to `matches`.
    pub accepted: Vec<bool>,
    /// Index of the match currently being prompted.
    pub idx: usize,
}

/// Memoised result of [`crate::render::search_count`]. Stored in a
/// `RefCell` on `App` so the render path (taking `&App`) can refresh
/// it without restructuring callers.
#[derive(Debug, Clone)]
pub(crate) struct SearchCountCache {
    pub buffer_id: crate::syntax::BufferId,
    pub dirty_gen: u64,
    pub cursor: (usize, usize),
    pub pattern: String,
    pub result: Option<(usize, usize)>,
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

pub(crate) use prompt::prompt_cursor_shape;

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
/// Probe whether `path` is writable by the current process without modifying it.
///
/// Uses `OpenOptions::append` which does not truncate. Returns `true` when the
/// file can be opened for appending (writable), `false` only on
/// `PermissionDenied`, and `true` for all other errors (don't over-block on
/// non-permission failures). Returns `true` for paths that don't exist yet.
fn is_path_writable(path: &std::path::Path) -> bool {
    match std::fs::OpenOptions::new().append(true).open(path) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => false,
        Err(_) => true,
    }
}

pub(super) fn build_slot(
    syntax: &mut SyntaxLayer,
    buffer_id: BufferId,
    path: Option<PathBuf>,
    config: &hjkl_app::config::Config,
) -> Result<BufferSlot, String> {
    let mut buffer = View::new();
    let mut is_new_file = false;
    let mut disk_mtime: Option<SystemTime> = None;
    let mut disk_len: Option<u64> = None;
    // Retained for modeline scanning after the buffer is seeded.
    let mut file_content: Option<String> = None;
    if let Some(ref p) = path {
        match std::fs::read_to_string(p) {
            Ok(content) => {
                // Snapshot disk metadata right after a successful read.
                if let Ok(meta) = std::fs::metadata(p) {
                    disk_mtime = meta.modified().ok();
                    disk_len = Some(meta.len());
                }
                let stripped = content.strip_suffix('\n').unwrap_or(&content);
                BufferEdit::replace_all(&mut buffer, stripped);
                file_content = Some(content);
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
        hjkl_app::editorconfig::overlay_for_path(&mut ec_opts, p);
    }
    // Modeline overlay — applied after editorconfig so per-file modelines win.
    // Only runs when `modeline` is enabled (default true).
    if ec_opts.modeline
        && let Some(ref content) = file_content
    {
        let scan_depth = ec_opts.modelines as usize;
        hjkl_app::modeline::overlay_modeline_for_content(&mut ec_opts, content, scan_depth);
    }
    let mut editor = hjkl_vim::vim_editor(buffer, host, ec_opts);
    // Tag the editor with its stable buffer_id so `mA`–`mZ` global marks
    // record the correct id from the first keystroke.
    editor.set_current_buffer_id(buffer_id);
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
        // Mirror the language detection onto the engine's filetype setting
        // so filetype-aware features (comment-continuation, `gcc` toggle,
        // `:set commentstring` defaults, future modeline knobs) light up
        // automatically on file open. Cheap synchronous extension lookup;
        // no grammar load.
        if let Some(lang) = syntax.language_name_for_path(p) {
            editor.set_filetype(&lang);
        }
    }

    let (vp_top, vp_height) = {
        let vp = editor.host().viewport();
        (vp.top_row, vp.height as usize)
    };
    // Sync render for immediate paint on open. recompute_and_install can't
    // be called here (slot isn't wired into App.slots yet), so go through
    // the layer directly.
    if let Some(out) = syntax.render_viewport(buffer_id, editor.buffer(), vp_top, vp_height) {
        editor.install_ratatui_syntax_spans(out.spans);
    }
    let _ = editor.take_content_edits();
    let _ = editor.take_content_reset();

    // Compute swap path for named files (best-effort; ignore errors here —
    // the write path handles errors per-write).
    let swap_path = if let Some(ref p) = path {
        let canonical = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
        hjkl_app::swap::swap_path_for(&canonical).ok()
    } else {
        None
    };

    let mut slot = BufferSlot {
        buffer_id,
        is_explorer: false,
        features: BufferFeatures::default(),
        editor,
        filename: path,
        dirty: false,
        is_new_file,
        is_untracked: false,
        diag_signs: Vec::new(),
        diag_signs_lsp: Vec::new(),
        lsp_diags: Vec::new(),
        last_lsp_dirty_gen: None,
        git_signs: Vec::new(),
        last_git_dirty_gen: None,
        last_git_refresh_at: Instant::now(),
        blame: Vec::new(),
        last_blame_dirty_gen: None,
        last_blame_refresh_at: Instant::now(),
        saved_hash: 0,
        saved_len: 0,
        signature_cache: None,
        disk_mtime,
        disk_len,
        disk_state: DiskState::Synced,
        swap_path,
        last_swap_dirty_gen: None,
        last_fold_dirty_gen: None,
        git_repo_present: None,
        commit_ctx: None,
    };
    slot.snapshot_saved();
    // Auto-readonly for files that exist but aren't writable by the current user.
    if let Some(ref p) = slot.filename
        && !slot.is_new_file
        && !is_path_writable(p)
    {
        slot.editor.settings_mut().readonly = true;
    }
    Ok(slot)
}

// build_app_keymap and engine_input_to_key_event moved to keymap_build.rs.

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
            mouse::Zone::TabBar { tab_idx } | mouse::Zone::TabBarClose { tab_idx } => {
                // Switch to the clicked tab so do_tabclose targets it,
                // then close.
                if tab_idx != self.active_tab {
                    self.switch_tab(tab_idx);
                }
                self.do_tabclose();
            }
            mouse::Zone::BufferLine { slot_idx } | mouse::Zone::BufferLineClose { slot_idx } => {
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
            self.active_editor().vim_mode(),
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
            self.switch_focus(win_id);
        }
        let target = match zone {
            mouse::Zone::Code {
                doc_row, doc_col, ..
            } => Some((doc_row, doc_col)),
            mouse::Zone::Gutter { doc_row, .. } => Some((doc_row, 0)),
            _ => None,
        };
        if let Some((doc_row, doc_col)) = target {
            self.active_editor_mut().mouse_click_doc(doc_row, doc_col);
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
            || self.filter_field.is_some()
            || self.search_field.is_some()
            || self.info_popup.is_some()
            || self.hop.is_some()
            || self.quickfix_open
            || self.loclist_open
    }

    /// Full-screen rect for clamping popups / context menus to the
    /// terminal area.
    ///
    /// Returns `last_frame_rect` — the real terminal `Rect` recorded by
    /// `render::frame` on the most recent draw — when available. Before the
    /// first frame (no draw has happened yet, e.g. in some unit tests) falls
    /// back to the old derivation from the focused window's viewport,
    /// matching the layout `render::frame` computes: optional top bar
    /// (tabs and buffer line, when multiple slots OR tabs are open) plus
    /// the editor viewport plus the bottom status line.
    ///
    /// That fallback derivation is WRONG once any split exists: the
    /// renderer sets each window's viewport to its PANE dims, not the
    /// terminal (render.rs `render_window`), so it silently returns the
    /// focused pane's size instead of the screen's. `last_frame_rect` is
    /// always correct because it's the actual `frame.area()` the renderer
    /// drew into, regardless of how many splits are open.
    ///
    /// MUST include the top bar when it's visible — otherwise this
    /// underestimates total height by 1 row and a popup anchored near
    /// the bottom flips one row too soon, putting the
    /// `Moved`-handler's row→item math out of sync with what
    /// `bounding_rect` produces at render time.
    pub(crate) fn screen_rect(&self) -> ratatui::layout::Rect {
        if let Some(rect) = self.last_frame_rect {
            return rect;
        }
        let vp = self.active_editor().host().viewport();
        let real_slots = self.slots.iter().filter(|s| !s.is_explorer).count();
        let show_top_bar = self.tabs.len() > 1 || real_slots > 1;
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
    pub fn focused_slot_idx(&self) -> usize {
        self.windows[self.focused_window()]
            .as_ref()
            .expect("focused_window must point to an open window")
            .slot
    }

    /// Return a shared reference to the active buffer slot.
    pub fn active(&self) -> &BufferSlot {
        &self.slots[self.focused_slot_idx()]
    }

    /// `true` when the active slot has buffer changes not yet written to its
    /// swap file (`dirty_gen` advanced past the last-swapped gen) AND has a
    /// swap path. Drives the idle swap-write timer; gating on this rather than
    /// bare `dirty` prevents a busy-loop: once the swap is current, the
    /// `updatetime` deadline stops shortening the poll timeout.
    pub(crate) fn active_swap_pending(&self) -> bool {
        let s = self.active();
        // A swap write is due when the buffer changed since the last swap AND
        // there is something to protect: either a named buffer with a swap path
        // already assigned, OR an unnamed (scratch) buffer that now holds
        // content. Scratch slots start with `swap_path = None` and have it
        // assigned lazily by `write_swap_for_slot` on the first non-empty
        // write — so the scratch arm must NOT require `swap_path.is_some()`,
        // else the idle writer would never fire for a scratch buffer.
        let has_target =
            s.swap_path.is_some() || (s.filename.is_none() && s.editor.buffer().byte_len() > 0);
        has_target && s.last_swap_dirty_gen != Some(s.editor.buffer().dirty_gen())
    }

    /// Cursor `(row, col)` of window `win_id`, read from its own editor (#151
    /// Phase D — the single source of truth). Falls back to the legacy
    /// `layout::Window` mirror if the window editor is somehow absent, then
    /// `(0, 0)`.
    /// The editor for window `win_id` (the View — single source of per-window
    /// cursor/viewport/is_blame, #151). Falls back to the slot bridge editor
    /// when no window editor exists yet (pre-reconcile / headless paths).
    pub(crate) fn window_editor(&self, win_id: window::WindowId) -> &Editor<View, TuiHost> {
        self.window_editors.get(&win_id).unwrap_or_else(|| {
            let slot = self
                .windows
                .get(win_id)
                .and_then(|w| w.as_ref())
                .map(|w| w.slot)
                .unwrap_or_else(|| self.focused_slot_idx());
            &self.slots[slot].editor
        })
    }

    /// Cursor `(row, col)` for slot `idx`, read from a window editor showing it
    /// (#151 single source of truth) — the focused window if it shows the slot,
    /// else any window on the slot, else the slot bridge editor. For per-slot
    /// machinery (swap metadata, autoreload) that must pick one view's cursor.
    pub(crate) fn slot_cursor(&self, idx: usize) -> (usize, usize) {
        let win_id = if self
            .windows
            .get(self.focused_window())
            .and_then(|w| w.as_ref())
            .map(|w| w.slot)
            == Some(idx)
        {
            Some(self.focused_window())
        } else {
            self.windows
                .iter()
                .enumerate()
                .find_map(|(id, w)| w.as_ref().filter(|w| w.slot == idx).map(|_| id))
        };
        match win_id.and_then(|id| self.window_editors.get(&id)) {
            Some(e) => {
                let c = e.buffer().cursor();
                (c.row, c.col)
            }
            None => self.slots[idx].editor.cursor(),
        }
    }

    pub(crate) fn window_cursor(&self, win_id: window::WindowId) -> (usize, usize) {
        self.window_editors
            .get(&win_id)
            .map(|e| {
                let c = e.buffer().cursor();
                (c.row, c.col)
            })
            .unwrap_or((0, 0))
    }

    /// Interpolated RENDER top for `win_id` if a scroll animation is mid-flight,
    /// else None (render uses the real viewport top). Ease-out cubic.
    pub(crate) fn scroll_anim_render_top(&self, win_id: window::WindowId) -> Option<usize> {
        let a = self.scroll_anim.as_ref()?;
        if a.win_id != win_id {
            return None;
        }
        let elapsed = a.started_at.elapsed();
        if elapsed >= a.duration {
            return None;
        }
        let p = (elapsed.as_secs_f32() / a.duration.as_secs_f32()).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - p).powi(3); // ease-out cubic
        let start = a.start_top as f32;
        let target = a.target_top as f32;
        Some((start + (target - start) * eased).round().max(0.0) as usize)
    }

    /// True once the active animation has run its full duration.
    pub(crate) fn scroll_anim_expired(&self) -> bool {
        self.scroll_anim
            .as_ref()
            .is_some_and(|a| a.started_at.elapsed() >= a.duration)
    }

    /// Scroll origin `(top_row, top_col)` of window `win_id`, read from its own
    /// editor's viewport (#151 Phase D — the single source of truth).
    pub(crate) fn window_scroll(&self, win_id: window::WindowId) -> (usize, usize) {
        self.window_editors
            .get(&win_id)
            .map(|e| {
                let vp = e.host().viewport();
                (vp.top_row, vp.top_col)
            })
            .unwrap_or((0, 0))
    }

    /// Seed a freshly-created window's editor with an inherited cursor + scroll
    /// (#151 Phase D) — used by splits so the new window opens at the source
    /// window's position. The window editor must already exist (call
    /// `reconcile_window_editors` first). No-op if absent.
    pub(crate) fn seed_window_editor(
        &mut self,
        win_id: window::WindowId,
        cursor_row: usize,
        cursor_col: usize,
        top_row: usize,
        top_col: usize,
    ) {
        if let Some(e) = self.window_editors.get_mut(&win_id) {
            e.jump_cursor(cursor_row, cursor_col);
            let vp = e.host_mut().viewport_mut();
            vp.top_row = top_row;
            vp.top_col = top_col;
        }
    }

    /// Rebase the cursor + scroll of every *sibling* window (one showing the
    /// same buffer as the focused window) against `edits` produced by the
    /// focused window's editor (#151 Phase E — multi-window same-buffer cursor
    /// rebase). Each `ContentEdit` replaces document rows `[start, old_end]`
    /// with `[start, new_end]`: a sibling position below the change shifts by the
    /// net row delta, one inside the change clamps to the new end, one above is
    /// untouched. Row-level only (the impactful case); the focused window's own
    /// cursor is owned by the engine and not touched here.
    pub(crate) fn rebase_sibling_cursors(&mut self, edits: &[hjkl_engine::types::ContentEdit]) {
        if edits.is_empty() {
            return;
        }
        let fw = self.focused_window();
        let Some(slot) = self
            .windows
            .get(fw)
            .and_then(|w| w.as_ref())
            .map(|w| w.slot)
        else {
            return;
        };
        let siblings: Vec<window::WindowId> = self
            .windows
            .iter()
            .enumerate()
            .filter_map(|(id, w)| w.as_ref().map(|w| (id, w.slot)))
            .filter(|&(id, s)| s == slot && id != fw)
            .map(|(id, _)| id)
            .collect();
        for sid in siblings {
            let Some(e) = self.window_editors.get_mut(&sid) else {
                continue;
            };
            let cur = e.buffer().cursor();
            let mut row = cur.row;
            let mut top = e.host().viewport().top_row;
            for ed in edits {
                let start = ed.start_position.0 as usize;
                let old_end = ed.old_end_position.0 as usize;
                let new_end = ed.new_end_position.0 as usize;
                row = rebase_row_for_edit(row, start, old_end, new_end);
                top = rebase_row_for_edit(top, start, old_end, new_end);
            }
            if row != cur.row {
                e.set_cursor_quiet(row, cur.col);
            }
            e.host_mut().viewport_mut().top_row = top;
        }
    }

    /// Return a mutable reference to the active buffer slot.
    pub fn active_mut(&mut self) -> &mut BufferSlot {
        let slot_idx = self.focused_slot_idx();
        &mut self.slots[slot_idx]
    }

    /// Shared reference to the focused window's editor (#151 Phase D). Each
    /// window owns its editor in [`window_editors`]; this resolves the focused
    /// one. Falls back to the focused slot's bridge editor only if the window
    /// editor is somehow absent (should not happen — the invariant keeps them
    /// in lockstep).
    pub fn active_editor(&self) -> &Editor<View, TuiHost> {
        let fw = self.focused_window();
        self.window_editors
            .get(&fw)
            .unwrap_or_else(|| &self.slots[self.focused_slot_idx()].editor)
    }

    /// Mutable reference to the focused window's editor. See [`active_editor`].
    pub fn active_editor_mut(&mut self) -> &mut Editor<View, TuiHost> {
        let fw = self.focused_window();
        if self.window_editors.contains_key(&fw) {
            self.window_editors.get_mut(&fw).unwrap()
        } else {
            let slot_idx = self.focused_slot_idx();
            &mut self.slots[slot_idx].editor
        }
    }

    /// Fetch (or create) the shared changelist bank for `buffer_id` (audit
    /// B3). All editors currently attached to the same `buffer_id` — the
    /// slot's own bridge editor plus every window's view editor onto it —
    /// must be wired to the SAME `Arc` so `g;`/`` `. `` in one split see
    /// edits made from another split on that buffer. Callers pair this with
    /// `Editor::set_current_buffer_id(buffer_id)` /
    /// `Editor::set_change_bank_arc(..)` at every site that retargets an
    /// editor's buffer (mirrors how the other shared banks are wired
    /// alongside `set_current_buffer_id`).
    pub(crate) fn change_bank_for(
        &mut self,
        buffer_id: u64,
    ) -> std::sync::Arc<std::sync::Mutex<hjkl_engine::ChangeBank>> {
        self.change_banks
            .entry(buffer_id)
            .or_insert_with(|| {
                std::sync::Arc::new(std::sync::Mutex::new(hjkl_engine::ChangeBank::default()))
            })
            .clone()
    }

    /// Build a fresh per-window view editor onto `slot_idx`'s shared `Buffer`.
    /// Copies the slot editor's settings + viewport dims so the new view
    /// renders identically; the cursor starts at the slot editor's cursor.
    pub(crate) fn make_view_editor(&self, slot_idx: usize) -> Editor<View, TuiHost> {
        let src = &self.slots[slot_idx].editor;
        let view = View::new_view(src.buffer().content_arc());
        let mut ed = hjkl_vim::vim_editor(view, TuiHost::new(), Options::default());
        *ed.settings_mut() = src.settings().clone();
        ed.set_current_buffer_id(self.slots[slot_idx].buffer_id);
        // Inherit the slot editor's cursor so the first view onto a buffer keeps
        // any pre-window positioning (startup `+/pat` search, `+N`, a split
        // inheriting the source cursor).
        let src_cursor = src.buffer().cursor();
        ed.set_cursor_quiet(src_cursor.row, src_cursor.col);
        // Last-search is a shared bank (audit B2) wired by the caller
        // (`reconcile_window_editors`) via `set_search_arc` right after this
        // returns — no per-window copy needed, `n`/`N` see it live.
        let (w, h, top_row, top_col) = {
            let vp = src.host().viewport();
            (vp.width, vp.height, vp.top_row, vp.top_col)
        };
        {
            let vp = ed.host_mut().viewport_mut();
            vp.width = w;
            vp.height = h;
            vp.top_row = top_row;
            vp.top_col = top_col;
        }
        ed.set_viewport_height(h);
        ed
    }

    /// Reconcile every window's editor with its current slot's `Buffer`
    /// (#151 Phase D). Rebuilds a window editor only when its content `Arc` no
    /// longer matches its slot's — so a pure slot-index reindex (e.g. after
    /// `:bd`) preserves the window's cursor, while a true buffer switch rebuilds
    /// the view. Drops editors for windows that are now `None`.
    pub(crate) fn reconcile_window_editors(&mut self) {
        let targets: Vec<(window::WindowId, usize)> = self
            .windows
            .iter()
            .enumerate()
            .filter_map(|(i, w)| w.as_ref().map(|w| (i, w.slot)))
            .collect();
        let live: std::collections::HashSet<window::WindowId> =
            targets.iter().map(|(id, _)| *id).collect();
        self.window_editors.retain(|id, _| live.contains(id));
        for (wid, slot) in targets {
            if slot >= self.slots.len() {
                continue;
            }
            let slot_content = self.slots[slot].editor.buffer().content_arc();
            let needs = match self.window_editors.get(&wid) {
                Some(e) => !std::sync::Arc::ptr_eq(&e.buffer().content_arc(), &slot_content),
                None => true,
            };
            if needs {
                let mut ed = self.make_view_editor(slot);
                ed.set_registers_arc(self.registers.clone());
                ed.set_global_marks_arc(self.global_marks.clone());
                ed.set_last_substitute_arc(self.last_substitute.clone());
                ed.set_abbrevs_arc(self.abbrevs.clone());
                ed.set_search_arc(self.search.clone());
                // Per-buffer changelist bank (audit B3): fetch-or-create the
                // bank keyed by this window's slot's buffer_id, and swap it
                // in — this is the point where a window's editor actually
                // (re)targets a buffer, so it is where the change-bank Arc
                // must be re-resolved too (unlike the five banks above,
                // which are one Arc for the whole session and never change).
                let bid = self.slots[slot].buffer_id;
                let bank = self.change_bank_for(bid);
                ed.set_change_bank_arc(bank);
                self.window_editors.insert(wid, ed);
            }
        }
    }

    /// Return a mutable reference to the active buffer slot.
    pub fn active_slot_mut(&mut self) -> &mut BufferSlot {
        let slot_idx = self.focused_slot_idx();
        &mut self.slots[slot_idx]
    }

    /// Return a shared slice of all buffer slots.
    pub fn slots(&self) -> &[BufferSlot] {
        &self.slots
    }

    /// Return a mutable slice of all buffer slots. Used by tests to set up
    /// buffer content/viewport directly. (#151 Phase D moved the renderer's
    /// per-window viewport publish onto the window editors, so this is now
    /// test-only.)
    ///
    /// The `#[allow(dead_code)]` here is NOT stale (audit D8 checked): every
    /// call site is inside a `#[cfg(test)]` module, so a plain non-test
    /// `cargo build` genuinely never calls this — unlike `active_slot_mut`,
    /// whose stale attribute audit D8 did drop.
    #[allow(dead_code)]
    pub fn slots_mut(&mut self) -> &mut [BufferSlot] {
        &mut self.slots
    }

    /// Return the slot index of the currently focused window (used by
    /// the buffer-line renderer to highlight the active buffer tab).
    pub fn active_index(&self) -> usize {
        self.focused_slot_idx()
    }

    /// When `matchparen` is on and the cursor sits on a C-style bracket with
    /// a matching partner, return `[(cursor_row, cursor_col), (match_row, match_col)]`.
    /// Otherwise returns `None`.
    ///
    /// Display-only: char-col indices (not byte offsets), suitable for
    /// direct screen column math after adding gutter width.
    /// Tag-pair matching (`<tag>…</tag>`) is handled by `matchparen_tag_cells`
    /// via char-scan (not tree-sitter) as of #243.
    ///
    /// Focused-window only: matchparen highlights the bracket under the live
    /// editor cursor, so it is not rendered for unfocused windows.
    pub fn matchparen_cells(&self) -> Option<[(usize, usize); 2]> {
        let editor = self.active_editor();
        if !editor.settings().matchparen {
            return None;
        }
        let cur = editor.buffer().cursor();
        let (row, col) = (cur.row, cur.col); // hjkl_buffer::Position fields are usize
        let match_pos = hjkl_engine::motions::matching_bracket_pos(editor.buffer(), row, col)?;
        Some([(row, col), match_pos])
    }

    /// Tag-name pair under the cursor for matchparen highlight (#243). Returns
    /// the per-cell char-col positions covering BOTH the open and close tag
    /// names, or `None` when matchparen is off or the cursor is not on a
    /// paired tag name.
    pub fn matchparen_tag_cells(&self) -> Option<Vec<(usize, usize)>> {
        let editor = self.active_editor();
        if !editor.settings().matchparen {
            return None;
        }
        let cur = editor.buffer().cursor();
        let pair = hjkl_engine::matching_tag_pair(editor.buffer(), cur.row, cur.col)?;
        let mut cells = Vec::new();
        for (row, start, end) in pair {
            for col in start..end {
                cells.push((row, col));
            }
        }
        Some(cells)
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
        self.sync_after_engine_mutation_inner(false);
    }

    /// Like [`sync_after_engine_mutation`] but defers the syntax recompute to
    /// the event loop's `pending_recompute` flush (one query per drawn frame)
    /// instead of running it synchronously. Used by the mouse handlers: a fast
    /// drag emits a burst of events drained in a single loop iteration, so a
    /// synchronous recompute per event would run the tree-sitter viewport query
    /// many times for one frame (the mouse-drag lag). Deferring collapses the
    /// whole burst into one recompute before the next draw.
    pub(crate) fn sync_after_engine_mutation_deferred(&mut self) {
        self.sync_after_engine_mutation_inner(true);
    }

    fn sync_after_engine_mutation_inner(&mut self, defer_recompute: bool) {
        // Keymap-dispatched motions go through `apply_motion_kind` which
        // calls `execute_motion` but does NOT invoke `ensure_cursor_in_scrolloff`
        // (the engine FSM `step()` path does it explicitly). Without this call
        // the engine cursor advances off-screen and the viewport top_row
        // never updates — the user sees the cursor disappear. Mirror the FSM
        // behaviour from the app side so the keymap path stays viewport-coherent.
        // Idempotent for non-motion mutations (already-in-bounds = no-op).
        self.active_editor_mut().ensure_cursor_in_scrolloff();
        // Propagate any mode change (e.g. i/I/a/A/o/O enter-insert actions
        // dispatched through the app keymap) to the host cursor-shape so the
        // render loop picks it up on the next frame. Idempotent when mode
        // did not change.
        self.active_editor_mut().emit_cursor_shape_if_changed();
        self.sync_viewport_from_editor();
        if self.active_editor_mut().take_dirty() {
            let elapsed = self.active_mut().refresh_dirty_against_saved();
            self.last_signature_us = elapsed;
            if self.active().dirty {
                self.active_mut().is_new_file = false;
            }
        }
        let buffer_id = self.active().buffer_id;
        let content_reset = self.active_editor_mut().take_content_reset();
        if content_reset {
            self.handle_active_content_reset(buffer_id);
        }
        let edits = self.active_editor_mut().take_content_edits();
        if !edits.is_empty() {
            self.syntax.apply_edits(buffer_id, &edits);
            self.active_editor_mut()
                .shift_syntax_spans_for_edits(&edits);
        }
        self.lsp_notify_change_active(&edits);
        // Sibling-window fold invalidation + diff-pair realignment. Factored
        // into a shared method (see its doc comment) so the event-loop
        // fall-through key-dispatch paths, which build their own `edits` via
        // the same `take_content_edits()` primitive but historically did NOT
        // call this function, can share the exact same logic instead of
        // drifting out of sync with it.
        self.sync_diff_and_fold_siblings(&edits);
        // Rebase sibling windows' cursor + scroll against this edit (#151 Phase
        // E) so a window showing the same buffer keeps pointing at the same
        // logical line when another window inserts/deletes lines above it.
        self.rebase_sibling_cursors(&edits);
        // Drain pending fold ops so the vec doesn't grow unboundedly.
        // `recompute_and_install` handles the visual refresh; the ops are
        // queued for host observation but this app has no other consumer.
        let had_fold_ops = !self.active_editor_mut().take_fold_ops().is_empty();

        // Only re-run the tree-sitter viewport query when something that
        // affects syntax spans actually changed: buffer content (dirty_gen
        // bumps on every edit path), a content reset, a fold toggle, or a
        // viewport scroll/resize/buffer-switch. A pure cursor move — notably a
        // mouse selection drag, which fires many events per second — leaves all
        // of these identical, so recomputing would be wasted work (it dominated
        // the mouse-drag profile at ~88% inclusive). Selection, search, and
        // matchparen highlights are render-time overlays and refresh each frame
        // regardless of this gate. Direct callers of `recompute_and_install`
        // (settings/theme/`:syntax`) are unaffected — they still recompute
        // unconditionally.
        let view_now = {
            let vp = self.active_editor().host().viewport();
            let dg = self.active_editor().buffer().dirty_gen();
            (buffer_id, vp.top_row, vp.height, dg)
        };
        if content_reset || had_fold_ops || self.last_synced_syntax_view != Some(view_now) {
            self.last_synced_syntax_view = Some(view_now);
            if defer_recompute {
                // Flushed once at the top of the event loop before drawing.
                self.pending_recompute = true;
            } else {
                self.recompute_and_install();
            }
        }
    }

    /// Drain the content-reset/edit queue produced by a direct
    /// `Editor::set_content()` mutation — used by the nvim-api
    /// `nvim_buf_set_lines` / `nvim_buf_set_text` handlers — and feed it
    /// through the same syntax + LSP + dirty-refresh pipeline the keystroke
    /// (`sync_after_engine_mutation`) and ex-command paths use.
    ///
    /// `set_content()` followed by the nvim-api `settle()` helper alone only
    /// reconciles window editors and flushes a pending recompute; neither
    /// drains `take_content_edits`/`take_content_reset`. Without this, a
    /// buffer mutated via `nvim_buf_set_lines`/`nvim_buf_set_text` got no
    /// `textDocument/didChange`, kept a stale tree-sitter tree (the parser
    /// never saw the edit), and never refreshed its dirty flag (audit R2,
    /// fix 1).
    ///
    /// `slot_idx` need not be the focused slot: `nvim_buf_set_lines` /
    /// `nvim_buf_set_text` can target any open buffer by handle, so this
    /// mirrors `apply_workspace_edit`'s per-slot drain (audit R2, fix 3)
    /// rather than assuming the active editor.
    pub(crate) fn sync_after_direct_content_mutation(&mut self, slot_idx: usize) {
        let Some(buffer_id) = self.slots.get(slot_idx).map(|s| s.buffer_id) else {
            return;
        };
        if self.slots[slot_idx].editor.take_content_reset() {
            self.syntax.reset(buffer_id);
            self.slots[slot_idx]
                .editor
                .install_ratatui_syntax_spans(Vec::new());
        }
        let edits = self.slots[slot_idx].editor.take_content_edits();
        if !edits.is_empty() {
            self.syntax.apply_edits(buffer_id, &edits);
            self.slots[slot_idx]
                .editor
                .shift_syntax_spans_for_edits(&edits);
        }
        self.lsp_notify_change_for_slot(slot_idx, &edits);
        if self.slots[slot_idx].editor.take_dirty() {
            self.slots[slot_idx].refresh_dirty_against_saved();
            if self.slots[slot_idx].dirty {
                self.slots[slot_idx].is_new_file = false;
            }
        }
        self.pending_recompute = true;
    }

    /// Invalidate sibling windows' folds across `edits`, and refresh the
    /// diff-pair alignment cache / scroll-bind if a diff pair is active.
    ///
    /// Factored out of [`sync_after_engine_mutation_inner`] so every
    /// edit-producing call site can share it. History: `event_loop.rs`'s
    /// inline post-dispatch sync blocks (the primary key-read arm, the
    /// same-tick drain-loop mirror, and the completion-popup insert-char /
    /// backspace / auto-trigger arms) each hand-duplicated pieces of
    /// [`sync_after_engine_mutation_inner`] instead of calling it, and this
    /// piece — sibling `window_folds` invalidation plus diff realignment —
    /// was dropped from every copy. With a diff pair active, an edit on any
    /// of those paths left `diff_cache` stale: the next frame's
    /// `diff_line_classes` (`diff_mode.rs`) indexes a row that no longer
    /// exists once the buffer shrank past it (e.g. `dG` near EOF), and
    /// `rope.line(row)` panics. `refresh_diff_alignment` and
    /// `sync_diff_scroll` are gen-keyed / pair-gated, so calling them
    /// unconditionally here is a cheap no-op when no diff pair is active or
    /// nothing relevant changed.
    ///
    /// [`sync_after_engine_mutation_inner`]: Self::sync_after_engine_mutation_inner
    pub(crate) fn sync_diff_and_fold_siblings(
        &mut self,
        edits: &[hjkl_engine::types::ContentEdit],
    ) {
        // Window-level folds: an edit updates any fold in OTHER windows
        // showing this same slot, mirroring what the engine now does for the
        // focused window's own (shared-buffer) folds (audit-r2 fix 1):
        // a same-row-count edit (delta == 0) still just invalidates
        // (drops) any fold it overlapped — matching `mutate_edit`'s
        // cursor-band `Invalidate` — but a row-count-changing edit shifts
        // survivors by the row delta (growing/shrinking/clipping/dropping
        // as appropriate) via `shift_folds_after_edit`, the exact rule
        // `Editor::shift_marks_after_edit` -> `rebase_folds` applies to the
        // focused window. Without this, a sibling split's fold snapshot
        // would keep the stale-row bug fix 1 just closed, just for
        // unfocused windows.
        if !edits.is_empty() {
            let fw = self.focused_window();
            let slot = self
                .windows
                .get(fw)
                .and_then(|w| w.as_ref())
                .map(|w| w.slot);
            if let Some(slot) = slot {
                let siblings: Vec<usize> = self
                    .windows
                    .iter()
                    .enumerate()
                    .filter_map(|(id, w)| w.as_ref().map(|w| (id, w.slot)))
                    .filter(|&(id, s)| s == slot && id != fw)
                    .map(|(id, _)| id)
                    .collect();
                for sid in siblings {
                    if let Some(folds) = self.window_folds.get_mut(&sid) {
                        for e in edits {
                            let edit_start = e.start_position.0 as usize;
                            let old_end_row = e.old_end_position.0 as usize;
                            let new_end_row = e.new_end_position.0 as usize;
                            let delta = new_end_row as isize - old_end_row as isize;
                            if delta == 0 {
                                let lo = edit_start;
                                let hi = old_end_row.max(new_end_row);
                                hjkl_buffer::invalidate_folds(folds, lo, hi);
                            } else {
                                let drop_end = old_end_row.max(edit_start);
                                let shift_threshold = drop_end.max(edit_start + 1);
                                hjkl_buffer::shift_folds_after_edit(
                                    folds,
                                    edit_start,
                                    drop_end,
                                    shift_threshold,
                                    delta,
                                );
                            }
                        }
                    }
                }
            }
        }
        // Keep the diff alignment current when a diff-mode buffer is edited.
        // Gen-keyed: a no-op unless the participating windows or buffer content
        // actually changed.
        if !self.diff_windows.is_empty() {
            self.refresh_diff_alignment();
            // Scroll-bind the partner window to the focused one.
            self.sync_diff_scroll();
        }
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
        use hjkl_mangler::{formatter_for_path, is_tool_installed};

        let filename = self.active().filename.clone();
        let Some(ref path) = filename else {
            return false;
        };

        let Some(formatter) = formatter_for_path(path) else {
            return false;
        };

        // Gate on "can we launch this binary?" — a spawn-only probe, NOT
        // `probe_tool` (which additionally requires `--version` to exit 0).
        // Some tools/wrappers print their version to stderr or exit non-zero
        // on `--version` (e.g. taplo via a mason shim), so the exit-0 probe
        // wrongly rejected them — `==` then silently did nothing while
        // format-on-save (which uses `is_tool_installed`) worked. Match the
        // save path. When the tool genuinely isn't on PATH, warn once and fall
        // back to the dumb auto-indent algo.
        let tool_name = formatter.tool_name().to_owned();
        if !is_tool_installed(&tool_name) {
            tracing::debug!(
                tool = %tool_name,
                "formatter not launchable; falling back to dumb algo"
            );
            self.bus.warn(format!("{tool_name}: not installed"));
            return false;
        }

        let source = self.active_editor().buffer().content_joined();
        let dirty_gen = self.active_editor().buffer().dirty_gen();
        let buffer_id = self.active().buffer_id;

        // `Path::parent()` of a bare relative filename (`foo.toml`) is
        // `Some("")`, not `None`; an empty root would break project discovery
        // and the formatter's working dir, so normalise it to `.`.
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_owned(),
            _ => std::path::PathBuf::from("."),
        };
        let project_root = types::find_project_root(&parent);

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
        self.bus.info(format!("{tool_name}: formatting\u{2026}"));

        // Arm the visual flash *immediately* on submit — the user sees
        // confirmation that `=` was accepted without waiting for the
        // (possibly multi-second) formatter to complete. Range is the
        // currently-visible viewport rows, so it covers whatever the
        // user is looking at.
        let vp = self.active_editor().host().viewport();
        let line_count = self.active_editor().buffer().row_count();
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

            // Stale check: drop the result only if the buffer's CONTENT changed
            // since the job was submitted. We compare the actual text, not
            // `dirty_gen` — non-content operations (notably fold open/close and
            // the per-window fold install) bump `dirty_gen` without changing the
            // text, which made `==` falsely reject every valid format result.
            let unchanged = {
                let current = self.slots[slot_idx].editor.buffer().content_joined();
                *current == *result.source
            };
            if !unchanged {
                tracing::debug!(
                    buffer_id = result.buffer_id,
                    "format result stale (content changed since submit); dropping"
                );
                // Dismiss the "formatting…" toast if it's still active.
                self.bus.clear_active();
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

                    // Dismiss the "formatting…" toast.
                    self.bus.clear_active();

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
                    self.bus.error(format!("{name}: not installed"));
                    redraw = true;
                }
                Err(e) => {
                    self.bus.error(format!("formatter: {e}"));
                    redraw = true;
                }
            }
        }
        redraw
    }

    // flush_pending_count_to_engine moved to count_prefix.rs
    // focus_below/above/left/right/next/previous, only_focused_window,
    // swap_with_sibling, move_window_to_new_tab, close_focused_window,
    // resize_height/width, equalize_layout, resize_split_to, equalize_split,
    // maximize_height, maximize_width moved to window.rs

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
        let directory = std::sync::Arc::new(hjkl_lang::LanguageDirectory::new()?);
        let mut syntax = syntax::layer_with_theme(theme.syntax.clone(), directory.clone());
        let buffer_id: BufferId = 0;
        // App::new uses bundled config defaults; main wires the XDG-merged
        // value via `with_config` after construction. For build_slot's
        // initial Options seed, the bundled defaults are correct because
        // tests never customize config and main re-applies overrides via
        // `apply_options` after `with_config`.
        let bootstrap_config = hjkl_app::config::Config::default();
        let no_file = filename.is_none();
        let mut slot = build_slot(&mut syntax, buffer_id, filename, &bootstrap_config)
            .map_err(|s| anyhow::anyhow!(s))?;

        // Create the app-wide shared register bank and inject it into the
        // initial slot's editor so all editors share one bank from the start.
        let shared_registers: std::sync::Arc<std::sync::Mutex<hjkl_engine::Registers>> =
            std::sync::Arc::new(std::sync::Mutex::new(hjkl_engine::Registers::default()));
        slot.editor.set_registers_arc(shared_registers.clone());

        // Same treatment for uppercase (global) marks — session-global in
        // vim, so every editor must share one bank from the start.
        let shared_global_marks: std::sync::Arc<std::sync::Mutex<hjkl_engine::GlobalMarks>> =
            std::sync::Arc::new(std::sync::Mutex::new(hjkl_engine::GlobalMarks::new()));
        slot.editor
            .set_global_marks_arc(shared_global_marks.clone());

        // Same treatment for the last `:s` command — session-global in vim,
        // so every editor must share one bank from the start.
        let shared_last_substitute: std::sync::Arc<
            std::sync::Mutex<Option<hjkl_engine::SubstituteCmd>>,
        > = std::sync::Arc::new(std::sync::Mutex::new(None));
        slot.editor
            .set_last_substitute_arc(shared_last_substitute.clone());

        // Same treatment for abbreviations — session-global in vim, so
        // every editor must share one bank from the start.
        let shared_abbrevs: std::sync::Arc<std::sync::Mutex<Vec<hjkl_engine::Abbrev>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        slot.editor.set_abbrevs_arc(shared_abbrevs.clone());

        // Same treatment for the last search pattern (the `"/` register) —
        // session-global in vim, so every editor must share one bank from
        // the start (audit B2).
        let shared_search: std::sync::Arc<std::sync::Mutex<hjkl_engine::SearchBank>> =
            std::sync::Arc::new(std::sync::Mutex::new(hjkl_engine::SearchBank::default()));
        slot.editor.set_search_arc(shared_search.clone());

        // Per-buffer changelist bank (audit B3). UNLIKE the banks above —
        // one Arc shared by the whole app session — this one is keyed by
        // `buffer_id`: seed the map with slot 0's bank up front so the
        // invariant "every editor's change_bank Arc == change_banks[its
        // buffer_id]" holds from the very first keystroke, before any
        // buffer switch runs `change_bank_for`.
        let mut change_banks: std::collections::HashMap<
            u64,
            std::sync::Arc<std::sync::Mutex<hjkl_engine::ChangeBank>>,
        > = std::collections::HashMap::new();
        let initial_change_bank =
            std::sync::Arc::new(std::sync::Mutex::new(hjkl_engine::ChangeBank::default()));
        change_banks.insert(buffer_id, initial_change_bank.clone());
        slot.editor.set_change_bank_arc(initial_change_bank);

        // Seed `"%` with the initial buffer's filename so `<C-r>%` / `"%p`
        // work from the first keystroke without requiring a buffer switch.
        {
            let fname = slot
                .filename
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned());
            slot.editor.registers_mut().set_filename(fname);
        }

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
            Some(crate::start_screen::new_with_theme(&theme))
        } else {
            None
        };

        // Single window pointing at slot 0. Its view editor is built by
        // `reconcile_window_editors()` (below), which copies the slot editor's
        // viewport — so any pre-event-loop scroll (e.g. +/pat search-on-open) is
        // preserved without a separate scroll mirror (#151 Phase D).
        let initial_window = window::Window::new(0);

        let default_leader = hjkl_app::config::Config::default().editor.leader;
        let mut app = Self {
            slots: vec![slot],
            windows: vec![Some(initial_window)],
            window_folds: std::collections::HashMap::new(),
            window_editors: std::collections::HashMap::new(),
            tabs: vec![window::Tab::new(window::LayoutTree::Leaf(0), 0)],
            active_tab: 0,
            next_window_id: 1,
            next_buffer_id: 1,
            prev_active: None,
            exit_requested: false,
            bus: HollerBus::new(),
            info_popup: None,
            command_field: None,
            command_completion_range: None,
            filter_field: None,
            filter_pending_range: None,
            search_field: None,
            picker: None,
            explorer: None,
            explorer_git_discard_confirm: None,
            icon_mode: hjkl_icons::IconMode::default(),
            pending_count: hjkl_vim::CountAccumulator::new(),
            g_chord_explicit_count: false,
            search_dir: SearchDir::Forward,
            last_cursor_shape: CursorShape::Block,
            syntax,
            git_worker: GitSignsWorker::new(),
            blame_worker: BlameWorker::new(),
            format_worker: hjkl_mangler::FormatWorker::spawn(),
            format_pending: HashSet::new(),
            directory,
            theme,
            preview_highlighters: std::sync::Mutex::new(std::collections::HashMap::new()),
            syntax_enabled: true,
            search_count_cache: std::cell::RefCell::new(None),
            // Seed the first-frame recompute via the event-loop's drain
            // flush so render::frame doesn't need a redundant sync parse.
            pending_recompute: true,
            last_signature_us: 0,
            last_synced_syntax_view: None,
            config: hjkl_app::config::Config::default(),
            start_screen,
            lsp: None,
            lsp_state: HashMap::new(),
            lsp_next_request_id: 0,
            lsp_pending: HashMap::new(),
            lsp_pending_seen_at: HashMap::new(),
            registers: shared_registers,
            global_marks: shared_global_marks,
            last_substitute: shared_last_substitute,
            abbrevs: shared_abbrevs,
            search: shared_search,
            change_banks,
            completion: None,
            pending_code_actions: Vec::new(),
            pending_code_actions_encoding: hjkl_lsp::PositionEncoding::default(),
            pending_ctrl_x: false,
            pending_prefix_at: None,
            which_key_active: false,
            which_key_sticky: false,
            chord_history: Vec::new(),
            suppress_open_notice: false,
            which_key_enabled: true,
            which_key_delay: std::time::Duration::from_millis(500),
            user_keymap_records: Vec::new(),
            replay_depth: 0,
            macro_replay: None,
            // Default to bundled config's value; main overrides via with_config
            // before crossterm capture is enabled.
            mouse_enabled: hjkl_app::config::Config::default().editor.mouse,
            mouse_flags: MouseFlags::all(),
            app_keymap: {
                let mut km = keymap_build::build_app_keymap(default_leader);
                // Chord timeout MUST exceed the default which-key delay
                // (500 ms) or the same loop iteration that activates the
                // popup also auto-resolves the chord and clears it. The
                // canonical default is sourced from EditorConfig::default()
                // so there is a single source of truth; with_config
                // overrides this before the event loop starts.
                km.set_timeout(std::time::Duration::from_millis(
                    hjkl_app::config::Config::default().editor.chord_timeout_ms,
                ));
                km
            },
            explorer_keymap: {
                let mut km = keymap_build::build_explorer_keymap(default_leader);
                km.set_timeout(std::time::Duration::from_millis(
                    hjkl_app::config::Config::default().editor.chord_timeout_ms,
                ));
                km
            },
            anvil_pool: hjkl_anvil::InstallPool::new(),
            anvil_handles: HashMap::new(),
            anvil_log: HashMap::new(),
            anvil_registry: hjkl_anvil::Registry::embedded().ok(),
            pending_state: None,
            last_ex_command: None,
            ex_history: Vec::new(),
            search_history_forward: Vec::new(),
            search_history_backward: Vec::new(),
            prompt_history_index: None,
            prompt_user_input: None,
            cmdline_win: None,
            mouse_click_tracker: mouse::MouseClickTracker::new(),
            context_menu: None,
            hover_popup: None,
            hover_timer: None,
            border_drag: None,
            indent_flash: None,
            force_clear_screen: false,
            debug_mode: false,
            confirming_substitute: None,
            pending_recovery: None,
            pending_disk_change: None,
            diff_windows: Vec::new(),
            diff_cache: None,
            last_input_at: std::time::Instant::now(),
            blame_prev_cursor: None,
            blame_cursor_moved_at: std::time::Instant::now(),
            colorscheme: "dark".to_string(),
            fs_watch: None,
            scroll_anim: None,
            hop: None,
            quickfix: hjkl_quickfix::QfList::new(),
            quickfix_open: false,
            loclist: hjkl_quickfix::QfList::new(),
            loclist_open: false,
            quickfix_older: Vec::new(),
            quickfix_newer: Vec::new(),
            loclist_older: Vec::new(),
            loclist_newer: Vec::new(),
            nvim_gvars: std::collections::HashMap::new(),
            nvim_bvars: std::collections::HashMap::new(),
            nvim_wvars: std::collections::HashMap::new(),
            last_frame_rect: None,
        };
        // Build the per-window view editor for the initial window (#151 Phase D).
        app.reconcile_window_editors();
        // Check for crash recovery on the initial file slot (#185).
        // If no recovery prompt is needed, arm the PID-lock swap immediately so
        // a concurrent second open of the same file sees it (even on unmodified
        // buffers). Crashes/kills leave the swap behind for recovery; graceful
        // exits remove it via cleanup_swaps_on_exit.
        if !no_file {
            let recovery = app.check_recovery_on_open(0);
            if !recovery {
                app.arm_swap_on_open(0);
            }
        }
        Ok(app)
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
            self.bus.info(if on { "mouse" } else { "nomouse" });
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
                self.bus.info(if on { "mouse" } else { "nomouse" });
            }
            Err(e) => {
                self.bus
                    .error(format!("E: failed to toggle mouse capture: {e}"));
            }
        }
    }

    pub fn with_config(mut self, config: hjkl_app::config::Config) -> Self {
        self.mouse_enabled = config.editor.mouse;
        self.which_key_enabled = config.which_key.enabled;
        self.which_key_delay = std::time::Duration::from_millis(config.which_key.delay_ms);
        // Rebuild the app keymap with the configured leader and timeout.
        //
        // Chord timeout (vim `timeoutlen`) must be strictly greater than
        // `which_key.delay_ms`, otherwise the same event-loop iteration
        // that activates the popup also resolves the chord and clears it
        // — the popup never paints.
        if config.editor.chord_timeout_ms <= config.which_key.delay_ms {
            tracing::warn!(
                chord_timeout_ms = config.editor.chord_timeout_ms,
                which_key_delay_ms = config.which_key.delay_ms,
                "chord_timeout_ms ({}) <= which_key.delay_ms ({}); chord-resolve race may re-emerge",
                config.editor.chord_timeout_ms,
                config.which_key.delay_ms,
            );
        }
        let leader = config.editor.leader;
        let timeout = Duration::from_millis(config.editor.chord_timeout_ms);
        self.app_keymap = keymap_build::build_app_keymap(leader);
        self.app_keymap.set_timeout(timeout);
        self.explorer_keymap = keymap_build::build_explorer_keymap(leader);
        self.explorer_keymap.set_timeout(timeout);
        // Resolve the explorer icon set. Explicit modes apply directly; `auto`
        // (and anything unrecognized) assumes a Nerd Font — terminals can't be
        // probed for font/glyph coverage, so `icons=unicode`/`ascii` is the
        // reliable fallback for non-Nerd setups.
        self.icon_mode = hjkl_icons::IconMode::from_config(&config.editor.icons)
            .unwrap_or(hjkl_icons::IconMode::Nerd);
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
                hjkl_app::editorconfig::overlay_for_path(&mut opts, p);
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
        // The read-only git blame view is its own mode (engine-owned, masked
        // to Normal by `is_blame`).
        if self.active_editor().is_blame() {
            return "BLAME";
        }
        // Vim projects its precise mode onto the discipline-agnostic CoarseMode;
        // the badge text is the discipline's concern. Read CoarseMode rather
        // than VimMode so it stays behind the engine's discipline seam.
        match self.active_editor().coarse_mode() {
            CoarseMode::Normal => "NORMAL",
            CoarseMode::Insert => "INSERT",
            CoarseMode::Select => "VISUAL",
            CoarseMode::SelectLine => "VISUAL LINE",
            CoarseMode::SelectBlock => "VISUAL BLOCK",
        }
    }

    /// Public entry point for loading an extra file from the CLI into a new
    /// slot without switching the active buffer. Used by `main` to handle
    /// `hjkl a.rs b.rs c.rs` — slots 1…N are populated here after `App::new`
    /// opens slot 0.
    pub fn open_extra(&mut self, path: PathBuf) -> Result<(), String> {
        let idx = self.open_new_slot(path)?;
        // Run the swap crash-recovery / multi-instance lock check for this
        // CLI-opened slot, same as the startup slot 0 and `:e`. Without this,
        // `hjkl a b c` skipped the check for every file after the first —
        // leaving locked secondaries editable (#185). When not locked / not
        // recovering, arm the swap so the lock exists immediately.
        let recovering = self.check_recovery_on_open(idx);
        if !recovering {
            self.arm_swap_on_open(idx);
        }
        Ok(())
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
        use crate::menu::MenuActionKind;
        // Dispatch through the exhaustive `MenuActionKind` view so no `_ => {}`
        // wildcard is needed despite `MenuAction` being `#[non_exhaustive]`.
        action.dispatch(|kind| match kind {
            MenuActionKind::Copy => self.menu_copy(),
            MenuActionKind::Cut => self.menu_cut(),
            MenuActionKind::Paste => self.menu_paste(),
            MenuActionKind::TabClose => self.dispatch_ex("tabclose"),
            MenuActionKind::TabCloseOthers => self.do_tabonly(),
            MenuActionKind::TabCloseRight => self.close_tabs_to_right(),
            MenuActionKind::TabCloseLeft => self.close_tabs_to_left(),
            // ── LSP actions ──────────────────────────────────────────────────
            MenuActionKind::LspGotoDefinition => self.lsp_goto_definition(),
            MenuActionKind::LspGotoReferences => self.lsp_goto_references(),
            MenuActionKind::LspHover => self.lsp_hover(),
            MenuActionKind::LspCodeActions => self.lsp_code_actions(),
            MenuActionKind::LspFormat => self.lsp_format(),
            // ── Gutter / diagnostic menu (#114 P6) ────────────────────────────
            MenuActionKind::DiagnosticDetail => self.show_diag_at_cursor(),
            // Rename needs a new name from the user.  The ex command
            // `:Rename <newname>` is the supported entry point — mirror the
            // same status-message prompt the `<leader>rn` keybind uses so the
            // user knows how to proceed.
            MenuActionKind::LspRename => {
                self.bus.info("use :Rename <newname> to rename");
            }
            // ── Status-line menu actions ──────────────────────────────────────
            MenuActionKind::LspRestart => self.restart_lsp(),
            MenuActionKind::OpenFilePicker => self.open_picker(),
            // ── Split-border menu actions ─────────────────────────────────────
            MenuActionKind::WindowEqualize => self.equalize_layout(),
            MenuActionKind::WindowClose => self.dispatch_ex("close"),
            // ── Picker overlay menu actions ───────────────────────────────────
            MenuActionKind::PickerOpen => self.picker_accept(),
            MenuActionKind::PickerOpenSplit => self.picker_open_in_split(),
            MenuActionKind::PickerOpenVSplit => self.picker_open_in_vsplit(),
            MenuActionKind::PickerOpenTab => self.picker_open_in_tab(),
            MenuActionKind::PickerCopyPath => self.picker_copy_path(),
            // ── Gutter / git-hunk menu (#114 P6/P10, #115) ────────────────────
            MenuActionKind::GitStageHunk => self.git_stage_hunk_at_cursor(),
            MenuActionKind::GitUnstageHunk => self.git_unstage_hunk_at_cursor(),
            MenuActionKind::GitRevertHunk => self.git_revert_hunk_at_cursor(),
            MenuActionKind::GitShowHunk => self.git_show_hunk_diff(),
            MenuActionKind::Separator | MenuActionKind::Info => {} // no-op
        });
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
        let vim_mode = self.active_editor().vim_mode();
        match vim_mode {
            VimMode::VisualBlock => {
                if let Some((top_row, bot_row, left_col, right_col)) =
                    self.active_editor().block_highlight()
                {
                    self.active_editor_mut()
                        .yank_block(top_row, bot_row, left_col, right_col, '"');
                }
            }
            VimMode::Visual => {
                if let Some((start, end)) = self.active_editor().char_highlight() {
                    self.active_editor_mut()
                        .yank_range(start, end, RangeKind::Inclusive, '"');
                }
            }
            VimMode::VisualLine => {
                if let Some((top_row, bot_row)) = self.active_editor().line_highlight() {
                    self.active_editor_mut().yank_range(
                        (top_row, 0),
                        (bot_row, usize::MAX),
                        RangeKind::Linewise,
                        '"',
                    );
                }
            }
            _ => {
                // No selection — yank current line (yy semantics).
                self.active_editor_mut().yank_to_eol(1);
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
        let vim_mode = self.active_editor().vim_mode();
        match vim_mode {
            VimMode::VisualBlock => {
                if let Some((top_row, bot_row, left_col, right_col)) =
                    self.active_editor().block_highlight()
                {
                    self.active_editor_mut()
                        .delete_block(top_row, bot_row, left_col, right_col, '"');
                    // Exit visual mode.
                    use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
                    hjkl_vim_tui::handle_key(
                        self.active_editor_mut(),
                        CtKeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                    );
                }
            }
            VimMode::Visual => {
                if let Some((start, end)) = self.active_editor().char_highlight() {
                    self.active_editor_mut()
                        .delete_range(start, end, RangeKind::Inclusive, '"');
                    use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
                    hjkl_vim_tui::handle_key(
                        self.active_editor_mut(),
                        CtKeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                    );
                }
            }
            VimMode::VisualLine => {
                if let Some((top_row, bot_row)) = self.active_editor().line_highlight() {
                    self.active_editor_mut().delete_range(
                        (top_row, 0),
                        (bot_row, usize::MAX),
                        RangeKind::Linewise,
                        '"',
                    );
                    use crossterm::event::{KeyCode, KeyEvent as CtKeyEvent, KeyModifiers};
                    hjkl_vim_tui::handle_key(
                        self.active_editor_mut(),
                        CtKeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                    );
                }
            }
            _ => {
                // No selection — delete current line (dd semantics):
                // yank_to_eol then delete_to_eol is not quite right for full-line;
                // use the engine's delete_range for the full current row.
                let (row, _) = self.active_editor().cursor();
                self.active_editor_mut().delete_range(
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
        if let Some(text) = self.active_editor_mut().host_mut().read_clipboard() {
            self.active_editor_mut().set_yank(text);
        }
        self.active_editor_mut().paste_after(1);
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

    /// True when a chord is mid-flight across any of the three pending owners:
    /// the app keymap trie, the app-side `pending_state` FSM, or the engine's
    /// own pending chord.
    pub(crate) fn any_chord_pending(&self) -> bool {
        self.pending_state.is_some()
            || self.active_editor().is_chord_pending()
            || !self
                .ctx_keymap()
                .pending(crate::app::keymap::HjklMode::Normal)
                .is_empty()
    }

    /// Cancel every in-flight chord (trie, app `pending_state`, engine pending)
    /// and reset the count prefix + which-key prefix timer. Does not touch
    /// `chord_history` (callers manage that).
    pub(crate) fn cancel_all_pending(&mut self) {
        self.app_keymap.reset(crate::app::keymap::HjklMode::Normal);
        self.explorer_keymap
            .reset(crate::app::keymap::HjklMode::Normal);
        self.pending_count.reset();
        self.pending_state = None;
        let _ = self.active_editor_mut().take_pending();
        self.clear_prefix_state();
    }

    /// Return a shared reference to the keymap that owns chord state and
    /// which-key entries for the current focus context.
    ///
    /// When the file-explorer sidebar is focused the explorer-specific keymap
    /// is returned; otherwise the global `app_keymap`.
    pub(crate) fn ctx_keymap(&self) -> &Keymap<AppAction, keymap::HjklMode> {
        if self.explorer_buf_focused() {
            &self.explorer_keymap
        } else {
            &self.app_keymap
        }
    }

    /// Return the currently-pending chord buffer for Normal mode, or an empty
    /// `Vec` when no prefix is active.
    ///
    /// The caller uses this to drive `which_key::entries_for` directly —
    /// the static `Prefix` enum is no longer needed.
    ///
    /// **Popup-mode note:** this intentionally reads only the Normal pending
    /// buffer.  When the editor is in Visual or Insert mode the Normal buffer
    /// is empty, so callers see an empty `Vec` and suppress the popup.  See
    /// the comment in `render.rs::which_key_popup` for the full rationale.
    pub fn active_which_key_prefix(&self) -> Vec<hjkl_keymap::KeyEvent> {
        let trie = self.ctx_keymap().pending(keymap::HjklMode::Normal);
        if !trie.is_empty() {
            return trie.to_vec();
        }
        // Engine-FSM chords (g/z/op-pending) don't surface through app_keymap
        // — synthesize a prefix so descriptors::children_for can list entries.
        // (These are never active when the explorer is focused, so reading
        // pending_state here is always correct for non-explorer context.)
        if let Some(state) = self.pending_state {
            use hjkl_vim::PendingState;
            let ch = match state {
                PendingState::AfterG { .. } => Some('g'),
                PendingState::AfterZ { .. } => Some('z'),
                PendingState::AfterOp { op, .. } => Some(op.double_char()),
                _ => None,
            };
            if let Some(c) = ch {
                return vec![hjkl_keymap::KeyEvent::char(c)];
            }
        }
        Vec::new()
    }

    // km_to_crossterm, replay_to_engine, route_chord_key, route_chord_key_inner moved to chord_routing.rs

    /// Push `entry` into a history ring (cap 100, skip consecutive duplicates).
    /// Delegates to [`hjkl_prompt::push_history`].
    pub(crate) fn push_history(ring: &mut Vec<String>, entry: &str) {
        hjkl_prompt::push_history(ring, entry);
    }

    /// `@:` — replay the last ex command. No-op when nothing has been
    /// dispatched yet. Phase 5d of kryptic-sh/hjkl#71.
    pub(crate) fn replay_last_ex(&mut self) {
        if let Some(cmd) = self.last_ex_command.clone() {
            self.dispatch_ex(&cmd);
        }
    }

    /// Replay a slice of `hjkl_keymap::KeyEvent`s straight to the engine,
    /// converting each one to a crossterm `KeyEvent` via the shared translator.
    pub(crate) fn replay_km_events_to_engine(&mut self, events: &[hjkl_keymap::KeyEvent]) {
        for km_ev in events {
            let ct_ev = crate::keymap_translate::to_crossterm(km_ev);
            hjkl_vim_tui::handle_key(self.active_editor_mut(), ct_ev);
        }
    }
}

/// Return the current `HjklMode` based on the active editor's vim mode.
/// Returns `None` for modes with no keymap equivalent (currently none, but
/// Terminal mode would be `None` if ever added here).
pub(crate) fn current_km_mode(app: &App) -> Option<keymap::HjklMode> {
    keymap::map_mode_to_km_mode(keymap::map_mode_for_vim(app.active_editor().vim_mode())?)
}
