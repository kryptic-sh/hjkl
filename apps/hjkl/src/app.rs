//! `App` — owns the editor + host, drives the event loop.

use anyhow::Result;
use crossterm::{
    cursor::SetCursorStyle,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
};
use hjkl_buffer::Buffer;
use hjkl_editor::runtime::ex::{self, ExEffect};
use hjkl_engine::{BufferEdit, Host};
use hjkl_engine::{CursorShape, Editor, Options, VimMode};
use hjkl_tree_sitter::DotFallbackTheme;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::Stdout;
use std::path::PathBuf;

use crate::host::TuiHost;
use crate::render;
use crate::syntax::{self, SyntaxLayer};

/// Height reserved for the status line at the bottom of the screen.
pub const STATUS_LINE_HEIGHT: u16 = 1;

/// Line-editing buffer for the `:` command prompt.
///
/// Tracks `text` and a byte-offset `cursor` within it so Phase 4 can
/// render the insertion point and support full editing ops.
///
/// Invariant: `cursor` is always a valid UTF-8 boundary in `text`.
#[derive(Default, Clone)]
pub struct CommandInput {
    /// The typed command text (without the leading `:`).
    pub text: String,
    /// Byte offset of the insertion point within `text`.
    pub cursor: usize,
}

impl CommandInput {
    /// Insert `c` at the current cursor position and advance past it.
    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Delete the character immediately before the cursor (Backspace).
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        // Step back to the previous char boundary.
        let prev = prev_char_boundary(&self.text, self.cursor);
        self.text.drain(prev..self.cursor);
        self.cursor = prev;
    }

    /// Delete the character at the cursor (Delete / Forward-delete).
    pub fn delete_forward(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = next_char_boundary(&self.text, self.cursor);
        self.text.drain(self.cursor..next);
    }

    /// Move cursor one char to the left.
    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = prev_char_boundary(&self.text, self.cursor);
        }
    }

    /// Move cursor one char to the right.
    pub fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor = next_char_boundary(&self.text, self.cursor);
        }
    }

    /// Move cursor to the start of the text (Home / Ctrl-A).
    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    /// Move cursor to the end of the text (End / Ctrl-E).
    pub fn move_end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Clear the text and reset the cursor (Ctrl-U).
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Delete back to the previous word boundary (Ctrl-W).
    ///
    /// Skips trailing spaces, then deletes back to the next space (or the
    /// start of text), matching vim's `Ctrl-W` in command line.
    pub fn delete_word_back(&mut self) {
        if self.cursor == 0 {
            return;
        }
        // Skip trailing spaces.
        let mut pos = self.cursor;
        while pos > 0 {
            let prev = prev_char_boundary(&self.text, pos);
            if !self.text[prev..pos].starts_with(' ') {
                break;
            }
            pos = prev;
        }
        // Delete back to the previous space.
        while pos > 0 {
            let prev = prev_char_boundary(&self.text, pos);
            if self.text[prev..pos].starts_with(' ') {
                break;
            }
            pos = prev;
        }
        self.text.drain(pos..self.cursor);
        self.cursor = pos;
    }

    /// Number of display columns the prefix `char` + text before cursor
    /// occupies. Used by the renderer to place the terminal cursor.
    /// `prefix_width` is 1 for `:`, `/`, `?`.
    pub fn display_cursor_col(&self, prefix_width: usize) -> u16 {
        // For now assume every byte of the text up to cursor is one
        // display column (ASCII assumption; good enough for command input).
        (prefix_width + self.text[..self.cursor].chars().count()) as u16
    }
}

/// Return the byte offset of the char boundary that is strictly before `pos`
/// in `s`. Panics if `pos == 0`.
fn prev_char_boundary(s: &str, pos: usize) -> usize {
    let mut p = pos - 1;
    while !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

/// Return the byte offset of the char boundary that is strictly after `pos`
/// in `s`. Panics if `pos >= s.len()`.
fn next_char_boundary(s: &str, pos: usize) -> usize {
    let mut p = pos + 1;
    while !s.is_char_boundary(p) {
        p += 1;
    }
    p
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
    /// Active `:` command input. `Some` while the user is typing an ex command.
    pub command_input: Option<CommandInput>,
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
        let initial_vp_top = editor.host().viewport().top_row;
        let initial_vp_height = editor.host().viewport().height as usize;
        if let Some(spans) =
            syntax.parse_and_render(editor.buffer(), initial_vp_top, initial_vp_height)
        {
            editor.install_ratatui_syntax_spans(spans);
        }
        // Drain any ContentEdit / reset state seeded during construction
        // so the first event-loop iteration starts clean.
        let _ = editor.take_content_edits();
        let _ = editor.take_content_reset();

        Ok(Self {
            editor,
            filename,
            exit_requested: false,
            dirty: false,
            status_message: None,
            command_input: None,
            last_cursor_shape: CursorShape::Block,
            is_new_file,
            syntax,
        })
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
            let current_shape = self.editor.host().cursor_shape();
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

            // Process the next event (blocking).
            match event::read()? {
                Event::Key(key) => {
                    // Ctrl-C is the hard-exit shortcut independent of the FSM.
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break;
                    }

                    // Clear status message on any keypress (vim-style).
                    self.status_message = None;

                    // ── Command input mode (`:` prompt) ──────────────────────
                    if let Some(ref mut cmd) = self.command_input {
                        match (key.modifiers, key.code) {
                            (KeyModifiers::NONE, KeyCode::Esc) => {
                                self.command_input = None;
                            }
                            (KeyModifiers::NONE, KeyCode::Enter) => {
                                let cmd_text = self.command_input.take().unwrap_or_default().text;
                                self.dispatch_ex(cmd_text.trim());
                            }
                            (KeyModifiers::NONE, KeyCode::Backspace) => {
                                cmd.backspace();
                            }
                            (KeyModifiers::NONE, KeyCode::Delete) => {
                                cmd.delete_forward();
                            }
                            (KeyModifiers::NONE, KeyCode::Left) => {
                                cmd.move_left();
                            }
                            (KeyModifiers::NONE, KeyCode::Right) => {
                                cmd.move_right();
                            }
                            (KeyModifiers::NONE, KeyCode::Home) => {
                                cmd.move_home();
                            }
                            (KeyModifiers::NONE, KeyCode::End) => {
                                cmd.move_end();
                            }
                            // Ctrl-A — move to start (readline convention).
                            (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
                                cmd.move_home();
                            }
                            // Ctrl-E — move to end.
                            (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
                                cmd.move_end();
                            }
                            // Ctrl-U — clear entire line.
                            (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
                                cmd.clear();
                            }
                            // Ctrl-W — delete-word-back.
                            (KeyModifiers::CONTROL, KeyCode::Char('w')) => {
                                cmd.delete_word_back();
                            }
                            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => {
                                cmd.insert_char(c);
                            }
                            _ => {}
                        }
                        // Don't fall through to editor FSM while in cmd mode.
                        if self.exit_requested {
                            break;
                        }
                        continue;
                    }

                    // ── Intercept `:` in Normal mode to open command prompt ──
                    if key.code == KeyCode::Char(':')
                        && key.modifiers == KeyModifiers::NONE
                        && self.editor.vim_mode() == VimMode::Normal
                    {
                        self.command_input = Some(CommandInput::default());
                        continue;
                    }

                    // ── Normal editor key handling ───────────────────────────
                    self.editor.handle_key(key);

                    // Drain dirty for the persistent UI flag.
                    if self.editor.take_dirty() {
                        self.dirty = true;
                        self.is_new_file = false;
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

    /// Execute an ex command string (without the leading `:`).
    fn dispatch_ex(&mut self, cmd: &str) {
        // Intercept `:set background={dark,light}` before the engine sees it.
        // Theme awareness is a binary-local concern; the engine has no theme API.
        if let Some(rest) = cmd.strip_prefix("set background=") {
            match rest.trim() {
                "dark" => {
                    self.syntax.set_theme(Box::new(DotFallbackTheme::dark()));
                    self.recompute_and_install();
                    self.status_message = Some("background=dark".into());
                    return;
                }
                "light" => {
                    self.syntax.set_theme(Box::new(DotFallbackTheme::light()));
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
                    self.dirty = true;
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
                        self.dirty = false;
                        self.is_new_file = false;
                    }
                    Err(e) => {
                        self.status_message = Some(format!("E: {}: {e}", p.display()));
                    }
                }
            }
        }
    }

    /// Recompute syntax spans for the current viewport and install them.
    ///
    /// No-op when no language is attached (highlighter is `None`) or
    /// when the incremental parse timed out (caller retries next frame).
    pub fn recompute_and_install(&mut self) {
        let (top, height) = {
            let vp = self.editor.host().viewport();
            (vp.top_row, vp.height as usize)
        };
        if let Some(spans) = self
            .syntax
            .parse_and_render(self.editor.buffer(), top, height)
        {
            self.editor.install_ratatui_syntax_spans(spans);
        }
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

    fn ci(text: &str, cursor: usize) -> CommandInput {
        CommandInput {
            text: text.to_string(),
            cursor,
        }
    }

    #[test]
    fn insert_char_at_end() {
        let mut c = CommandInput::default();
        c.insert_char('h');
        c.insert_char('i');
        assert_eq!(c.text, "hi");
        assert_eq!(c.cursor, 2);
    }

    #[test]
    fn insert_char_at_middle() {
        let mut c = ci("ac", 1);
        c.insert_char('b');
        assert_eq!(c.text, "abc");
        assert_eq!(c.cursor, 2);
    }

    #[test]
    fn backspace_removes_before_cursor() {
        let mut c = ci("abc", 2);
        c.backspace();
        assert_eq!(c.text, "ac");
        assert_eq!(c.cursor, 1);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut c = ci("abc", 0);
        c.backspace();
        assert_eq!(c.text, "abc");
        assert_eq!(c.cursor, 0);
    }

    #[test]
    fn delete_forward_removes_at_cursor() {
        let mut c = ci("abc", 1);
        c.delete_forward();
        assert_eq!(c.text, "ac");
        assert_eq!(c.cursor, 1);
    }

    #[test]
    fn delete_forward_at_end_is_noop() {
        let mut c = ci("abc", 3);
        c.delete_forward();
        assert_eq!(c.text, "abc");
    }

    #[test]
    fn move_left_right() {
        let mut c = ci("ab", 2);
        c.move_left();
        assert_eq!(c.cursor, 1);
        c.move_left();
        assert_eq!(c.cursor, 0);
        c.move_left(); // already at start
        assert_eq!(c.cursor, 0);
        c.move_right();
        assert_eq!(c.cursor, 1);
    }

    #[test]
    fn home_end() {
        let mut c = ci("hello", 3);
        c.move_home();
        assert_eq!(c.cursor, 0);
        c.move_end();
        assert_eq!(c.cursor, 5);
    }

    #[test]
    fn clear_resets_text_and_cursor() {
        let mut c = ci("hello", 3);
        c.clear();
        assert_eq!(c.text, "");
        assert_eq!(c.cursor, 0);
    }

    #[test]
    fn delete_word_back_removes_word() {
        let mut c = ci("hello world", 11);
        c.delete_word_back();
        assert_eq!(c.text, "hello ");
        assert_eq!(c.cursor, 6);
    }

    #[test]
    fn delete_word_back_skips_trailing_spaces() {
        let mut c = ci("hello   ", 8);
        c.delete_word_back();
        assert_eq!(c.text, "");
        assert_eq!(c.cursor, 0);
    }

    #[test]
    fn delete_word_back_at_start_is_noop() {
        let mut c = ci("hello", 0);
        c.delete_word_back();
        assert_eq!(c.text, "hello");
        assert_eq!(c.cursor, 0);
    }

    #[test]
    fn display_cursor_col_counts_correctly() {
        let c = ci("hello", 3);
        // prefix width 1 (`:`) + 3 chars before cursor = 4
        assert_eq!(c.display_cursor_col(1), 4);
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
}
