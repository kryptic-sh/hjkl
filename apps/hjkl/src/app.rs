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
    /// Active file picker overlay.
    pub picker: Option<crate::picker::FilePicker>,
    /// `true` after the user pressed `<Space>` in normal mode and we're
    /// waiting for the next key to resolve the leader sequence.
    pub pending_leader: bool,
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
            exit_requested: false,
            status_message: None,
            command_field: None,
            search_field: None,
            picker: None,
            pending_leader: false,
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
                        if key.modifiers == KeyModifiers::NONE
                            && (key.code == KeyCode::Char(' ') || key.code == KeyCode::Char('f'))
                        {
                            self.open_picker();
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
        self.picker = Some(crate::picker::FilePicker::new(source));
        self.pending_leader = false;
    }

    fn handle_picker_key(&mut self, key: crossterm::event::KeyEvent) {
        let picker = match self.picker.as_mut() {
            Some(p) => p,
            None => return,
        };
        match picker.handle_key(key) {
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
                    self.exit_requested = true;
                } else if force {
                    self.exit_requested = true;
                } else {
                    if self.active().dirty {
                        self.status_message =
                            Some("E37: No write since last change (add ! to override)".into());
                    } else {
                        self.exit_requested = true;
                    }
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
        if self.active().editor.is_readonly() {
            self.status_message = Some("E45: 'readonly' option is set (add ! to override)".into());
            return;
        }
        let target = path.or_else(|| self.active().filename.clone());
        match target {
            None => {
                self.status_message = Some("E32: No file name".into());
            }
            Some(p) => {
                let lines = self.active().editor.buffer().lines();
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
                        self.active_mut().filename = Some(p);
                        self.active_mut().is_new_file = false;
                        self.active_mut().snapshot_saved();
                        self.refresh_git_signs_force();
                    }
                    Err(e) => {
                        self.status_message = Some(format!("E: {}: {e}", p.display()));
                    }
                }
            }
        }
    }

    /// Open or reload a file via `:e [path]` / `:e!`.
    fn do_edit(&mut self, arg: &str, force: bool) {
        let path_str = if arg.is_empty() {
            match &self.active().filename {
                Some(p) => p.to_string_lossy().into_owned(),
                None => {
                    self.status_message = Some("E32: No file name".into());
                    return;
                }
            }
        } else if arg.contains('%') {
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

        if !force && self.active().dirty {
            self.status_message =
                Some("E37: No write since last change (add ! to override)".into());
            return;
        }

        let path = PathBuf::from(&path_str);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                self.status_message = Some(format!("E484: Can't open file {path_str}: {e}"));
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
        self.active_mut().filename = Some(path.clone());
        self.active_mut().is_new_file = false;
        let buffer_id = self.active().buffer_id;
        self.syntax.set_language_for_path(buffer_id, &path);
        self.syntax.reset(buffer_id);
        self.active_mut().last_recompute_key = None;
        // Wipe the previous file's spans up-front.
        self.active_mut()
            .editor
            .install_ratatui_syntax_spans(Vec::new());
        // Synchronous viewport-only preview.
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
        self.status_message = Some(format!("\"{path_str}\" {line_count}L, {byte_count}B"));
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
}
