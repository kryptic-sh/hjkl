//! `App` — owns the editor + host, drives the event loop.

use anyhow::Result;
use crossterm::{
    cursor::SetCursorStyle,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
};
use hjkl_buffer::Buffer;
use hjkl_engine::Host;
use hjkl_engine::{CursorShape, Editor, Options, VimMode};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::Stdout;
use std::path::PathBuf;

use crate::host::TuiHost;
use crate::render;

/// Height reserved for the status line at the bottom of the screen.
pub const STATUS_LINE_HEIGHT: u16 = 1;

/// Top-level application state. Everything the event loop and renderer need.
pub struct App {
    /// The live editor — buffer + FSM + host, all in one.
    pub editor: Editor<Buffer, TuiHost>,
    /// File path for status line display. Not used for I/O until Phase 3.
    pub filename: Option<PathBuf>,
    /// Set to `true` when the FSM or Ctrl-C wants to quit.
    pub exit_requested: bool,
    /// Last cursor shape we emitted to the terminal. Compared each
    /// frame so we only write the DECSCUSR sequence on transitions.
    last_cursor_shape: CursorShape,
}

impl App {
    /// Build a fresh [`App`] with an empty buffer and default settings.
    /// `filename` is shown in the status line but not read from disk.
    pub fn new(filename: Option<PathBuf>) -> Self {
        let buffer = Buffer::new();
        let host = TuiHost::new();
        let options = Options::default();
        let editor = Editor::new(buffer, host, options);
        Self {
            editor,
            filename,
            exit_requested: false,
            last_cursor_shape: CursorShape::Block,
        }
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
                    self.editor.handle_key(key);
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
