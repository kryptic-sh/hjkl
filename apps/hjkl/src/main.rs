//! `hjkl` — standalone vim-modal terminal editor.
//!
//! Phase 2: event loop wired, motions work, mode switching, status line,
//! cursor shape per mode. File loading/saving is Phase 3+.

mod app;
mod host;
mod render;

use anyhow::Result;
use clap::Parser;
use crossterm::{execute, terminal};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, stdout};

#[derive(Parser)]
#[command(version, about = "Vim-modal terminal editor")]
struct Args {
    /// File to open. Phase 2: shown in status line; not yet read from disk.
    file: Option<std::path::PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    terminal::enable_raw_mode()?;
    execute!(stdout(), terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = app::App::new(args.file);
    let result = app.run(&mut terminal);

    // Restore terminal regardless of outcome.
    let _ = terminal::disable_raw_mode();
    let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen);

    result
}
