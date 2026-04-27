//! `App` — owns the editor + host, drives the event loop.

use anyhow::Result;
use crossterm::{
    cursor::SetCursorStyle,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
};
use hjkl_buffer::Buffer;
use hjkl_editor::runtime::ex::{self, ExEffect};
use hjkl_engine::{BufferEdit, Host, Input as EngineInput, Key as EngineKey, Query};
use hjkl_engine::{CursorShape, Editor, Options, VimMode};
use hjkl_form::TextFieldEditor;
use hjkl_tree_sitter::DotFallbackTheme;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Stdout;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::host::TuiHost;
use crate::render;
use crate::syntax::{self, BufferId, SyntaxLayer};

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

/// Wrapper that lets `App::picker` hold either a file picker or a buffer
/// picker without boxing the trait.
pub enum AnyPicker {
    File(crate::picker::FilePicker),
    Buffer(crate::picker::BufferPicker),
}

/// Top-level application state. Everything the event loop and renderer need.
pub struct App {
    /// All open buffer slots. Never empty — always at least one slot.
    pub slots: Vec<BufferSlot>,
    /// Index into `slots` of the currently active buffer.
    pub active: usize,
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
    /// Active `:` command input. `Some` while the user is typing an ex
    /// command. Backed by a vim-grammar [`TextFieldEditor`] so motions
    /// (h/l/w/b/dw/diw/...) work inside the prompt.
    pub command_field: Option<TextFieldEditor>,
    /// Active `/` (forward) / `?` (backward) search prompt.
    pub search_field: Option<TextFieldEditor>,
    /// Active file or buffer picker overlay.
    pub picker: Option<AnyPicker>,
    /// `true` after the user pressed `<Space>` in normal mode and we're
    /// waiting for the next key to resolve the leader sequence.
    pub pending_leader: bool,
    /// Pending buffer-motion prefix key in normal mode. Set to `'g'`
    /// after pressing `g`, `']'` after `]`, `'['` after `[`. Cleared
    /// once the motion is resolved or forwarded to the engine.
    pub pending_buffer_motion: Option<char>,
    /// Direction of the active `search_field`.
    pub search_dir: SearchDir,
    /// Last cursor shape we emitted to the terminal.
    last_cursor_shape: CursorShape,
    /// Tree-sitter syntax highlighting layer. Owns the registry, highlighter,
    /// and active theme. Multiplexed by BufferId (Phase A API).
    syntax: SyntaxLayer,
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
        let mut buffer = Buffer::new();
        let mut is_new_file = false;
        if let Some(ref path) = filename {
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    // Strip one trailing newline (vim default): a file
                    // ending in `\n` is the EOL of its last line, not
                    // a separator before an empty trailing line. Save
                    // re-appends one.
                    let content = content.strip_suffix('\n').unwrap_or(&content);
                    BufferEdit::replace_all(&mut buffer, content);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // New file — buffer stays empty, filename retained.
                    is_new_file = true;
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("{}: {}", path.display(), e));
                }
            }
        }

        let host = TuiHost::new();
        let options = Options {
            readonly,
            ..Options::default()
        };
        let mut editor = Editor::new(buffer, host, options);

        // +N line jump — 1-based, clamp to buffer.
        if let Some(n) = goto_line {
            editor.goto_line(n);
        }

        // +/pattern initial search — compile the pattern and set it.
        if let Some(pat) = search_pattern {
            match regex::Regex::new(&pat) {
                Ok(re) => {
                    editor.set_search_pattern(Some(re));
                    editor.search_advance_forward(false);
                }
                Err(e) => {
                    eprintln!("hjkl: bad search pattern: {e}");
                }
            }
        }

        // Build syntax layer (dark theme default) and detect language for
        // the opened file, then run an initial highlight pass.
        let mut syntax = syntax::default_layer();
        let buffer_id: BufferId = 0;
        if let Some(ref path) = filename {
            syntax.set_language_for_path(buffer_id, path);
        }
        // Sync host viewport to the real terminal size before sizing the
        // preview / submitting the initial parse.
        if let Ok(size) = crossterm::terminal::size() {
            let vp = editor.host_mut().viewport_mut();
            vp.width = size.0;
            vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
        }
        let initial_vp_top = editor.host().viewport().top_row;
        let initial_vp_height = editor.host().viewport().height as usize;
        // Synchronous viewport-only preview so the first frame has
        // highlights regardless of file size or viewport position.
        let mut initial_signs: Vec<hjkl_buffer::Sign> = Vec::new();
        let initial_dg = editor.buffer().dirty_gen();
        if let Some(out) = syntax.preview_render(
            buffer_id,
            editor.buffer(),
            initial_vp_top,
            initial_vp_height,
        ) {
            editor.install_ratatui_syntax_spans(out.spans);
        }
        syntax.submit_render(
            buffer_id,
            editor.buffer(),
            initial_vp_top,
            initial_vp_height,
        );
        let initial_key: Option<(u64, usize, usize)> =
            if let Some(out) = syntax.wait_for_initial_result(Duration::from_millis(150)) {
                let key = out.key;
                editor.install_ratatui_syntax_spans(out.spans);
                initial_signs = out.signs;
                Some(key)
            } else {
                Some((initial_dg, initial_vp_top, initial_vp_height))
            };
        // Drain any ContentEdit / reset state seeded during construction.
        let _ = editor.take_content_edits();
        let _ = editor.take_content_reset();

        let mut slot = BufferSlot {
            buffer_id,
            editor,
            filename,
            dirty: false,
            is_new_file,
            is_untracked: false,
            diag_signs: initial_signs,
            git_signs: Vec::new(),
            last_git_dirty_gen: None,
            last_git_refresh_at: Instant::now(),
            last_recompute_at: Instant::now() - Duration::from_secs(1),
            last_recompute_key: initial_key,
            saved_hash: 0,
            saved_len: 0,
        };
        // Snapshot the loaded content so undo-to-saved clears dirty.
        slot.snapshot_saved();

        Ok(Self {
            slots: vec![slot],
            active: 0,
            next_buffer_id: 1,
            prev_active: None,
            exit_requested: false,
            status_message: None,
            command_field: None,
            search_field: None,
            picker: None,
            pending_leader: false,
            pending_buffer_motion: None,
            search_dir: SearchDir::Forward,
            last_cursor_shape: CursorShape::Block,
            syntax,
            perf_overlay: false,
            last_recompute_us: 0,
            last_install_us: 0,
            last_signature_us: 0,
            last_git_us: 0,
            last_perf: crate::syntax::PerfBreakdown::default(),
            recompute_hits: 0,
            recompute_throttled: 0,
            recompute_runs: 0,
        })
    }

    /// Recompute git diff signs from the current buffer content (vs
    /// the HEAD blob) when `dirty_gen` has advanced since the last rebuild.
    fn refresh_git_signs(&mut self) {
        self.refresh_git_signs_inner(false);
    }

    fn refresh_git_signs_force(&mut self) {
        self.refresh_git_signs_inner(true);
    }

    fn refresh_git_signs_inner(&mut self, force: bool) {
        const HUGE_FILE_LINES: u32 = 50_000;
        const REFRESH_MIN_INTERVAL: Duration = Duration::from_millis(250);

        let path = match self.active().filename.as_deref() {
            Some(p) => p.to_path_buf(),
            None => {
                let slot = self.active_mut();
                slot.git_signs.clear();
                slot.last_git_dirty_gen = None;
                return;
            }
        };
        let dg = self.active().editor.buffer().dirty_gen();
        if !force && self.active().last_git_dirty_gen == Some(dg) {
            return;
        }
        if !force && self.active().editor.buffer().line_count() >= HUGE_FILE_LINES {
            return;
        }
        let now = Instant::now();
        if !force && now.duration_since(self.active().last_git_refresh_at) < REFRESH_MIN_INTERVAL {
            return;
        }

        let lines = self.active().editor.buffer().lines();
        let mut bytes = lines.join("\n").into_bytes();
        if !bytes.is_empty() {
            bytes.push(b'\n');
        }
        let git_signs = crate::git::signs_for_bytes(&path, &bytes);
        let is_untracked = crate::git::is_untracked(&path);
        let slot = self.active_mut();
        slot.git_signs = git_signs;
        slot.is_untracked = is_untracked;
        slot.last_git_dirty_gen = Some(dg);
        slot.last_git_refresh_at = now;
    }

    /// Main event loop. Draws every frame, routes key events through
    /// the vim FSM, handles resize, exits on Ctrl-C.
    pub fn run(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        loop {
            // Update host viewport dimensions from the current terminal size.
            {
                let size = terminal.size()?;
                let vp = self.active_mut().editor.host_mut().viewport_mut();
                vp.width = size.width;
                vp.height = size.height.saturating_sub(STATUS_LINE_HEIGHT);
            }

            // Emit cursor shape before the draw call, once per transition.
            let current_shape = if let Some(ref f) = self.command_field {
                match f.vim_mode() {
                    hjkl_form::VimMode::Insert => CursorShape::Bar,
                    _ => CursorShape::Block,
                }
            } else if let Some(ref f) = self.search_field {
                match f.vim_mode() {
                    hjkl_form::VimMode::Insert => CursorShape::Bar,
                    _ => CursorShape::Block,
                }
            } else {
                self.active().editor.host().cursor_shape()
            };
            if current_shape != self.last_cursor_shape {
                match current_shape {
                    CursorShape::Block => {
                        let _ = execute!(terminal.backend_mut(), SetCursorStyle::SteadyBlock);
                    }
                    CursorShape::Bar => {
                        let _ = execute!(terminal.backend_mut(), SetCursorStyle::SteadyBar);
                    }
                    CursorShape::Underline => {
                        let _ = execute!(terminal.backend_mut(), SetCursorStyle::SteadyUnderScore);
                    }
                }
                self.last_cursor_shape = current_shape;
            }

            // Draw the current frame.
            terminal.draw(|frame| render::frame(frame, self))?;

            // Wait for the next event with a 120 ms ceiling.
            if !event::poll(Duration::from_millis(120))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        if self.command_field.is_some() {
                            self.command_field = None;
                            continue;
                        }
                        if self.search_field.is_some() {
                            self.cancel_search_prompt();
                            continue;
                        }
                        break;
                    }

                    self.status_message = None;

                    // ── Command palette (`:` prompt) ─────────────────────────
                    if self.command_field.is_some() {
                        self.handle_command_field_key(key);
                        if self.exit_requested {
                            break;
                        }
                        continue;
                    }

                    // ── Search prompt (`/` `?`) ──────────────────────────────
                    if self.search_field.is_some() {
                        self.handle_search_field_key(key);
                        if self.exit_requested {
                            break;
                        }
                        continue;
                    }

                    // ── Picker overlay ────────────────────────────────────────
                    if self.picker.is_some() {
                        self.handle_picker_key(key);
                        if self.exit_requested {
                            break;
                        }
                        continue;
                    }

                    // ── Leader resolution ────────────────────────────────────
                    if self.pending_leader && self.active().editor.vim_mode() == VimMode::Normal {
                        self.pending_leader = false;
                        if key.modifiers == KeyModifiers::NONE {
                            match key.code {
                                KeyCode::Char(' ') | KeyCode::Char('f') => {
                                    self.open_picker();
                                }
                                KeyCode::Char('b') => {
                                    self.open_buffer_picker();
                                }
                                _ => {}
                            }
                        }
                        continue;
                    }

                    // ── Leader prefix ────────────────────────────────────────
                    if key.code == KeyCode::Char(' ')
                        && key.modifiers == KeyModifiers::NONE
                        && self.active().editor.vim_mode() == VimMode::Normal
                    {
                        self.pending_leader = true;
                        continue;
                    }

                    // ── Alt-buffer toggle (Ctrl-^ / Ctrl-6) ─────────────────
                    if self.active().editor.vim_mode() == VimMode::Normal
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                        && (key.code == KeyCode::Char('^') || key.code == KeyCode::Char('6'))
                    {
                        self.buffer_alt();
                        continue;
                    }

                    // ── Shift-H / Shift-L cycle buffers ──────────────────────
                    // Only when more than one buffer is open; with a single
                    // slot fall through to the engine's H/L viewport motions.
                    if self.active().editor.vim_mode() == VimMode::Normal
                        && self.slots.len() > 1
                        && (key.modifiers == KeyModifiers::SHIFT
                            || key.modifiers == KeyModifiers::NONE)
                    {
                        if key.code == KeyCode::Char('H') {
                            self.buffer_prev();
                            continue;
                        }
                        if key.code == KeyCode::Char('L') {
                            self.buffer_next();
                            continue;
                        }
                    }

                    // ── Buffer-motion pending state ──────────────────────────
                    if self.active().editor.vim_mode() == VimMode::Normal
                        && key.modifiers == KeyModifiers::NONE
                    {
                        if let Some(prefix) = self.pending_buffer_motion.take() {
                            match (prefix, key.code) {
                                ('g', KeyCode::Char('t')) => {
                                    self.buffer_next();
                                    continue;
                                }
                                ('g', KeyCode::Char('T')) => {
                                    self.buffer_prev();
                                    continue;
                                }
                                (']', KeyCode::Char('b')) => {
                                    self.buffer_next();
                                    continue;
                                }
                                ('[', KeyCode::Char('b')) => {
                                    self.buffer_prev();
                                    continue;
                                }
                                // Didn't match — forward only the current key;
                                // drop the pending prefix (g/]/[ alone has no
                                // other mapped meaning in our engine yet).
                                _ => {
                                    self.active_mut().editor.handle_key(key);
                                    continue;
                                }
                            }
                        }
                    } else {
                        // Any non-Normal key clears the pending motion.
                        self.pending_buffer_motion = None;
                    }

                    // ── Intercept `:` in Normal mode ─────────────────────────
                    if key.code == KeyCode::Char(':')
                        && key.modifiers == KeyModifiers::NONE
                        && self.active().editor.vim_mode() == VimMode::Normal
                    {
                        self.open_command_prompt();
                        continue;
                    }

                    // ── Intercept `/` and `?` in Normal mode ─────────────────
                    if key.modifiers == KeyModifiers::NONE
                        && self.active().editor.vim_mode() == VimMode::Normal
                    {
                        if key.code == KeyCode::Char('/') {
                            self.open_search_prompt(SearchDir::Forward);
                            continue;
                        }
                        if key.code == KeyCode::Char('?') {
                            self.open_search_prompt(SearchDir::Backward);
                            continue;
                        }
                    }

                    // ── Set pending buffer-motion prefix ─────────────────────
                    if self.active().editor.vim_mode() == VimMode::Normal
                        && key.modifiers == KeyModifiers::NONE
                        && matches!(
                            key.code,
                            KeyCode::Char('g') | KeyCode::Char(']') | KeyCode::Char('[')
                        )
                        && let KeyCode::Char(c) = key.code
                    {
                        self.pending_buffer_motion = Some(c);
                        // Fall through: also forward the key to the engine
                        // so its own `g`-pending state is updated correctly
                        // (the engine handles gj/gk/gg/G etc).
                    }

                    // ── Normal editor key handling ───────────────────────────
                    self.active_mut().editor.handle_key(key);

                    // Drain dirty for the persistent UI flag.
                    if self.active_mut().editor.take_dirty() {
                        let elapsed = self.active_mut().refresh_dirty_against_saved();
                        self.last_signature_us = elapsed;
                        if self.active().dirty {
                            self.active_mut().is_new_file = false;
                        }
                    }
                    // Fan engine ContentEdits into the syntax tree.
                    let buffer_id = self.active().buffer_id;
                    if self.active_mut().editor.take_content_reset() {
                        self.syntax.reset(buffer_id);
                    }
                    let edits = self.active_mut().editor.take_content_edits();
                    if !edits.is_empty() {
                        self.syntax.apply_edits(buffer_id, &edits);
                    }
                    self.recompute_and_install();
                }
                Event::Resize(w, h) => {
                    let vp = self.active_mut().editor.host_mut().viewport_mut();
                    vp.width = w;
                    vp.height = h.saturating_sub(STATUS_LINE_HEIGHT);
                }
                _ => {}
            }

            if self.exit_requested {
                break;
            }
        }
        Ok(())
    }

    /// Open the fuzzy file picker.
    pub fn open_picker(&mut self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let source = crate::picker::FileSource::new(cwd);
        self.picker = Some(AnyPicker::File(crate::picker::FilePicker::new(source)));
        self.pending_leader = false;
    }

    /// Open the buffer picker over the currently open slots.
    pub fn open_buffer_picker(&mut self) {
        let source = crate::picker::BufferSource::new(
            &self.slots,
            |s| {
                s.filename
                    .as_ref()
                    .and_then(|p| p.to_str())
                    .unwrap_or("[No Name]")
                    .to_owned()
            },
            |s| s.dirty,
        );
        self.picker = Some(AnyPicker::Buffer(crate::picker::BufferPicker::new(source)));
        self.pending_leader = false;
    }

    fn handle_picker_key(&mut self, key: crossterm::event::KeyEvent) {
        let event = match self.picker.as_mut() {
            Some(AnyPicker::File(p)) => p.handle_key(key),
            Some(AnyPicker::Buffer(p)) => p.handle_key(key),
            None => return,
        };
        match event {
            crate::picker::PickerEvent::None => {}
            crate::picker::PickerEvent::Cancel => {
                self.picker = None;
            }
            crate::picker::PickerEvent::Select(action) => {
                self.picker = None;
                self.dispatch_picker_action(action);
            }
        }
    }

    fn dispatch_picker_action(&mut self, action: crate::picker::PickerAction) {
        match action {
            crate::picker::PickerAction::OpenPath(path) => {
                let s = path.to_string_lossy().to_string();
                self.do_edit(&s, false);
            }
            crate::picker::PickerAction::SwitchBuffer(idx) => {
                if idx < self.slots.len() {
                    self.switch_to(idx);
                }
            }
        }
    }

    fn open_command_prompt(&mut self) {
        let mut field = TextFieldEditor::new(true);
        field.enter_insert_at_end();
        self.command_field = Some(field);
    }

    fn handle_command_field_key(&mut self, key: crossterm::event::KeyEvent) {
        let input: EngineInput = key.into();
        let field = match self.command_field.as_mut() {
            Some(f) => f,
            None => return,
        };

        if input.key == EngineKey::Enter {
            let text = field.text();
            self.command_field = None;
            self.dispatch_ex(text.trim());
            return;
        }

        if input.key == EngineKey::Esc {
            if field.text().is_empty() {
                self.command_field = None;
            } else if field.vim_mode() == VimMode::Insert {
                field.enter_normal();
            } else {
                self.command_field = None;
            }
            return;
        }

        field.handle_input(input);
    }

    fn open_search_prompt(&mut self, dir: SearchDir) {
        let mut field = TextFieldEditor::new(true);
        field.enter_insert_at_end();
        self.search_field = Some(field);
        self.search_dir = dir;
        self.active_mut().editor.set_search_pattern(None);
    }

    fn cancel_search_prompt(&mut self) {
        self.search_field = None;
        let last = self.active().editor.last_search().map(str::to_owned);
        match last {
            Some(p) if !p.is_empty() => {
                if let Ok(re) = regex::Regex::new(&p) {
                    self.active_mut().editor.set_search_pattern(Some(re));
                } else {
                    self.active_mut().editor.set_search_pattern(None);
                }
            }
            _ => self.active_mut().editor.set_search_pattern(None),
        }
    }

    fn handle_search_field_key(&mut self, key: crossterm::event::KeyEvent) {
        let input: EngineInput = key.into();
        let field = match self.search_field.as_mut() {
            Some(f) => f,
            None => return,
        };

        if input.key == EngineKey::Enter {
            let pattern = field.text();
            self.search_field = None;
            self.commit_search(&pattern);
            return;
        }

        if input.key == EngineKey::Esc {
            if field.text().is_empty() {
                self.cancel_search_prompt();
                return;
            }
            if field.vim_mode() == VimMode::Insert {
                field.enter_normal();
            } else {
                self.cancel_search_prompt();
            }
            return;
        }

        let dirty = field.handle_input(input);
        if dirty {
            self.live_preview_search();
        }
    }

    fn live_preview_search(&mut self) {
        let pattern = match self.search_field.as_ref() {
            Some(f) => f.text(),
            None => return,
        };
        if pattern.is_empty() {
            self.active_mut().editor.set_search_pattern(None);
            return;
        }
        let case_insensitive = self.active().editor.settings().ignore_case
            && !(self.active().editor.settings().smartcase
                && pattern.chars().any(|c| c.is_uppercase()));
        let effective: std::borrow::Cow<'_, str> = if case_insensitive {
            std::borrow::Cow::Owned(format!("(?i){pattern}"))
        } else {
            std::borrow::Cow::Borrowed(pattern.as_str())
        };
        match regex::Regex::new(&effective) {
            Ok(re) => self.active_mut().editor.set_search_pattern(Some(re)),
            Err(_) => self.active_mut().editor.set_search_pattern(None),
        }
    }

    fn commit_search(&mut self, pattern: &str) {
        let effective: Option<String> = if pattern.is_empty() {
            self.active().editor.last_search().map(str::to_owned)
        } else {
            Some(pattern.to_owned())
        };
        let Some(p) = effective else {
            self.active_mut().editor.set_search_pattern(None);
            return;
        };
        let case_insensitive = self.active().editor.settings().ignore_case
            && !(self.active().editor.settings().smartcase && p.chars().any(|c| c.is_uppercase()));
        let compile_src: std::borrow::Cow<'_, str> = if case_insensitive {
            std::borrow::Cow::Owned(format!("(?i){p}"))
        } else {
            std::borrow::Cow::Borrowed(p.as_str())
        };
        match regex::Regex::new(&compile_src) {
            Ok(re) => {
                self.active_mut().editor.set_search_pattern(Some(re));
                let forward = self.search_dir == SearchDir::Forward;
                if forward {
                    self.active_mut().editor.search_advance_forward(true);
                } else {
                    self.active_mut().editor.search_advance_backward(true);
                }
                self.active_mut().editor.set_last_search(Some(p), forward);
            }
            Err(e) => {
                self.active_mut().editor.set_search_pattern(None);
                self.status_message = Some(format!("E: bad search pattern: {e}"));
            }
        }
    }

    /// Execute an ex command string (without the leading `:`).
    fn dispatch_ex(&mut self, cmd: &str) {
        let canon = ex::canonical_command_name(cmd);
        let cmd: &str = canon.as_ref();
        if cmd == "perf" {
            self.perf_overlay = !self.perf_overlay;
            self.recompute_hits = 0;
            self.recompute_throttled = 0;
            self.recompute_runs = 0;
            self.status_message = Some(if self.perf_overlay {
                "perf overlay: on (counters reset)".into()
            } else {
                "perf overlay: off".into()
            });
            return;
        }
        if let Some(rest) = cmd.strip_prefix("set background=") {
            match rest.trim() {
                "dark" => {
                    self.syntax.set_theme(Arc::new(DotFallbackTheme::dark()));
                    self.active_mut().last_recompute_key = None;
                    self.recompute_and_install();
                    self.status_message = Some("background=dark".into());
                    return;
                }
                "light" => {
                    self.syntax.set_theme(Arc::new(DotFallbackTheme::light()));
                    self.active_mut().last_recompute_key = None;
                    self.recompute_and_install();
                    self.status_message = Some("background=light".into());
                    return;
                }
                other => {
                    self.status_message = Some(format!("E: unknown background value: {other}"));
                    return;
                }
            }
        }

        if cmd == "picker" {
            self.open_picker();
            return;
        }

        if cmd == "bpicker" {
            self.open_buffer_picker();
            return;
        }

        // E1 — `:b [num|name]` — must be matched BEFORE the `bn`/`bp` block.
        if cmd == "b" || cmd.starts_with("b ") {
            let arg = cmd.strip_prefix("b ").map(str::trim).unwrap_or("").trim();
            if arg.is_empty() {
                self.status_message = Some("E94: No matching buffer".into());
            } else if arg.chars().all(|c| c.is_ascii_digit()) {
                let n: usize = arg.parse().unwrap_or(0);
                if n == 0 || n > self.slots.len() {
                    self.status_message = Some(format!("E86: Buffer {n} does not exist"));
                } else {
                    self.switch_to(n - 1);
                }
            } else {
                let arg_lower = arg.to_lowercase();
                let matches: Vec<usize> = self
                    .slots
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| {
                        s.filename
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .map(|n| n.to_lowercase().contains(&arg_lower))
                            .unwrap_or(false)
                    })
                    .map(|(i, _)| i)
                    .collect();
                match matches.len() {
                    0 => {
                        self.status_message = Some(format!("E94: No matching buffer for {arg}"));
                    }
                    1 => {
                        self.switch_to(matches[0]);
                    }
                    _ => {
                        self.status_message = Some(format!("E93: More than one match for {arg}"));
                    }
                }
            }
            return;
        }

        // Multi-buffer commands (Phase C) — `bn`/`bp`/`bd`/`ls` are not
        // in the engine's COMMAND_NAMES table, so canonicalization
        // leaves them as-is. Match raw spellings here.
        match cmd {
            "bn" | "bnext" => {
                self.buffer_next();
                return;
            }
            "bp" | "bN" | "bprev" | "bprevious" | "bNext" => {
                self.buffer_prev();
                return;
            }
            "bd" | "bdelete" => {
                self.buffer_delete(false);
                return;
            }
            "bd!" | "bdelete!" => {
                self.buffer_delete(true);
                return;
            }
            // E2 — `:bfirst` / `:blast`
            "bfirst" | "bf" => {
                self.switch_to(0);
                return;
            }
            "blast" | "bl" => {
                let last = self.slots.len() - 1;
                self.switch_to(last);
                return;
            }
            "ls" | "buffers" | "files" => {
                self.status_message = Some(self.list_buffers());
                return;
            }
            "b#" => {
                self.buffer_alt();
                return;
            }
            // E3 — `:wa` / `:qa` / `:wqa`
            "wa" | "wall" => {
                self.write_all();
                return;
            }
            "qa" | "qall" => {
                self.quit_all(false);
                return;
            }
            "qa!" | "qall!" => {
                self.quit_all(true);
                return;
            }
            "wqa" | "wqall" => {
                self.write_quit_all(false);
                return;
            }
            "wqa!" | "wqall!" => {
                self.write_quit_all(true);
                return;
            }
            _ => {}
        }

        if cmd == "edit" || cmd == "edit!" || cmd.starts_with("edit ") || cmd.starts_with("edit!") {
            let force = cmd.starts_with("edit!");
            let arg = if let Some(rest) = cmd.strip_prefix("edit!") {
                rest.trim()
            } else if let Some(rest) = cmd.strip_prefix("edit ") {
                rest.trim()
            } else {
                ""
            };
            self.do_edit(arg, force);
            return;
        }

        match ex::run(&mut self.slots[self.active].editor, cmd) {
            ExEffect::None => {}
            ExEffect::Ok => {}
            ExEffect::Save => {
                self.do_save(None);
            }
            ExEffect::SaveAs(path) => {
                self.do_save(Some(PathBuf::from(path)));
            }
            ExEffect::Quit { force, save } => {
                if save {
                    self.do_save(None);
                    // Fall through to close-or-quit.
                }
                // E4: multi-slot — close active slot, stay in app.
                if self.slots.len() > 1 {
                    self.buffer_delete(force);
                    return;
                }
                // Last slot: original quit semantics.
                if force || save {
                    self.exit_requested = true;
                } else if self.active().dirty {
                    self.status_message =
                        Some("E37: No write since last change (add ! to override)".into());
                } else {
                    self.exit_requested = true;
                }
            }
            ExEffect::Substituted { count } => {
                // Engine applied the substitution in-place; propagate dirty
                // and fan ContentEdits into the syntax tree.
                if self.slots[self.active].editor.take_dirty() {
                    let elapsed = self.slots[self.active].refresh_dirty_against_saved();
                    self.last_signature_us = elapsed;
                    let buffer_id = self.slots[self.active].buffer_id;
                    if self.slots[self.active].editor.take_content_reset() {
                        self.syntax.reset(buffer_id);
                    }
                    let edits = self.slots[self.active].editor.take_content_edits();
                    if !edits.is_empty() {
                        self.syntax.apply_edits(buffer_id, &edits);
                    }
                    self.recompute_and_install();
                }
                self.status_message = Some(format!("{count} substitution(s)"));
            }
            ExEffect::Info(msg) => {
                self.status_message = Some(msg);
            }
            ExEffect::Error(msg) => {
                self.status_message = Some(format!("E: {msg}"));
            }
            ExEffect::Unknown(c) => {
                self.status_message = Some(format!("E492: Not an editor command: :{c}"));
            }
        }
    }

    /// Write buffer content to `path` (or `self.active().filename` if `path` is `None`).
    fn do_save(&mut self, path: Option<PathBuf>) {
        let idx = self.active;
        self.save_slot(idx, path);
    }

    /// Write slot `idx`'s buffer to `path` (or the slot's own filename if
    /// `path` is `None`). Updates `status_message` on success or failure.
    /// Does NOT change `self.active`.
    fn save_slot(&mut self, idx: usize, path: Option<PathBuf>) {
        if self.slots[idx].editor.is_readonly() {
            self.status_message = Some("E45: 'readonly' option is set (add ! to override)".into());
            return;
        }
        let target = path.or_else(|| self.slots[idx].filename.clone());
        match target {
            None => {
                self.status_message = Some("E32: No file name".into());
            }
            Some(p) => {
                let lines = self.slots[idx].editor.buffer().lines();
                let content = if lines.is_empty() {
                    String::new()
                } else {
                    let mut s = lines.join("\n");
                    s.push('\n');
                    s
                };
                match std::fs::write(&p, &content) {
                    Ok(()) => {
                        let line_count = lines.len();
                        let byte_count = content.len();
                        self.status_message = Some(format!(
                            "\"{}\" {}L, {}B written",
                            p.display(),
                            line_count,
                            byte_count,
                        ));
                        self.slots[idx].filename = Some(p);
                        self.slots[idx].is_new_file = false;
                        self.slots[idx].snapshot_saved();
                        if idx == self.active {
                            self.refresh_git_signs_force();
                        }
                    }
                    Err(e) => {
                        self.status_message = Some(format!("E: {}: {e}", p.display()));
                    }
                }
            }
        }
    }

    /// `:wa` / `:wall` — write all named dirty slots.
    fn write_all(&mut self) {
        let mut written = 0usize;
        let mut skipped = 0usize;
        for i in 0..self.slots.len() {
            if self.slots[i].filename.is_none() {
                skipped += 1;
                continue;
            }
            if !self.slots[i].dirty {
                continue;
            }
            self.save_slot(i, None);
            written += 1;
        }
        self.status_message = Some(format!("{written} buffer(s) written, {skipped} skipped"));
    }

    /// `:qa[!]` — quit all. Blocks when any slot is dirty unless `force`.
    fn quit_all(&mut self, force: bool) {
        if !force && let Some(idx) = self.slots.iter().position(|s| s.dirty) {
            let name = self.slots[idx]
                .filename
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "[No Name]".into());
            self.status_message = Some(format!(
                "E37: No write since last change for buffer \"{name}\" (add ! to override)"
            ));
            return;
        }
        self.exit_requested = true;
    }

    /// `:wqa[!]` — write all named dirty slots then quit.
    fn write_quit_all(&mut self, force: bool) {
        self.write_all();
        self.quit_all(force);
    }

    /// Open or reload a file via `:e [path]` / `:e!`.
    ///
    /// Switch-or-create semantics (Phase C):
    /// - `:e` with no arg → reload current buffer (blocked when dirty
    ///   unless `force`).
    /// - `:e %` → reload current (`%` expands to current filename).
    /// - `:e <path>` where `<path>` matches an open slot → switch to it.
    /// - `:e <path>` for a new path → load the file in a new slot,
    ///   append, and switch active. The previous slot is untouched.
    fn do_edit(&mut self, arg: &str, force: bool) {
        if arg.is_empty() {
            self.reload_current(force);
            return;
        }
        let path_str = if arg.contains('%') {
            let curr = match self.active().filename.as_ref().and_then(|p| p.to_str()) {
                Some(s) => s,
                None => {
                    self.status_message = Some("E499: Empty file name for '%'".into());
                    return;
                }
            };
            arg.replace('%', curr)
        } else {
            arg.to_string()
        };
        let path = PathBuf::from(&path_str);
        let target = canon_for_match(&path);

        // Switch when the path matches an open slot.
        if let Some(idx) = self
            .slots
            .iter()
            .position(|s| s.filename.as_deref().map(canon_for_match) == Some(target.clone()))
        {
            if idx == self.active {
                self.reload_current(force);
                return;
            }
            self.switch_to(idx);
            self.status_message = Some(format!(
                "switched to buffer {}: \"{}\"",
                idx + 1,
                self.active()
                    .filename
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ));
            return;
        }

        // Otherwise create a new slot.
        match self.open_new_slot(path) {
            Ok(idx) => {
                // Track alt-buffer before switching.
                self.prev_active = Some(self.active);
                self.active = idx;
                let line_count = self.active().editor.buffer().line_count() as usize;
                let path_display = self
                    .active()
                    .filename
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.status_message = Some(format!("\"{path_display}\" {line_count}L"));
                self.refresh_git_signs_force();
            }
            Err(msg) => {
                self.status_message = Some(msg);
            }
        }
    }

    /// Reload the active slot from disk (`:e` no-arg / `:e %`).
    fn reload_current(&mut self, force: bool) {
        let path = match self.active().filename.clone() {
            Some(p) => p,
            None => {
                self.status_message = Some("E32: No file name".into());
                return;
            }
        };
        if !force && self.active().dirty {
            self.status_message =
                Some("E37: No write since last change (add ! to override)".into());
            return;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                self.status_message =
                    Some(format!("E484: Can't open file {}: {e}", path.display()));
                return;
            }
        };
        let trimmed = content.strip_suffix('\n').unwrap_or(&content);
        let line_count = trimmed.lines().count();
        let byte_count = content.len();
        self.active_mut().editor.set_content(trimmed);
        self.active_mut().editor.goto_line(1);
        {
            let vp = self.active_mut().editor.host_mut().viewport_mut();
            vp.top_row = 0;
            vp.top_col = 0;
        }
        self.active_mut().is_new_file = false;
        let buffer_id = self.active().buffer_id;
        self.syntax.set_language_for_path(buffer_id, &path);
        self.syntax.reset(buffer_id);
        self.active_mut().last_recompute_key = None;
        self.active_mut()
            .editor
            .install_ratatui_syntax_spans(Vec::new());
        let (vp_top, vp_height) = {
            let vp = self.active().editor.host().viewport();
            (vp.top_row, vp.height as usize)
        };
        if let Some(out) =
            self.syntax
                .preview_render(buffer_id, self.active().editor.buffer(), vp_top, vp_height)
        {
            self.active_mut()
                .editor
                .install_ratatui_syntax_spans(out.spans);
        }
        self.recompute_and_install();
        self.active_mut().snapshot_saved();
        self.refresh_git_signs_force();
        self.status_message = Some(format!(
            "\"{}\" {line_count}L, {byte_count}B",
            path.display()
        ));
    }

    /// Public entry point for loading an extra file from the CLI into a new
    /// slot without switching the active buffer. Used by `main` to handle
    /// `hjkl a.rs b.rs c.rs` — slots 1…N are populated here after `App::new`
    /// opens slot 0.
    pub fn open_extra(&mut self, path: PathBuf) -> Result<(), String> {
        self.open_new_slot(path).map(|_| ())
    }

    /// Allocate a fresh `BufferId` and load `path` into a new slot.
    /// Returns the index of the newly pushed slot (does NOT switch).
    fn open_new_slot(&mut self, path: PathBuf) -> Result<usize, String> {
        let mut buffer = Buffer::new();
        let mut is_new_file = false;
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let content = content.strip_suffix('\n').unwrap_or(&content);
                BufferEdit::replace_all(&mut buffer, content);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                is_new_file = true;
            }
            Err(e) => return Err(format!("E484: Can't open file {}: {e}", path.display())),
        }
        let host = TuiHost::new();
        let mut editor = Editor::new(buffer, host, Options::default());
        if let Ok(size) = crossterm::terminal::size() {
            let vp = editor.host_mut().viewport_mut();
            vp.width = size.0;
            vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
        }
        let buffer_id = self.next_buffer_id;
        self.next_buffer_id += 1;
        self.syntax.set_language_for_path(buffer_id, &path);
        let (vp_top, vp_height) = {
            let vp = editor.host().viewport();
            (vp.top_row, vp.height as usize)
        };
        if let Some(out) = self
            .syntax
            .preview_render(buffer_id, editor.buffer(), vp_top, vp_height)
        {
            editor.install_ratatui_syntax_spans(out.spans);
        }
        self.syntax
            .submit_render(buffer_id, editor.buffer(), vp_top, vp_height);
        let initial_dg = editor.buffer().dirty_gen();
        let (key, signs) = if let Some(out) = self
            .syntax
            .wait_for_initial_result(Duration::from_millis(150))
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
            filename: Some(path),
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
        self.slots.push(slot);
        Ok(self.slots.len() - 1)
    }

    /// Switch active to `idx` and refresh its viewport spans.
    /// Records the previous active index in `prev_active` for alt-buffer.
    pub(crate) fn switch_to(&mut self, idx: usize) {
        if idx != self.active {
            self.prev_active = Some(self.active);
        }
        self.active = idx;
        if let Ok(size) = crossterm::terminal::size() {
            let vp = self.active_mut().editor.host_mut().viewport_mut();
            vp.width = size.0;
            vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
        }
        let buffer_id = self.active().buffer_id;
        let (vp_top, vp_height) = {
            let vp = self.active().editor.host().viewport();
            (vp.top_row, vp.height as usize)
        };
        if let Some(out) =
            self.syntax
                .preview_render(buffer_id, self.active().editor.buffer(), vp_top, vp_height)
        {
            self.active_mut()
                .editor
                .install_ratatui_syntax_spans(out.spans);
        }
        self.active_mut().last_recompute_key = None;
        self.recompute_and_install();
        self.refresh_git_signs_force();
    }

    /// `:bnext` — cycle active forward. No-op when only one slot.
    fn buffer_next(&mut self) {
        if self.slots.len() <= 1 {
            self.status_message = Some("only one buffer open".into());
            return;
        }
        let next = (self.active + 1) % self.slots.len();
        self.switch_to(next);
    }

    /// `:bprev` — cycle active backward. No-op when only one slot.
    fn buffer_prev(&mut self) {
        if self.slots.len() <= 1 {
            self.status_message = Some("only one buffer open".into());
            return;
        }
        let prev = (self.active + self.slots.len() - 1) % self.slots.len();
        self.switch_to(prev);
    }

    /// `<C-^>` / `:b#` — switch to the previously-active buffer slot.
    fn buffer_alt(&mut self) {
        if self.slots.len() <= 1 {
            self.status_message = Some("only one buffer open".into());
            return;
        }
        match self.prev_active {
            Some(i) if i < self.slots.len() => {
                self.switch_to(i);
            }
            _ => {
                self.status_message = Some("no alternate buffer".into());
            }
        }
    }

    /// `:bdelete[!]` — close the active slot. With more than one slot
    /// open the slot is removed; on the last slot the buffer is reset
    /// to an empty unnamed scratch buffer (vim parity for `:bd` on the
    /// only buffer leaving an empty editor instead of quitting).
    fn buffer_delete(&mut self, force: bool) {
        if !force && self.active().dirty {
            self.status_message =
                Some("E89: No write since last change (add ! to override)".into());
            return;
        }
        if self.slots.len() == 1 {
            let old_id = self.active().buffer_id;
            self.syntax.forget(old_id);
            let new_id = self.next_buffer_id;
            self.next_buffer_id += 1;
            let host = TuiHost::new();
            let mut editor = Editor::new(Buffer::new(), host, Options::default());
            if let Ok(size) = crossterm::terminal::size() {
                let vp = editor.host_mut().viewport_mut();
                vp.width = size.0;
                vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
            }
            let _ = editor.take_content_edits();
            let _ = editor.take_content_reset();
            let slot = &mut self.slots[0];
            slot.buffer_id = new_id;
            slot.editor = editor;
            slot.filename = None;
            slot.dirty = false;
            slot.is_new_file = false;
            slot.is_untracked = false;
            slot.diag_signs.clear();
            slot.git_signs.clear();
            slot.last_git_dirty_gen = None;
            slot.last_recompute_key = None;
            slot.saved_hash = 0;
            slot.saved_len = 0;
            slot.snapshot_saved();
            self.status_message = Some("buffer closed (replaced with [No Name])".into());
            return;
        }
        let removed = self.slots.remove(self.active);
        self.syntax.forget(removed.buffer_id);
        if self.active >= self.slots.len() {
            self.active = self.slots.len() - 1;
        }
        let target = self.active;
        self.switch_to(target);
        // Clear alt-buffer pointer after the switch: prev_active may refer
        // to a removed or re-indexed slot. Reset unconditionally.
        self.prev_active = None;
        let name = removed
            .filename
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "[No Name]".into());
        self.status_message = Some(format!("buffer closed: \"{name}\""));
    }

    /// `:ls` / `:buffers` — render the buffer list to a single status
    /// line. Marks: `%` active, `+` modified.
    fn list_buffers(&self) -> String {
        let mut parts = Vec::with_capacity(self.slots.len());
        for (i, slot) in self.slots.iter().enumerate() {
            let active = if i == self.active { '%' } else { ' ' };
            let modf = if slot.dirty { '+' } else { ' ' };
            let name = slot
                .filename
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "[No Name]".into());
            parts.push(format!("{}:{active}{modf} \"{name}\"", i + 1));
        }
        parts.join(" | ")
    }

    /// Submit a new viewport-scoped parse on the syntax worker and install
    /// whatever the worker has produced since the last frame.
    pub fn recompute_and_install(&mut self) {
        const RECOMPUTE_THROTTLE: Duration = Duration::from_millis(100);
        let buffer_id = self.active().buffer_id;
        let (top, height) = {
            let vp = self.active().editor.host().viewport();
            (vp.top_row, vp.height as usize)
        };
        let dg = self.active().editor.buffer().dirty_gen();
        let key = (dg, top, height);

        let prev_dirty_gen = self
            .active()
            .last_recompute_key
            .map(|(prev_dg, _, _)| prev_dg);

        let t_total = Instant::now();
        let mut submitted = false;
        if self.active().last_recompute_key == Some(key) {
            self.recompute_hits = self.recompute_hits.saturating_add(1);
        } else {
            let buffer_changed = self
                .active()
                .last_recompute_key
                .map(|(prev_dg, _, _)| prev_dg != dg)
                .unwrap_or(true);
            let now = Instant::now();
            if buffer_changed
                && now.duration_since(self.active().last_recompute_at) < RECOMPUTE_THROTTLE
            {
                self.recompute_throttled = self.recompute_throttled.saturating_add(1);
            } else {
                self.recompute_runs = self.recompute_runs.saturating_add(1);
                // Split borrow: get a raw pointer to the buffer so `self.syntax`
                // can be borrowed mutably without fighting the borrow checker on
                // `self.slots`. Safety: the buffer lives inside `self.slots[active]`
                // which is not touched inside `submit_render`.
                let submit_result = {
                    let buf = self.slots[self.active].editor.buffer();
                    self.syntax.submit_render(buffer_id, buf, top, height)
                };
                if submit_result.is_some() {
                    submitted = true;
                    self.active_mut().last_recompute_at = Instant::now();
                    self.active_mut().last_recompute_key = Some(key);
                }
            }
        }

        let t_install = Instant::now();
        let drained = if submitted {
            let viewport_only = prev_dirty_gen == Some(dg);
            if viewport_only {
                self.syntax.wait_result(Duration::from_millis(5))
            } else {
                self.syntax.take_result()
            }
        } else {
            self.syntax.take_result()
        };
        if let Some(out) = drained {
            self.active_mut()
                .editor
                .install_ratatui_syntax_spans(out.spans);
            self.active_mut().diag_signs = out.signs;
            self.last_install_us = t_install.elapsed().as_micros();
        } else {
            self.last_install_us = 0;
        }
        self.last_perf = self.syntax.last_perf;

        let t_git = Instant::now();
        self.refresh_git_signs();
        self.last_git_us = t_git.elapsed().as_micros();
        self.last_recompute_us = t_total.elapsed().as_micros();
        let _ = submitted;
    }

    /// Mode label for the status line.
    pub fn mode_label(&self) -> &'static str {
        match self.active().editor.vim_mode() {
            VimMode::Normal => "NORMAL",
            VimMode::Insert => "INSERT",
            VimMode::Visual => "VISUAL",
            VimMode::VisualLine => "VISUAL LINE",
            VimMode::VisualBlock => "VISUAL BLOCK",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_str(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_command_field_key(key(KeyCode::Char(c)));
        }
    }

    // ── Command palette (`:`) tests ─────────────────────────────────────────

    #[test]
    fn palette_open_and_submit_runs_dispatch_and_closes() {
        let mut app = App::new(None, false, None, None).unwrap();
        app.open_command_prompt();
        assert!(app.command_field.is_some());
        type_str(&mut app, "wq");
        assert_eq!(app.command_field.as_ref().unwrap().text(), "wq");
        app.handle_command_field_key(key(KeyCode::Enter));
        assert!(app.command_field.is_none());
        assert!(app.exit_requested);
    }

    #[test]
    fn palette_esc_in_insert_drops_to_normal_then_motions_apply() {
        let mut app = App::new(None, false, None, None).unwrap();
        app.open_command_prompt();
        type_str(&mut app, "abc");
        app.handle_command_field_key(key(KeyCode::Esc));
        assert!(app.command_field.is_some());
        let f = app.command_field.as_ref().unwrap();
        assert_eq!(f.vim_mode(), VimMode::Normal);
        assert_eq!(f.text(), "abc");
        app.handle_command_field_key(key(KeyCode::Char('b')));
        app.handle_command_field_key(key(KeyCode::Char('d')));
        app.handle_command_field_key(key(KeyCode::Char('w')));
        let f = app.command_field.as_ref().unwrap();
        assert_eq!(f.text(), "");
        app.handle_command_field_key(key(KeyCode::Esc));
        assert!(app.command_field.is_none());
    }

    #[test]
    fn palette_ctrl_c_cancels_without_quitting_app() {
        let mut app = App::new(None, false, None, None).unwrap();
        app.open_command_prompt();
        type_str(&mut app, "wq");
        let cc = ctrl_key('c');
        if app.command_field.is_some()
            && cc.code == KeyCode::Char('c')
            && cc.modifiers.contains(KeyModifiers::CONTROL)
        {
            app.command_field = None;
        }
        assert!(app.command_field.is_none());
        assert!(!app.exit_requested);
    }

    // ── Search prompt (`/` `?`) tests ───────────────────────────────────────

    fn type_search(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_search_field_key(key(KeyCode::Char(c)));
        }
    }

    fn seed_buffer(app: &mut App, content: &str) {
        BufferEdit::replace_all(app.active_mut().editor.buffer_mut(), content);
    }

    #[test]
    fn search_open_and_type_drives_live_preview() {
        let mut app = App::new(None, false, None, None).unwrap();
        seed_buffer(&mut app, "foo bar foo baz");
        app.open_search_prompt(SearchDir::Forward);
        assert!(app.search_field.is_some());
        type_search(&mut app, "foo");
        assert_eq!(app.search_field.as_ref().unwrap().text(), "foo");
        assert!(app.active().editor.search_state().pattern.is_some());
    }

    #[test]
    fn search_more_typing_updates_pattern() {
        let mut app = App::new(None, false, None, None).unwrap();
        seed_buffer(&mut app, "foobar foozle");
        app.open_search_prompt(SearchDir::Forward);
        type_search(&mut app, "foo");
        let p1 = app
            .active()
            .editor
            .search_state()
            .pattern
            .as_ref()
            .unwrap()
            .as_str()
            .to_string();
        type_search(&mut app, "z");
        let p2 = app
            .active()
            .editor
            .search_state()
            .pattern
            .as_ref()
            .unwrap()
            .as_str()
            .to_string();
        assert_ne!(p1, p2, "pattern must update on further typing");
    }

    #[test]
    fn search_motion_in_normal_edits_prompt_and_updates_highlight() {
        let mut app = App::new(None, false, None, None).unwrap();
        seed_buffer(&mut app, "alpha beta\ngamma");
        app.open_search_prompt(SearchDir::Forward);
        type_search(&mut app, "alpha beta");
        app.handle_search_field_key(key(KeyCode::Esc));
        assert_eq!(
            app.search_field.as_ref().unwrap().vim_mode(),
            VimMode::Normal
        );
        app.handle_search_field_key(key(KeyCode::Char('b')));
        app.handle_search_field_key(key(KeyCode::Char('d')));
        app.handle_search_field_key(key(KeyCode::Char('b')));
        let new_text = app.search_field.as_ref().unwrap().text();
        assert!(
            new_text.len() < "alpha beta".len(),
            "prompt text shrank: {new_text:?}"
        );
    }

    #[test]
    fn search_enter_commits_and_advances_cursor() {
        let mut app = App::new(None, false, None, None).unwrap();
        seed_buffer(&mut app, "alpha\nbeta\nfoo here\ndone");
        app.open_search_prompt(SearchDir::Forward);
        type_search(&mut app, "foo");
        app.handle_search_field_key(key(KeyCode::Enter));
        assert!(app.search_field.is_none());
        let (row, col) = app.active().editor.cursor();
        assert_eq!(row, 2);
        assert_eq!(col, 0);
        assert_eq!(app.active().editor.last_search(), Some("foo"));
        assert!(app.active().editor.last_search_forward());
    }

    #[test]
    fn search_esc_twice_cancels_and_clears_when_no_prior_search() {
        let mut app = App::new(None, false, None, None).unwrap();
        seed_buffer(&mut app, "abc def");
        app.open_search_prompt(SearchDir::Forward);
        type_search(&mut app, "abc");
        app.handle_search_field_key(key(KeyCode::Esc));
        assert!(app.search_field.is_some());
        app.handle_search_field_key(key(KeyCode::Esc));
        assert!(app.search_field.is_none());
        assert!(app.active().editor.search_state().pattern.is_none());
    }

    #[test]
    fn search_backward_prompt_uses_question_dir() {
        let mut app = App::new(None, false, None, None).unwrap();
        seed_buffer(&mut app, "foo here\nbar there\nfoo again");
        app.active_mut().editor.goto_line(3);
        app.open_search_prompt(SearchDir::Backward);
        type_search(&mut app, "foo");
        app.handle_search_field_key(key(KeyCode::Enter));
        let (row, _) = app.active().editor.cursor();
        assert_eq!(row, 0);
        assert!(!app.active().editor.last_search_forward());
    }

    // ── App::new tests ──────────────────────────────────────────────────────

    #[test]
    fn app_new_no_file() {
        let app = App::new(None, false, None, None).unwrap();
        assert!(!app.active().dirty);
        assert!(!app.active().is_new_file);
        assert!(app.active().filename.is_none());
        assert!(!app.active().editor.is_readonly());
    }

    #[test]
    fn app_new_readonly_flag() {
        let app = App::new(None, true, None, None).unwrap();
        assert!(app.active().editor.is_readonly());
    }

    #[test]
    fn app_new_not_found_sets_is_new_file() {
        let path = std::path::PathBuf::from("/tmp/hjkl_phase5_nonexistent_abc123.txt");
        let _ = std::fs::remove_file(&path);
        let app = App::new(Some(path), false, None, None).unwrap();
        assert!(app.active().is_new_file);
        assert!(!app.active().dirty);
    }

    #[test]
    fn app_new_goto_line_clamps() {
        let app = App::new(None, false, Some(999), None).unwrap();
        let (row, _col) = app.active().editor.cursor();
        assert_eq!(row, 0);
    }

    #[test]
    fn do_save_readonly_blocked() {
        let mut app = App::new(None, true, None, None).unwrap();
        app.active_mut().filename = Some(std::path::PathBuf::from("/tmp/hjkl_phase5_ro_test.txt"));
        app.do_save(None);
        let msg = app.status_message.unwrap_or_default();
        assert!(
            msg.contains("E45"),
            "expected E45 readonly error, got: {msg}"
        );
    }

    #[test]
    fn do_save_no_filename_e32() {
        let mut app = App::new(None, false, None, None).unwrap();
        app.do_save(None);
        let msg = app.status_message.unwrap_or_default();
        assert!(msg.contains("E32"), "expected E32, got: {msg}");
    }

    // ── :e tests ────────────────────────────────────────────────────────────

    #[test]
    fn edit_percent_reloads_current_file() {
        let path = std::env::temp_dir().join("hjkl_edit_percent_reload.txt");
        std::fs::write(&path, "first\nsecond\n").unwrap();
        let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
        app.dispatch_ex("e %");
        let lines = app.active().editor.buffer().lines();
        assert_eq!(lines, vec!["alpha", "beta", "gamma"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn edit_no_arg_reloads_current_file() {
        let path = std::env::temp_dir().join("hjkl_edit_noarg_reload.txt");
        std::fs::write(&path, "v1\n").unwrap();
        let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
        std::fs::write(&path, "v2\n").unwrap();
        app.dispatch_ex("e");
        assert_eq!(app.active().editor.buffer().lines(), vec!["v2".to_string()]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn edit_blocks_dirty_buffer_without_force() {
        let path = std::env::temp_dir().join("hjkl_edit_dirty_block.txt");
        std::fs::write(&path, "orig\n").unwrap();
        let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
        app.active_mut().dirty = true;
        app.dispatch_ex("e %");
        let msg = app.status_message.unwrap_or_default();
        assert!(msg.contains("E37"), "expected E37, got: {msg}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn edit_force_reloads_dirty_buffer() {
        let path = std::env::temp_dir().join("hjkl_edit_force.txt");
        std::fs::write(&path, "disk\n").unwrap();
        let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
        app.active_mut().dirty = true;
        app.dispatch_ex("e!");
        assert_eq!(
            app.active().editor.buffer().lines(),
            vec!["disk".to_string()]
        );
        assert!(!app.active().dirty);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn undo_to_saved_state_clears_dirty() {
        let path = std::env::temp_dir().join("hjkl_undo_clears_dirty.txt");
        std::fs::write(&path, "alpha\nbravo\n").unwrap();
        let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
        assert!(!app.active().dirty);
        app.active_mut().editor.handle_key(key(KeyCode::Char('i')));
        app.active_mut().editor.handle_key(key(KeyCode::Char('X')));
        if app.active_mut().editor.take_dirty() {
            app.active_mut().refresh_dirty_against_saved();
        }
        assert!(app.active().dirty, "edit should mark dirty");
        app.active_mut().editor.handle_key(key(KeyCode::Esc));
        app.active_mut().editor.handle_key(key(KeyCode::Char('u')));
        if app.active_mut().editor.take_dirty() {
            app.active_mut().refresh_dirty_against_saved();
        }
        assert!(
            !app.active().dirty,
            "undo to saved state should clear dirty"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn esc_on_empty_command_prompt_dismisses() {
        let mut app = App::new(None, false, None, None).unwrap();
        app.open_command_prompt();
        assert!(app.command_field.is_some());
        app.handle_command_field_key(key(KeyCode::Esc));
        assert!(
            app.command_field.is_none(),
            "empty : prompt should close on Esc"
        );
    }

    #[test]
    fn esc_on_nonempty_command_drops_to_normal_then_closes() {
        let mut app = App::new(None, false, None, None).unwrap();
        app.open_command_prompt();
        app.handle_command_field_key(key(KeyCode::Char('w')));
        app.handle_command_field_key(key(KeyCode::Esc));
        assert!(app.command_field.is_some());
        assert_eq!(
            app.command_field.as_ref().unwrap().vim_mode(),
            VimMode::Normal
        );
        app.handle_command_field_key(key(KeyCode::Esc));
        assert!(app.command_field.is_none());
    }

    #[test]
    fn esc_on_empty_search_prompt_dismisses() {
        let mut app = App::new(None, false, None, None).unwrap();
        app.open_search_prompt(SearchDir::Forward);
        assert!(app.search_field.is_some());
        app.handle_search_field_key(key(KeyCode::Esc));
        assert!(
            app.search_field.is_none(),
            "empty / prompt should close on Esc"
        );
    }

    #[test]
    fn edit_no_arg_no_filename_e32() {
        let mut app = App::new(None, false, None, None).unwrap();
        app.dispatch_ex("e");
        let msg = app.status_message.unwrap_or_default();
        assert!(msg.contains("E32"), "expected E32, got: {msg}");
    }

    // ── Phase C: multi-buffer tests ─────────────────────────────────────────

    #[test]
    fn edit_new_path_appends_slot_and_switches() {
        let path_a = std::env::temp_dir().join("hjkl_phc_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_phc_b.txt");
        std::fs::write(&path_a, "alpha\n").unwrap();
        std::fs::write(&path_b, "beta\n").unwrap();
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        assert_eq!(app.slots.len(), 1);
        app.dispatch_ex(&format!("e {}", path_b.display()));
        assert_eq!(app.slots.len(), 2);
        assert_eq!(app.active, 1);
        assert_eq!(
            app.active().editor.buffer().lines(),
            vec!["beta".to_string()]
        );
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    #[test]
    fn edit_existing_path_switches_to_open_slot() {
        let path_a = std::env::temp_dir().join("hjkl_phc_switch_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_phc_switch_b.txt");
        std::fs::write(&path_a, "alpha\n").unwrap();
        std::fs::write(&path_b, "beta\n").unwrap();
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_b.display()));
        assert_eq!(app.active, 1);
        // Re-open path_a → switch back, no third slot.
        app.dispatch_ex(&format!("e {}", path_a.display()));
        assert_eq!(app.slots.len(), 2);
        assert_eq!(app.active, 0);
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    #[test]
    fn edit_other_open_path_does_not_block_on_dirty() {
        let path_a = std::env::temp_dir().join("hjkl_phc_dirty_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_phc_dirty_b.txt");
        std::fs::write(&path_a, "a\n").unwrap();
        std::fs::write(&path_b, "b\n").unwrap();
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.active_mut().dirty = true;
        // Switching to a *different* file must not be gated on the
        // current slot's dirty flag — the slot isn't being destroyed.
        app.dispatch_ex(&format!("e {}", path_b.display()));
        assert_eq!(app.active, 1);
        assert!(app.slots[0].dirty, "slot 0 should remain dirty");
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    #[test]
    fn bnext_bprev_cycle_active() {
        let path_a = std::env::temp_dir().join("hjkl_phc_cycle_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_phc_cycle_b.txt");
        let path_c = std::env::temp_dir().join("hjkl_phc_cycle_c.txt");
        for p in [&path_a, &path_b, &path_c] {
            std::fs::write(p, "x\n").unwrap();
        }
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_b.display()));
        app.dispatch_ex(&format!("e {}", path_c.display()));
        assert_eq!(app.active, 2);
        app.dispatch_ex("bn");
        assert_eq!(app.active, 0, "wrap forward to 0");
        app.dispatch_ex("bn");
        assert_eq!(app.active, 1);
        app.dispatch_ex("bp");
        assert_eq!(app.active, 0);
        app.dispatch_ex("bp");
        assert_eq!(app.active, 2, "wrap backward to last");
        for p in [&path_a, &path_b, &path_c] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn bnext_no_op_with_single_slot() {
        let mut app = App::new(None, false, None, None).unwrap();
        app.dispatch_ex("bn");
        assert_eq!(app.active, 0);
        assert_eq!(app.slots.len(), 1);
    }

    #[test]
    fn bdelete_blocks_dirty_without_force() {
        let path_a = std::env::temp_dir().join("hjkl_phc_bd_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_phc_bd_b.txt");
        std::fs::write(&path_a, "a\n").unwrap();
        std::fs::write(&path_b, "b\n").unwrap();
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_b.display()));
        app.active_mut().dirty = true;
        app.dispatch_ex("bd");
        let msg = app.status_message.clone().unwrap_or_default();
        assert!(msg.contains("E89"), "expected E89, got: {msg}");
        assert_eq!(app.slots.len(), 2);
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    #[test]
    fn bdelete_force_removes_dirty_slot() {
        let path_a = std::env::temp_dir().join("hjkl_phc_bdforce_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_phc_bdforce_b.txt");
        std::fs::write(&path_a, "a\n").unwrap();
        std::fs::write(&path_b, "b\n").unwrap();
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_b.display()));
        app.active_mut().dirty = true;
        app.dispatch_ex("bd!");
        assert_eq!(app.slots.len(), 1);
        assert_eq!(app.active, 0);
        assert_eq!(app.active().editor.buffer().lines(), vec!["a".to_string()]);
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    #[test]
    fn bdelete_on_last_slot_resets_to_no_name() {
        let path = std::env::temp_dir().join("hjkl_phc_bd_last.txt");
        std::fs::write(&path, "content\n").unwrap();
        let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
        app.dispatch_ex("bd");
        assert_eq!(app.slots.len(), 1);
        assert!(app.active().filename.is_none());
        let lines = app.active().editor.buffer().lines();
        assert!(
            lines.is_empty() || (lines.len() == 1 && lines[0].is_empty()),
            "expected empty scratch buffer, got: {lines:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── Alt-buffer (D2) tests ───────────────────────────────────────────────

    #[test]
    fn buffer_alt_swaps_with_prev_active() {
        let path_a = std::env::temp_dir().join("hjkl_d2_alt_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_d2_alt_b.txt");
        let path_c = std::env::temp_dir().join("hjkl_d2_alt_c.txt");
        for p in [&path_a, &path_b, &path_c] {
            std::fs::write(p, "x\n").unwrap();
        }
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_b.display())); // active=1, prev=0
        app.dispatch_ex(&format!("e {}", path_c.display())); // active=2, prev=1
        assert_eq!(app.active, 2);
        assert_eq!(app.prev_active, Some(1));

        // First alt: go back to 1, prev becomes 2.
        app.buffer_alt();
        assert_eq!(app.active, 1);
        assert_eq!(app.prev_active, Some(2));

        // Second alt: go back to 2.
        app.buffer_alt();
        assert_eq!(app.active, 2);

        for p in [&path_a, &path_b, &path_c] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn buffer_alt_with_single_slot_no_op_with_message() {
        let mut app = App::new(None, false, None, None).unwrap();
        assert_eq!(app.slots.len(), 1);
        app.buffer_alt();
        assert_eq!(app.active, 0);
        let msg = app.status_message.clone().unwrap_or_default();
        assert!(
            msg.contains("only one buffer"),
            "expected 'only one buffer' message, got: {msg}"
        );
    }

    #[test]
    fn bd_clears_prev_active() {
        let path_a = std::env::temp_dir().join("hjkl_d2_bd_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_d2_bd_b.txt");
        std::fs::write(&path_a, "a\n").unwrap();
        std::fs::write(&path_b, "b\n").unwrap();
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_b.display())); // active=1, prev=0
        assert_eq!(app.prev_active, Some(0));
        // Force-close the active slot (b.txt).
        app.dispatch_ex("bd!");
        // prev_active must be reset so the stale index is gone.
        assert!(
            app.prev_active.is_none(),
            "prev_active should be None after bd!"
        );
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    // ── Buffer picker (D4) source tests ────────────────────────────────────

    #[test]
    fn buffer_source_new_produces_n_entries() {
        let path_a = std::env::temp_dir().join("hjkl_d4_src_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_d4_src_b.txt");
        std::fs::write(&path_a, "a\n").unwrap();
        std::fs::write(&path_b, "b\n").unwrap();
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_b.display()));
        assert_eq!(app.slots.len(), 2);

        let source = crate::picker::BufferSource::new(
            &app.slots,
            |s| {
                s.filename
                    .as_ref()
                    .and_then(|p| p.to_str())
                    .unwrap_or("[No Name]")
                    .to_owned()
            },
            |s| s.dirty,
        );
        // Build a Picker from the source — it calls enumerate internally.
        let mut picker = crate::picker::BufferPicker::new(source);
        picker.refresh();
        assert_eq!(picker.total(), 2, "expected 2 entries");
        assert!(picker.scan_done(), "scan_done must be set");
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    #[test]
    fn buffer_source_select_returns_switch_buffer() {
        use crate::picker::{BufferSource, PickerAction, PickerSource};
        let path = std::env::temp_dir().join("hjkl_d4_sel.txt");
        std::fs::write(&path, "x\n").unwrap();
        let app = App::new(Some(path.clone()), false, None, None).unwrap();
        let source = BufferSource::new(
            &app.slots,
            |s| {
                s.filename
                    .as_ref()
                    .and_then(|p| p.to_str())
                    .unwrap_or("[No Name]")
                    .to_owned()
            },
            |s| s.dirty,
        );
        let entry = crate::picker::BufferEntry {
            idx: 0,
            name: "foo".into(),
            dirty: false,
        };
        match source.select(&entry) {
            PickerAction::SwitchBuffer(i) => assert_eq!(i, 0),
            _ => panic!("expected SwitchBuffer(0)"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_extra_adds_slot_and_leaves_active_zero() {
        let path_a = std::env::temp_dir().join("hjkl_open_extra_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_open_extra_b.txt");
        std::fs::write(&path_a, "first\n").unwrap();
        std::fs::write(&path_b, "second\n").unwrap();
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        assert_eq!(app.slots.len(), 1);
        assert_eq!(app.active, 0);
        app.open_extra(path_b.clone()).unwrap();
        assert_eq!(app.slots.len(), 2, "extra slot should have been added");
        assert_eq!(app.active, 0, "active must stay at 0 after open_extra");
        assert_eq!(
            app.slots[0].editor.buffer().lines(),
            vec!["first".to_string()]
        );
        assert_eq!(
            app.slots[1].editor.buffer().lines(),
            vec!["second".to_string()]
        );
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    #[test]
    fn ls_lists_all_buffers_with_active_marker() {
        let path_a = std::env::temp_dir().join("hjkl_phc_ls_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_phc_ls_b.txt");
        std::fs::write(&path_a, "a\n").unwrap();
        std::fs::write(&path_b, "b\n").unwrap();
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_b.display()));
        app.dispatch_ex("ls");
        let msg = app.status_message.clone().unwrap_or_default();
        assert!(msg.contains("1: "), "expected slot 1 entry, got: {msg}");
        assert!(
            msg.contains("2:%"),
            "active marker missing on slot 2: {msg}"
        );
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    // ── Phase E: multi-buffer ex-command parity tests ──────────────────────

    #[test]
    fn b_num_switches_by_index() {
        let path_a = std::env::temp_dir().join("hjkl_phe_bnum_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_phe_bnum_b.txt");
        let path_c = std::env::temp_dir().join("hjkl_phe_bnum_c.txt");
        for p in [&path_a, &path_b, &path_c] {
            std::fs::write(p, "x\n").unwrap();
        }
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_b.display()));
        app.dispatch_ex(&format!("e {}", path_c.display()));
        assert_eq!(app.slots.len(), 3);
        app.dispatch_ex("b 2");
        assert_eq!(app.active, 1, "`:b 2` should switch to index 1");
        for p in [&path_a, &path_b, &path_c] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn b_num_out_of_range_errors() {
        let mut app = App::new(None, false, None, None).unwrap();
        assert_eq!(app.slots.len(), 1);
        app.dispatch_ex("b 5");
        let msg = app.status_message.clone().unwrap_or_default();
        assert!(msg.contains("E86"), "expected E86, got: {msg}");
    }

    #[test]
    fn b_name_substring_switches() {
        let path_foo = std::env::temp_dir().join("hjkl_phe_bname_foo.txt");
        let path_bar = std::env::temp_dir().join("hjkl_phe_bname_bar.txt");
        std::fs::write(&path_foo, "foo\n").unwrap();
        std::fs::write(&path_bar, "bar\n").unwrap();
        let mut app = App::new(Some(path_foo.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_bar.display()));
        assert_eq!(app.active, 1);
        // Switch to the foo slot by substring
        app.dispatch_ex("b foo");
        assert_eq!(app.active, 0, "`:b foo` should switch to foo's slot");
        let _ = std::fs::remove_file(&path_foo);
        let _ = std::fs::remove_file(&path_bar);
    }

    #[test]
    fn b_name_ambiguous_errors() {
        let path_a = std::env::temp_dir().join("hjkl_phe_bamb_foo_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_phe_bamb_foo_b.txt");
        std::fs::write(&path_a, "a\n").unwrap();
        std::fs::write(&path_b, "b\n").unwrap();
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_b.display()));
        // Both filenames contain "foo" — ambiguous
        app.dispatch_ex("b foo");
        let msg = app.status_message.clone().unwrap_or_default();
        assert!(
            msg.contains("E93"),
            "expected E93 ambiguous error, got: {msg}"
        );
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    #[test]
    fn bfirst_blast_jump_to_ends() {
        let path_a = std::env::temp_dir().join("hjkl_phe_bfl_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_phe_bfl_b.txt");
        let path_c = std::env::temp_dir().join("hjkl_phe_bfl_c.txt");
        for p in [&path_a, &path_b, &path_c] {
            std::fs::write(p, "x\n").unwrap();
        }
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_b.display()));
        app.dispatch_ex(&format!("e {}", path_c.display()));
        assert_eq!(app.slots.len(), 3);
        // Start in middle
        app.dispatch_ex("b 2");
        assert_eq!(app.active, 1);
        app.dispatch_ex("bfirst");
        assert_eq!(app.active, 0, "`:bfirst` should go to slot 0");
        app.dispatch_ex("blast");
        assert_eq!(app.active, 2, "`:blast` should go to last slot");
        for p in [&path_a, &path_b, &path_c] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn wa_writes_dirty_named_slots() {
        let path_a = std::env::temp_dir().join("hjkl_phe_wa_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_phe_wa_b.txt");
        std::fs::write(&path_a, "original a\n").unwrap();
        std::fs::write(&path_b, "original b\n").unwrap();
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_b.display()));
        // Mark both slots dirty with new content
        app.slots[0].dirty = true;
        BufferEdit::replace_all(app.slots[0].editor.buffer_mut(), "edited a");
        app.slots[1].dirty = true;
        BufferEdit::replace_all(app.slots[1].editor.buffer_mut(), "edited b");
        app.dispatch_ex("wa");
        assert!(!app.slots[0].dirty, "slot 0 should be clean after :wa");
        assert!(!app.slots[1].dirty, "slot 1 should be clean after :wa");
        let contents_a = std::fs::read_to_string(&path_a).unwrap_or_default();
        let contents_b = std::fs::read_to_string(&path_b).unwrap_or_default();
        assert!(
            contents_a.contains("edited a"),
            "file a should contain edited content, got: {contents_a}"
        );
        assert!(
            contents_b.contains("edited b"),
            "file b should contain edited content, got: {contents_b}"
        );
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    #[test]
    fn qa_blocks_when_any_slot_dirty() {
        let path_a = std::env::temp_dir().join("hjkl_phe_qa_dirty_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_phe_qa_dirty_b.txt");
        std::fs::write(&path_a, "a\n").unwrap();
        std::fs::write(&path_b, "b\n").unwrap();
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_b.display()));
        app.slots[0].dirty = true;
        app.dispatch_ex("qa");
        assert!(
            !app.exit_requested,
            ":qa should not exit when dirty slot exists"
        );
        let msg = app.status_message.clone().unwrap_or_default();
        assert!(msg.contains("E37"), "expected E37, got: {msg}");
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    #[test]
    fn qa_force_exits_with_dirty() {
        let path_a = std::env::temp_dir().join("hjkl_phe_qa_force_a.txt");
        std::fs::write(&path_a, "a\n").unwrap();
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.slots[0].dirty = true;
        app.dispatch_ex("qa!");
        assert!(app.exit_requested, ":qa! should exit even when dirty");
        let _ = std::fs::remove_file(&path_a);
    }

    #[test]
    fn q_on_multi_slot_closes_slot_not_app() {
        let path_a = std::env::temp_dir().join("hjkl_phe_q_multi_a.txt");
        let path_b = std::env::temp_dir().join("hjkl_phe_q_multi_b.txt");
        std::fs::write(&path_a, "a\n").unwrap();
        std::fs::write(&path_b, "b\n").unwrap();
        let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
        app.dispatch_ex(&format!("e {}", path_b.display()));
        assert_eq!(app.slots.len(), 2);
        app.dispatch_ex("q!");
        assert_eq!(
            app.slots.len(),
            1,
            "`:q!` with 2 slots should close active slot"
        );
        assert!(
            !app.exit_requested,
            "app should remain open after closing one slot"
        );
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    #[test]
    fn q_on_last_slot_quits_app() {
        let mut app = App::new(None, false, None, None).unwrap();
        assert_eq!(app.slots.len(), 1);
        assert!(!app.active().dirty);
        app.dispatch_ex("q");
        assert!(app.exit_requested, "`:q` on clean last slot should exit");
    }
}
