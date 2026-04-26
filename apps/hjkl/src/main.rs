//! `hjkl` — standalone vim-modal terminal editor.
//!
//! Phase 3: file I/O + ex commands (`:w`, `:q`, `:wq`, `:x`) + dirty tracking.

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
    /// File to open. If the file does not exist a new empty buffer is started.
    file: Option<std::path::PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Build app state (may read file from disk) before entering alternate screen
    // so we can print errors to the normal terminal if the file is unreadable.
    let mut app = app::App::new(args.file)?;

    terminal::enable_raw_mode()?;
    execute!(stdout(), terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = app.run(&mut terminal);

    // Restore terminal regardless of outcome.
    let _ = terminal::disable_raw_mode();
    let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen);

    result
}
