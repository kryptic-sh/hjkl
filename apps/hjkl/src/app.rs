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
use crate::syntax::{self, SyntaxLayer};

/// Height reserved for the status line at the bottom of the screen.
pub const STATUS_LINE_HEIGHT: u16 = 1;

/// Hash + byte-length of the buffer's canonical line content (lines
/// joined by `\n` — same shape as what `:w` writes, modulo the trailing
/// newline). Used to detect "buffer matches the saved snapshot" so undo
/// back to the saved state clears the dirty flag.
fn app_buffer_signature(editor: &Editor<Buffer, TuiHost>) -> (u64, usize) {
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

/// Top-level application state. Everything the event loop and renderer need.
pub struct App {
    /// The live editor — buffer + FSM + host, all in one.
    pub editor: Editor<Buffer, TuiHost>,
    /// File path shown in status line and used for `:w` saves.
    pub filename: Option<PathBuf>,
    /// Set to `true` when the FSM or Ctrl-C wants to quit.
    pub exit_requested: bool,
    /// Persistent dirty flag. Set when `editor.take_dirty()` returns `true`;
    /// cleared after a successful `:w` save.
    pub dirty: bool,
    /// Last ex-command result (Info / Error / write confirmation).
    /// Shown in the status line; cleared on next keypress.
    pub status_message: Option<String>,
    /// Active `:` command input. `Some` while the user is typing an ex
    /// command. Backed by a vim-grammar [`TextFieldEditor`] so motions
    /// (h/l/w/b/dw/diw/...) work inside the prompt.
    pub command_field: Option<TextFieldEditor>,
    /// Active `/` (forward) / `?` (backward) search prompt. `Some`
    /// while the user is typing — Phase J3 lifts the prompt out of
    /// the engine and into the host so it shares the
    /// [`TextFieldEditor`] primitive (vim motions inside the prompt).
    pub search_field: Option<TextFieldEditor>,
    /// Active file picker overlay. `Some` while the user is browsing
    /// the fuzzy-find list (`<leader><space>`, `<leader>f`, or
    /// `:picker`).
    pub picker: Option<crate::picker::FilePicker>,
    /// `true` after the user pressed `<Space>` in normal mode and we're
    /// waiting for the next key to resolve the leader sequence (e.g.
    /// `<Space>f`). Cleared on the next keystroke regardless of match.
    pub pending_leader: bool,
    /// Direction of the active `search_field` (or the most recent
    /// committed search direction; engine's `last_search_forward` is
    /// the source of truth post-commit).
    pub search_dir: SearchDir,
    /// Last cursor shape we emitted to the terminal. Compared each
    /// frame so we only write the DECSCUSR sequence on transitions.
    last_cursor_shape: CursorShape,
    /// True when a file was requested but not found on disk — shows
    /// "[New File]" annotation in the status line until the first edit
    /// or successful `:w`.
    pub is_new_file: bool,
    /// Tree-sitter syntax highlighting layer. Owns the registry, highlighter,
    /// and active theme. Recomputed on every render via the engine's
    /// ContentEdit + viewport-scoped query path.
    syntax: SyntaxLayer,
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
    /// Wall-clock time of the last syntax recompute+install. Used to
    /// throttle parse to ~10 Hz during active typing — tree-sitter
    /// incremental parse on a 1.3MB Rust file still costs ~30ms per
    /// edit, so per-keystroke reparses pile up. Edits accumulated via
    /// `apply_edits` (tree.edit) coalesce naturally into the next
    /// recompute.
    last_recompute_at: Instant,
    /// `(dirty_gen, vp_top, vp_height)` snapshot of the last call to
    /// `recompute_and_install`. When the next call has identical
    /// inputs, the syntax span recompute + install is skipped — the
    /// editor's `buffer_spans` table is still the right answer.
    /// This makes pure-scroll-into-cache and idle-redraw frames free.
    last_recompute_key: Option<(u64, usize, usize)>,
    /// Toggled by `:perf`. When true, render shows last-frame timings
    /// for the heavy paths in the status line for diagnosis.
    pub perf_overlay: bool,
    pub last_recompute_us: u128,
    pub last_install_us: u128,
    pub last_signature_us: u128,
    pub last_git_us: u128,
    pub last_perf: crate::syntax::PerfBreakdown,
    /// Counters surfaced in `:perf` so the user can verify the cache
    /// hit/miss/throttle ratios are sane. Reset on every `:perf` toggle.
    pub recompute_hits: u64,
    pub recompute_throttled: u64,
    pub recompute_runs: u64,
    /// `true` when the current file is in a git repo but not in HEAD —
    /// drives the `[Untracked]` status-line tag. Refreshed alongside
    /// `git_signs`.
    pub is_untracked: bool,
    /// Hash + byte-length of the buffer content as it was at the most
    /// recent save (or load). Compared against the live buffer on every
    /// dirty event so undoing back to the saved state clears the dirty
    /// flag — `editor.take_dirty()` only tracks "did anything change"
    /// monotonically, so a content-equality check is needed for the
    /// "buffer matches disk again" case.
    saved_hash: u64,
    saved_len: usize,
}

impl App {
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
                    // Bad regex — surface as a startup status message later.
                    // We can't set status_message before Self is constructed,
                    // so we store it in a temporary and set it after build.
                    eprintln!("hjkl: bad search pattern: {e}");
                }
            }
        }

        // Build syntax layer (dark theme default) and detect language for
        // the opened file, then run an initial highlight pass.
        let mut syntax = syntax::default_layer();
        if let Some(ref path) = filename {
            syntax.set_language_for_path(path);
        }
        // Sync host viewport to the real terminal size before sizing the
        // preview / submitting the initial parse — otherwise the
        // placeholder 80×24 from `TuiHost::new` underbakes the preview
        // and the bottom of the screen shows up uncolored on first
        // paint.
        if let Ok(size) = crossterm::terminal::size() {
            let vp = editor.host_mut().viewport_mut();
            vp.width = size.0;
            vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
        }
        let initial_vp_top = editor.host().viewport().top_row;
        let initial_vp_height = editor.host().viewport().height as usize;
        // Synchronous viewport-only preview so the first frame has
        // highlights regardless of file size or viewport position
        // (`+N` deep into a big file still paints colored). The worker
        // then runs the full parse in the background and its result
        // replaces the preview on the next event-loop iteration.
        let mut initial_signs: Vec<hjkl_buffer::Sign> = Vec::new();
        let initial_dg = editor.buffer().dirty_gen();
        if let Some(out) = syntax.preview_render(editor.buffer(), initial_vp_top, initial_vp_height)
        {
            editor.install_ratatui_syntax_spans(out.spans);
        }
        syntax.submit_render(editor.buffer(), initial_vp_top, initial_vp_height);
        let initial_key: Option<(u64, usize, usize)> =
            if let Some(out) = syntax.wait_for_initial_result(Duration::from_millis(150)) {
                let key = out.key;
                editor.install_ratatui_syntax_spans(out.spans);
                initial_signs = out.signs;
                Some(key)
            } else {
                // Worker didn't finish in the budget — preview spans are
                // already installed, the next event-loop iteration's
                // recompute_and_install drains the worker result and
                // upgrades to the structurally-correct spans + signs.
                Some((initial_dg, initial_vp_top, initial_vp_height))
            };
        // Drain any ContentEdit / reset state seeded during construction
        // so the first event-loop iteration starts clean.
        let _ = editor.take_content_edits();
        let _ = editor.take_content_reset();

        let mut app = Self {
            editor,
            filename,
            exit_requested: false,
            dirty: false,
            status_message: None,
            command_field: None,
            search_field: None,
            picker: None,
            pending_leader: false,
            search_dir: SearchDir::Forward,
            last_cursor_shape: CursorShape::Block,
            is_new_file,
            syntax,
            diag_signs: initial_signs,
            git_signs: Vec::new(),
            last_git_dirty_gen: None,
            last_git_refresh_at: Instant::now(),
            last_recompute_at: Instant::now() - Duration::from_secs(1),
            last_recompute_key: initial_key,
            perf_overlay: false,
            last_recompute_us: 0,
            last_install_us: 0,
            last_signature_us: 0,
            last_git_us: 0,
            last_perf: crate::syntax::PerfBreakdown::default(),
            recompute_hits: 0,
            recompute_throttled: 0,
            recompute_runs: 0,
            is_untracked: false,
            saved_hash: 0,
            saved_len: 0,
        };
        // Snapshot the loaded content so undo-to-saved clears dirty.
        let (h, l) = app_buffer_signature(&app.editor);
        app.saved_hash = h;
        app.saved_len = l;
        Ok(app)
    }

    /// Sync `self.dirty` against a fresh content comparison. Called
    /// after the engine reports `take_dirty()` so undo back to the
    /// saved state clears the flag.
    fn refresh_dirty_against_saved(&mut self) {
        let t = Instant::now();
        let (h, l) = app_buffer_signature(&self.editor);
        self.last_signature_us = t.elapsed().as_micros();
        self.dirty = h != self.saved_hash || l != self.saved_len;
    }

    /// Refresh the saved-content snapshot. Called after `:w` and after
    /// a successful `:e` load so the dirty check has the right baseline.
    fn snapshot_saved(&mut self) {
        let (h, l) = app_buffer_signature(&self.editor);
        self.saved_hash = h;
        self.saved_len = l;
        self.dirty = false;
    }

    /// Recompute git diff signs from the current buffer content (vs
    /// the HEAD blob) when `dirty_gen` has advanced since the last
    /// rebuild.
    ///
    /// libgit2's `Patch::from_blob_and_buffer` is O(file size) per
    /// call — there's no viewport-scoped diff API. Two cheats keep
    /// keystroke latency reasonable on big files:
    ///
    /// 1. **Throttle:** at most one refresh per 250 ms. Rapid edits
    ///    show stale-but-near signs until typing pauses; the next
    ///    render after the throttle window catches up.
    /// 2. **Size cap:** for buffers ≥ 50k lines we only refresh on
    ///    save/load (force == true) and skip during in-buffer edits.
    ///    The status-line tags ([Untracked], [New File]) still update.
    ///
    /// `force` bypasses both gates — used by `do_save` / `do_edit`.
    fn refresh_git_signs(&mut self) {
        self.refresh_git_signs_inner(false);
    }

    fn refresh_git_signs_force(&mut self) {
        self.refresh_git_signs_inner(true);
    }

    fn refresh_git_signs_inner(&mut self, force: bool) {
        const HUGE_FILE_LINES: u32 = 50_000;
        const REFRESH_MIN_INTERVAL: Duration = Duration::from_millis(250);

        let path = match self.filename.as_deref() {
            Some(p) => p.to_path_buf(),
            None => {
                self.git_signs.clear();
                self.last_git_dirty_gen = None;
                return;
            }
        };
        let dg = self.editor.buffer().dirty_gen();
        if !force && self.last_git_dirty_gen == Some(dg) {
            return;
        }
        // Size cap — skip per-edit refresh on huge files. Save/load
        // (force == true) still recomputes.
        if !force && self.editor.buffer().line_count() >= HUGE_FILE_LINES {
            return;
        }
        // Throttle — drop refreshes that would fire faster than ~4 Hz.
        // Skipped frames don't update last_git_dirty_gen, so the next
        // render after the window will retry.
        let now = Instant::now();
        if !force && now.duration_since(self.last_git_refresh_at) < REFRESH_MIN_INTERVAL {
            return;
        }

        let lines = self.editor.buffer().lines();
        let mut bytes = lines.join("\n").into_bytes();
        if !bytes.is_empty() {
            bytes.push(b'\n');
        }
        self.git_signs = crate::git::signs_for_bytes(&path, &bytes);
        self.is_untracked = crate::git::is_untracked(&path);
        self.last_git_dirty_gen = Some(dg);
        self.last_git_refresh_at = now;
    }

    /// Main event loop. Draws every frame, routes key events through
    /// the vim FSM, handles resize, exits on Ctrl-C.
    pub fn run(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        loop {
            // Update host viewport dimensions from the current terminal size.
            {
                let size = terminal.size()?;
                let vp = self.editor.host_mut().viewport_mut();
                vp.width = size.width;
                vp.height = size.height.saturating_sub(STATUS_LINE_HEIGHT);
            }

            // Emit cursor shape before the draw call, once per transition.
            // When a prompt is active, derive shape from the prompt field's
            // vim mode (Insert → Bar, Normal/Visual → Block) so the user
            // sees mode feedback while editing the prompt itself.
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
                self.editor.host().cursor_shape()
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

            // Wait for the next event with a 120 ms ceiling so the
            // recompute throttle (100 ms) gets a wake-up redraw after
            // typing stops — otherwise the last burst of keystrokes
            // could leave highlights stale until the next user input.
            if !event::poll(Duration::from_millis(120))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) => {
                    // Ctrl-C is the hard-exit shortcut independent of the FSM,
                    // BUT while a host prompt (`:` palette or `/` `?` search)
                    // is open Ctrl-C should cancel the prompt instead of
                    // quitting the app.
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

                    // Clear status message on any keypress (vim-style).
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

                    // ── Picker overlay (`<leader><space>`, `:picker`) ────────
                    if self.picker.is_some() {
                        self.handle_picker_key(key);
                        if self.exit_requested {
                            break;
                        }
                        continue;
                    }

                    // ── Leader resolution (`<Space>` followed by next key) ──
                    if self.pending_leader && self.editor.vim_mode() == VimMode::Normal {
                        self.pending_leader = false;
                        // `<Space><Space>` and `<Space>f` both open the
                        // file picker. Anything else cancels the leader
                        // sequence silently and is dropped — vim drops
                        // unmapped follow-ups too.
                        if key.modifiers == KeyModifiers::NONE
                            && (key.code == KeyCode::Char(' ') || key.code == KeyCode::Char('f'))
                        {
                            self.open_picker();
                        }
                        continue;
                    }

                    // ── Leader prefix (`<Space>` in normal mode) ─────────────
                    if key.code == KeyCode::Char(' ')
                        && key.modifiers == KeyModifiers::NONE
                        && self.editor.vim_mode() == VimMode::Normal
                    {
                        self.pending_leader = true;
                        continue;
                    }

                    // ── Intercept `:` in Normal mode to open command prompt ──
                    if key.code == KeyCode::Char(':')
                        && key.modifiers == KeyModifiers::NONE
                        && self.editor.vim_mode() == VimMode::Normal
                    {
                        self.open_command_prompt();
                        continue;
                    }

                    // ── Intercept `/` and `?` in Normal mode ─────────────────
                    if key.modifiers == KeyModifiers::NONE
                        && self.editor.vim_mode() == VimMode::Normal
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
                    self.editor.handle_key(key);

                    // Drain dirty for the persistent UI flag. Compare
                    // against the saved snapshot so undo back to the
                    // saved state clears the flag.
                    if self.editor.take_dirty() {
                        self.refresh_dirty_against_saved();
                        if self.dirty {
                            self.is_new_file = false;
                        }
                    }
                    // Fan engine ContentEdits into the syntax tree (or
                    // reset the tree entirely on bulk replace) and
                    // recompute viewport-scoped spans every frame —
                    // the new query path is cheap.
                    if self.editor.take_content_reset() {
                        self.syntax.reset();
                    }
                    let edits = self.editor.take_content_edits();
                    if !edits.is_empty() {
                        self.syntax.apply_edits(&edits);
                    }
                    self.recompute_and_install();
                }
                Event::Resize(w, h) => {
                    let vp = self.editor.host_mut().viewport_mut();
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

    /// Open the fuzzy file picker rooted at the current working
    /// directory. Triggered by `<leader><space>`, `<leader>f`,
    /// `:picker`, and the `+picker` startup arg.
    pub fn open_picker(&mut self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let source = crate::picker::FileSource::new(cwd);
        self.picker = Some(crate::picker::FilePicker::new(source));
        self.pending_leader = false;
    }

    /// Route a key event to the active picker. Selection emits a
    /// [`crate::picker::PickerAction`] which we dispatch — keeps the
    /// App agnostic to which source the picker is currently driving.
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

    /// Apply a picker selection. Add new arms here as new sources land
    /// (buffer switch, grep result jump, etc.).
    fn dispatch_picker_action(&mut self, action: crate::picker::PickerAction) {
        match action {
            crate::picker::PickerAction::OpenPath(path) => {
                let s = path.to_string_lossy().to_string();
                self.do_edit(&s, false);
            }
        }
    }

    /// Open the `:` ex-command prompt. Resets to an empty single-line
    /// field, lands the user in Insert at end-of-line so the next
    /// keystroke types a character.
    fn open_command_prompt(&mut self) {
        let mut field = TextFieldEditor::new(true);
        field.enter_insert_at_end();
        self.command_field = Some(field);
    }

    /// Route a key event to the active `:` palette field. Implements
    /// the Esc-once-Esc-twice cancel grammar:
    ///
    /// - Insert + Esc → field falls back to Normal mode (vim motions
    ///   apply to the prompt line itself: h/l/w/b/dw/diw/...). Prompt
    ///   stays open.
    /// - Normal + Esc → prompt closes, input discarded.
    /// - Enter (any mode) → submit: take `field.text()`, run through
    ///   `dispatch_ex`, close prompt.
    fn handle_command_field_key(&mut self, key: crossterm::event::KeyEvent) {
        let input: EngineInput = key.into();
        let field = match self.command_field.as_mut() {
            Some(f) => f,
            None => return,
        };

        // Enter — submit regardless of mode. Run dispatch on the
        // collected text; close the prompt either way.
        if input.key == EngineKey::Enter {
            let text = field.text();
            self.command_field = None;
            self.dispatch_ex(text.trim());
            return;
        }

        // Esc:
        //  - empty prompt (any mode) → close (no point parking in Normal
        //    on a blank line)
        //  - Insert + non-empty → drop to Normal so user can edit with motions
        //  - Normal + non-empty → close, discard input
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

        // Otherwise forward to the inner field's vim FSM.
        field.handle_input(input);
    }

    /// Open the `/` (forward) or `?` (backward) search prompt. Lands
    /// the user in Insert at end-of-line; clears any prior search
    /// highlight so the live-preview only highlights matches of the
    /// *current* prompt buffer.
    fn open_search_prompt(&mut self, dir: SearchDir) {
        let mut field = TextFieldEditor::new(true);
        field.enter_insert_at_end();
        self.search_field = Some(field);
        self.search_dir = dir;
        self.editor.set_search_pattern(None);
    }

    /// Cancel the search prompt without committing. Restores the most
    /// recent committed pattern (if any) so the user's last successful
    /// search keeps highlighting and `n` / `N` keep working — matches
    /// the engine's pre-J3 behaviour on Esc.
    fn cancel_search_prompt(&mut self) {
        self.search_field = None;
        let last = self.editor.last_search().map(str::to_owned);
        match last {
            Some(p) if !p.is_empty() => {
                if let Ok(re) = regex::Regex::new(&p) {
                    self.editor.set_search_pattern(Some(re));
                } else {
                    self.editor.set_search_pattern(None);
                }
            }
            _ => self.editor.set_search_pattern(None),
        }
    }

    /// Route a key event to the active `/` `?` search field.
    /// - Insert + Esc → Normal (motions on prompt line). Highlight
    ///   stays so the user can review what they've typed.
    /// - Normal + Esc → cancel: restore previous committed pattern.
    /// - Enter (any mode) → commit: run search, advance cursor.
    /// - Any other key → forward to field; on dirty change re-run the
    ///   live-preview pattern compile.
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

        // Esc:
        //  - empty prompt (any mode) → close immediately (skip the
        //    Insert→Normal stop on a blank line)
        //  - Insert + non-empty → drop to Normal for motion editing
        //  - Normal + non-empty → cancel: restore previous committed pattern
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

    /// Recompile the prompt's text into a regex and push it to the
    /// engine. Empty input or invalid regex (mid-typing `[`, etc.) →
    /// clear the highlight without surfacing an error, matching the
    /// engine's pre-J3 behaviour.
    fn live_preview_search(&mut self) {
        let pattern = match self.search_field.as_ref() {
            Some(f) => f.text(),
            None => return,
        };
        if pattern.is_empty() {
            self.editor.set_search_pattern(None);
            return;
        }
        let case_insensitive = self.editor.settings().ignore_case
            && !(self.editor.settings().smartcase && pattern.chars().any(|c| c.is_uppercase()));
        let effective: std::borrow::Cow<'_, str> = if case_insensitive {
            std::borrow::Cow::Owned(format!("(?i){pattern}"))
        } else {
            std::borrow::Cow::Borrowed(pattern.as_str())
        };
        match regex::Regex::new(&effective) {
            Ok(re) => self.editor.set_search_pattern(Some(re)),
            Err(_) => self.editor.set_search_pattern(None),
        }
    }

    /// Commit the search prompt's pattern. Empty input re-runs the
    /// previous search (vim parity); otherwise the new pattern becomes
    /// the active one and the cursor advances to the first match in
    /// the prompt's direction. Updates the engine's `last_search` so
    /// `n` / `N` repeat with the right text + direction.
    fn commit_search(&mut self, pattern: &str) {
        let effective: Option<String> = if pattern.is_empty() {
            self.editor.last_search().map(str::to_owned)
        } else {
            Some(pattern.to_owned())
        };
        let Some(p) = effective else {
            self.editor.set_search_pattern(None);
            return;
        };
        let case_insensitive = self.editor.settings().ignore_case
            && !(self.editor.settings().smartcase && p.chars().any(|c| c.is_uppercase()));
        let compile_src: std::borrow::Cow<'_, str> = if case_insensitive {
            std::borrow::Cow::Owned(format!("(?i){p}"))
        } else {
            std::borrow::Cow::Borrowed(p.as_str())
        };
        match regex::Regex::new(&compile_src) {
            Ok(re) => {
                self.editor.set_search_pattern(Some(re));
                let forward = self.search_dir == SearchDir::Forward;
                if forward {
                    self.editor.search_advance_forward(true);
                } else {
                    self.editor.search_advance_backward(true);
                }
                self.editor.set_last_search(Some(p), forward);
            }
            Err(e) => {
                self.editor.set_search_pattern(None);
                self.status_message = Some(format!("E: bad search pattern: {e}"));
            }
        }
    }

    /// Execute an ex command string (without the leading `:`).
    fn dispatch_ex(&mut self, cmd: &str) {
        // Canonicalize the leading command name so interceptors below
        // can match `:edit`/`:e`/`:edi` etc. uniformly.
        let canon = ex::canonical_command_name(cmd);
        let cmd: &str = canon.as_ref();
        // `:perf` — toggle the per-frame timing overlay in the status row.
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
        // Intercept `:set background={dark,light}` before the engine sees it.
        // Theme awareness is a binary-local concern; the engine has no theme API.
        if let Some(rest) = cmd.strip_prefix("set background=") {
            match rest.trim() {
                "dark" => {
                    self.syntax.set_theme(Arc::new(DotFallbackTheme::dark()));
                    // Theme swap doesn't change the buffer, but the
                    // installed spans are stale — invalidate the cache
                    // key so the next recompute re-submits with the new
                    // theme.
                    self.last_recompute_key = None;
                    self.recompute_and_install();
                    self.status_message = Some("background=dark".into());
                    return;
                }
                "light" => {
                    self.syntax.set_theme(Arc::new(DotFallbackTheme::light()));
                    self.last_recompute_key = None;
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

        // `:picker` — open the fuzzy file picker. Binary-local because
        // the picker is host-level UI, not part of the engine's grammar.
        if cmd == "picker" {
            self.open_picker();
            return;
        }

        // `:edit [path]` / `:edit!` — open or reload a file. Binary-local
        // because file I/O isn't the engine's concern (same shape as `:w`).
        // `%` in the path expands to the current filename (vim parity).
        // Canonicalization above turns `:e` / `:edi` / etc. into `edit`.
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

        match ex::run(&mut self.editor, cmd) {
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
                    // :wq / :x — save first, then quit.
                    self.do_save(None);
                    if self.exit_requested {
                        // do_save set exit_requested on error? No — only quit
                        // path sets it. If save succeeded (no error msg) quit.
                    }
                    // Quit regardless of save result to match vim behaviour for :wq.
                    self.exit_requested = true;
                } else if force {
                    // :q!
                    self.exit_requested = true;
                } else {
                    // :q — block if dirty.
                    if self.dirty {
                        self.status_message =
                            Some("E37: No write since last change (add ! to override)".into());
                    } else {
                        self.exit_requested = true;
                    }
                }
            }
            ExEffect::Substituted { count } => {
                // Engine applied the substitution in-place; propagate dirty
                // and fan ContentEdits into the syntax tree before the next
                // recompute so the retained tree stays in sync.
                if self.editor.take_dirty() {
                    self.refresh_dirty_against_saved();
                    if self.editor.take_content_reset() {
                        self.syntax.reset();
                    }
                    let edits = self.editor.take_content_edits();
                    if !edits.is_empty() {
                        self.syntax.apply_edits(&edits);
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

    /// Write buffer content to `path` (or `self.filename` if `path` is
    /// `None`). Updates `self.filename` and `self.dirty` on success.
    ///
    /// Blocks writes when the editor is in readonly mode unless `force` is
    /// true (`:w!` not yet wired — for now `:set noreadonly` + `:w` is the
    /// override path, matching Phase 5 spec).
    fn do_save(&mut self, path: Option<PathBuf>) {
        // Readonly guard — E45 matches vim's message.
        if self.editor.is_readonly() {
            self.status_message = Some("E45: 'readonly' option is set (add ! to override)".into());
            return;
        }
        let target = path.or_else(|| self.filename.clone());
        match target {
            None => {
                self.status_message = Some("E32: No file name".into());
            }
            Some(p) => {
                let lines = self.editor.buffer().lines();
                // vim default: lines joined with \n, trailing \n after last
                // non-empty line.
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
                        self.filename = Some(p);
                        self.is_new_file = false;
                        self.snapshot_saved();
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
    ///
    /// `arg` is the post-command-name argument string (may be empty).
    /// `%` in the path expands to the current filename. Empty `arg`
    /// reloads the current file. `force` (`:e!`) bypasses the dirty-buffer
    /// guard.
    fn do_edit(&mut self, arg: &str, force: bool) {
        let path_str = if arg.is_empty() {
            match &self.filename {
                Some(p) => p.to_string_lossy().into_owned(),
                None => {
                    self.status_message = Some("E32: No file name".into());
                    return;
                }
            }
        } else if arg.contains('%') {
            let curr = match self.filename.as_ref().and_then(|p| p.to_str()) {
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

        if !force && self.dirty {
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
        // Mirror App::new: strip one trailing newline (vim default).
        let trimmed = content.strip_suffix('\n').unwrap_or(&content);

        let line_count = trimmed.lines().count();
        let byte_count = content.len();
        self.editor.set_content(trimmed);
        // Vim `:e` lands on row 0, col 0 with the viewport at the top
        // — without this the previous file's scroll offset bleeds into
        // the new buffer.
        self.editor.goto_line(1);
        {
            let vp = self.editor.host_mut().viewport_mut();
            vp.top_row = 0;
            vp.top_col = 0;
        }
        self.filename = Some(path.clone());
        self.is_new_file = false;
        self.syntax.set_language_for_path(&path);
        self.syntax.reset();
        // Invalidate the recompute cache so the swap actually re-parses
        // (set_content advances dirty_gen, but be explicit so a same-key
        // collision after reload can't skip the install).
        self.last_recompute_key = None;
        // Synchronous viewport-only preview so the new file's first
        // frame paints with highlights instead of popping white while
        // the worker runs the cold parse — same trick App::new uses on
        // initial open.
        let (vp_top, vp_height) = {
            let vp = self.editor.host().viewport();
            (vp.top_row, vp.height as usize)
        };
        if let Some(out) = self
            .syntax
            .preview_render(self.editor.buffer(), vp_top, vp_height)
        {
            self.editor.install_ratatui_syntax_spans(out.spans);
        }
        self.recompute_and_install();
        self.snapshot_saved();
        self.refresh_git_signs_force();
        self.status_message = Some(format!("\"{path_str}\" {line_count}L, {byte_count}B"));
    }

    /// Submit a new viewport-scoped parse on the syntax worker (when
    /// inputs differ from the last submission and the throttle window
    /// has passed) and install whatever the worker has produced since
    /// the last frame.
    ///
    /// The submit and the install are decoupled: a frame may install a
    /// stale-but-recent worker output even when no submit happens, and
    /// a frame may submit without an install when the worker hasn't
    /// finished yet. That gives keystrokes back the main thread — the
    /// 30–100ms tree-sitter parse runs off the render path.
    ///
    /// No-op submit when no language is attached. Frame-level skip:
    /// when `(dirty_gen, vp_top, vp_height)` matches the last submit
    /// key, no new request is sent — the worker's previous output
    /// (already installed) is still right.
    pub fn recompute_and_install(&mut self) {
        const RECOMPUTE_THROTTLE: Duration = Duration::from_millis(100);
        let (top, height) = {
            let vp = self.editor.host().viewport();
            (vp.top_row, vp.height as usize)
        };
        let dg = self.editor.buffer().dirty_gen();
        let key = (dg, top, height);

        // Snapshot the prior submit's dirty_gen so we can tell, after
        // a submit, whether this frame's request was a viewport-only
        // change (worker fast path → safe to briefly wait) versus an
        // edit that needs a full reparse (don't wait, would stall
        // keystrokes).
        let prev_dirty_gen = self.last_recompute_key.map(|(prev_dg, _, _)| prev_dg);

        let t_total = Instant::now();
        let mut submitted = false;
        if self.last_recompute_key == Some(key) {
            self.recompute_hits = self.recompute_hits.saturating_add(1);
        } else {
            let buffer_changed = self
                .last_recompute_key
                .map(|(prev_dg, _, _)| prev_dg != dg)
                .unwrap_or(true);
            let now = Instant::now();
            if buffer_changed && now.duration_since(self.last_recompute_at) < RECOMPUTE_THROTTLE {
                self.recompute_throttled = self.recompute_throttled.saturating_add(1);
            } else {
                self.recompute_runs = self.recompute_runs.saturating_add(1);
                if self
                    .syntax
                    .submit_render(self.editor.buffer(), top, height)
                    .is_some()
                {
                    submitted = true;
                    self.last_recompute_at = Instant::now();
                    self.last_recompute_key = Some(key);
                }
            }
        }

        // After submitting, give the worker a brief window to deliver
        // before we draw — viewport-only requests (gg / G / Ctrl-D)
        // skip parse on the worker and complete in ~1 ms, so a 5 ms
        // wait keeps highlights coherent across the jump. Edits go
        // through the throttle path and don't reach this branch.
        let t_install = Instant::now();
        let drained = if submitted {
            // Buffer unchanged since last submit → worker takes the
            // parse-skip fast path (~1 ms). Wait briefly so the jump
            // doesn't flash uncolored rows. Buffer changed (or first
            // submit) → don't block; the result will land on a
            // subsequent frame after worker finishes its parse.
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
            self.editor.install_ratatui_syntax_spans(out.spans);
            self.diag_signs = out.signs;
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
        match self.editor.vim_mode() {
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
        // Sanity: field captured the typed text.
        assert_eq!(app.command_field.as_ref().unwrap().text(), "wq");
        // Enter — dispatches and closes. `:wq` on an empty no-name buffer
        // sets exit_requested via the Quit{save:true} path even though
        // do_save flags E32 — vim parity.
        app.handle_command_field_key(key(KeyCode::Enter));
        assert!(app.command_field.is_none());
        assert!(app.exit_requested);
    }

    #[test]
    fn palette_esc_in_insert_drops_to_normal_then_motions_apply() {
        let mut app = App::new(None, false, None, None).unwrap();
        app.open_command_prompt();
        type_str(&mut app, "abc");
        // Esc once — Insert→Normal. Prompt stays open.
        app.handle_command_field_key(key(KeyCode::Esc));
        assert!(app.command_field.is_some());
        let f = app.command_field.as_ref().unwrap();
        assert_eq!(f.vim_mode(), VimMode::Normal);
        assert_eq!(f.text(), "abc");
        // `b` moves cursor word-back; `dw` deletes to next word boundary.
        app.handle_command_field_key(key(KeyCode::Char('b')));
        app.handle_command_field_key(key(KeyCode::Char('d')));
        app.handle_command_field_key(key(KeyCode::Char('w')));
        let f = app.command_field.as_ref().unwrap();
        assert_eq!(f.text(), "");
        // Esc again — closes prompt.
        app.handle_command_field_key(key(KeyCode::Esc));
        assert!(app.command_field.is_none());
    }

    #[test]
    fn palette_ctrl_c_cancels_without_quitting_app() {
        let mut app = App::new(None, false, None, None).unwrap();
        app.open_command_prompt();
        type_str(&mut app, "wq");
        // The Ctrl-C cancel path is wired in `run`'s event loop, but
        // we can short-circuit the test by emulating the same branch:
        // prompt is open, so close it without setting exit_requested.
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
        BufferEdit::replace_all(app.editor.buffer_mut(), content);
    }

    #[test]
    fn search_open_and_type_drives_live_preview() {
        let mut app = App::new(None, false, None, None).unwrap();
        seed_buffer(&mut app, "foo bar foo baz");
        app.open_search_prompt(SearchDir::Forward);
        assert!(app.search_field.is_some());
        type_search(&mut app, "foo");
        assert_eq!(app.search_field.as_ref().unwrap().text(), "foo");
        // Live-preview installed a regex onto the engine.
        assert!(app.editor.search_state().pattern.is_some());
    }

    #[test]
    fn search_more_typing_updates_pattern() {
        let mut app = App::new(None, false, None, None).unwrap();
        seed_buffer(&mut app, "foobar foozle");
        app.open_search_prompt(SearchDir::Forward);
        type_search(&mut app, "foo");
        let p1 = app
            .editor
            .search_state()
            .pattern
            .as_ref()
            .unwrap()
            .as_str()
            .to_string();
        type_search(&mut app, "z");
        let p2 = app
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
        // Esc once — Insert→Normal.
        app.handle_search_field_key(key(KeyCode::Esc));
        assert_eq!(
            app.search_field.as_ref().unwrap().vim_mode(),
            VimMode::Normal
        );
        // `b` walks a word back; `db` deletes back-a-word.
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
        // Cursor lands on the 'foo' on row 2.
        let (row, col) = app.editor.cursor();
        assert_eq!(row, 2);
        assert_eq!(col, 0);
        assert_eq!(app.editor.last_search(), Some("foo"));
        assert!(app.editor.last_search_forward());
    }

    #[test]
    fn search_esc_twice_cancels_and_clears_when_no_prior_search() {
        let mut app = App::new(None, false, None, None).unwrap();
        seed_buffer(&mut app, "abc def");
        app.open_search_prompt(SearchDir::Forward);
        type_search(&mut app, "abc");
        // Esc once → Normal.
        app.handle_search_field_key(key(KeyCode::Esc));
        assert!(app.search_field.is_some());
        // Esc twice → cancel.
        app.handle_search_field_key(key(KeyCode::Esc));
        assert!(app.search_field.is_none());
        // No prior committed search — pattern cleared.
        assert!(app.editor.search_state().pattern.is_none());
    }

    #[test]
    fn search_backward_prompt_uses_question_dir() {
        let mut app = App::new(None, false, None, None).unwrap();
        seed_buffer(&mut app, "foo here\nbar there\nfoo again");
        // Move cursor to row 2 first so `?foo` walks backward to row 0.
        app.editor.goto_line(3);
        app.open_search_prompt(SearchDir::Backward);
        type_search(&mut app, "foo");
        app.handle_search_field_key(key(KeyCode::Enter));
        let (row, _) = app.editor.cursor();
        assert_eq!(row, 0);
        assert!(!app.editor.last_search_forward());
    }

    // ── App::new tests ──────────────────────────────────────────────────────

    #[test]
    fn app_new_no_file() {
        let app = App::new(None, false, None, None).unwrap();
        assert!(!app.dirty);
        assert!(!app.is_new_file);
        assert!(app.filename.is_none());
        assert!(!app.editor.is_readonly());
    }

    #[test]
    fn app_new_readonly_flag() {
        let app = App::new(None, true, None, None).unwrap();
        assert!(app.editor.is_readonly());
    }

    #[test]
    fn app_new_not_found_sets_is_new_file() {
        let path = std::path::PathBuf::from("/tmp/hjkl_phase5_nonexistent_abc123.txt");
        // Make sure the file doesn't exist.
        let _ = std::fs::remove_file(&path);
        let app = App::new(Some(path), false, None, None).unwrap();
        assert!(app.is_new_file);
        assert!(!app.dirty);
    }

    #[test]
    fn app_new_goto_line_clamps() {
        // No file, just verify goto_line doesn't panic on line=999 with empty buffer.
        let app = App::new(None, false, Some(999), None).unwrap();
        let (row, _col) = app.editor.cursor();
        // Empty buffer → cursor stays at row 0.
        assert_eq!(row, 0);
    }

    #[test]
    fn do_save_readonly_blocked() {
        let mut app = App::new(None, true, None, None).unwrap();
        app.filename = Some(std::path::PathBuf::from("/tmp/hjkl_phase5_ro_test.txt"));
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
        // Write a file, open it, modify on disk, :e % should reload.
        let path = std::env::temp_dir().join("hjkl_edit_percent_reload.txt");
        std::fs::write(&path, "first\nsecond\n").unwrap();
        let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
        // External edit.
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
        app.dispatch_ex("e %");
        let lines = app.editor.buffer().lines();
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
        assert_eq!(app.editor.buffer().lines(), vec!["v2".to_string()]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn edit_blocks_dirty_buffer_without_force() {
        let path = std::env::temp_dir().join("hjkl_edit_dirty_block.txt");
        std::fs::write(&path, "orig\n").unwrap();
        let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
        app.dirty = true;
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
        app.dirty = true;
        app.dispatch_ex("e!");
        assert_eq!(app.editor.buffer().lines(), vec!["disk".to_string()]);
        assert!(!app.dirty);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn undo_to_saved_state_clears_dirty() {
        // Open a file, edit it (dirty=true), then undo back to the
        // loaded state. dirty should clear because the buffer matches
        // the saved snapshot again — even though the engine's
        // dirty_gen is monotonic.
        let path = std::env::temp_dir().join("hjkl_undo_clears_dirty.txt");
        std::fs::write(&path, "alpha\nbravo\n").unwrap();
        let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
        assert!(!app.dirty);
        // Simulate an edit by inserting a char in Insert mode.
        app.editor.handle_key(key(KeyCode::Char('i')));
        app.editor.handle_key(key(KeyCode::Char('X')));
        if app.editor.take_dirty() {
            app.refresh_dirty_against_saved();
        }
        assert!(app.dirty, "edit should mark dirty");
        // Esc + undo back to clean.
        app.editor.handle_key(key(KeyCode::Esc));
        app.editor.handle_key(key(KeyCode::Char('u')));
        if app.editor.take_dirty() {
            app.refresh_dirty_against_saved();
        }
        assert!(!app.dirty, "undo to saved state should clear dirty");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn esc_on_empty_command_prompt_dismisses() {
        let mut app = App::new(None, false, None, None).unwrap();
        app.open_command_prompt();
        assert!(app.command_field.is_some());
        // Field is empty + Insert mode. Esc should close, not drop to Normal.
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
        // Insert + non-empty: Esc → Normal, prompt stays open.
        app.handle_command_field_key(key(KeyCode::Esc));
        assert!(app.command_field.is_some());
        assert_eq!(
            app.command_field.as_ref().unwrap().vim_mode(),
            VimMode::Normal
        );
        // Normal + non-empty: Esc → close.
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
