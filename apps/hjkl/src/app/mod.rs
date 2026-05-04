//! `App` — owns the editor + host, drives the event loop.

use anyhow::Result;
use hjkl_buffer::Buffer;
use hjkl_engine::{BufferEdit, Host};
use hjkl_engine::{CursorShape, Editor, Options, VimMode};
use hjkl_form::TextFieldEditor;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::host::TuiHost;
use crate::syntax::{self, BufferId, SyntaxLayer};

mod buffer_ops;
mod event_loop;
mod ex_dispatch;
mod picker_glue;
mod prompt;
mod syntax_glue;
#[cfg(test)]
mod tests;

/// Height reserved for the status line at the bottom of the screen.
pub const STATUS_LINE_HEIGHT: u16 = 1;

/// Height of the buffer/tab line at the top of the screen, when shown.
pub const BUFFER_LINE_HEIGHT: u16 = 1;

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
    /// Index into `slots` of the currently active buffer.
    active: usize,
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
    if let Some(ref p) = path {
        match std::fs::read_to_string(p) {
            Ok(content) => {
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
    if let Some(ref p) = path {
        syntax.set_language_for_path(buffer_id, p);
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
        git_signs: Vec::new(),
        last_git_dirty_gen: None,
        last_git_refresh_at: Instant::now(),
        last_recompute_at: Instant::now() - Duration::from_secs(1),
        last_recompute_key: key,
        saved_hash: 0,
        saved_len: 0,
    };
    slot.snapshot_saved();
    Ok(slot)
}

impl App {
    /// Return a shared reference to the active buffer slot.
    pub fn active(&self) -> &BufferSlot {
        &self.slots[self.active]
    }

    /// Return a mutable reference to the active buffer slot.
    pub fn active_mut(&mut self) -> &mut BufferSlot {
        &mut self.slots[self.active]
    }

    /// Return a shared slice of all buffer slots.
    pub fn slots(&self) -> &[BufferSlot] {
        &self.slots
    }

    /// Return the index of the currently active slot.
    pub fn active_index(&self) -> usize {
        self.active
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

        Ok(Self {
            slots: vec![slot],
            active: 0,
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
