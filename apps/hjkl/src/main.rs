//! `hjkl` — standalone vim-modal terminal editor.
//!
//! Phase 1 scaffold. Boots a ratatui alternate screen, renders an empty
//! buffer panel with a status line, and exits cleanly on `Ctrl+C`. No
//! event loop, no file loading, no motions — Phase 2+ work.

mod host;

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute, terminal,
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, Paragraph},
};
use std::io::{self, Stdout, stdout};

#[derive(Parser)]
#[command(version, about = "Vim-modal terminal editor")]
struct Args {
    /// File to open (Phase 1: ignored — empty buffer always).
    file: Option<std::path::PathBuf>,
}

fn main() -> Result<()> {
    let _args = Args::parse();
    let _host = host::TuiHost::new();

    terminal::enable_raw_mode()?;
    execute!(stdout(), terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal);

    // Restore terminal regardless of `run`'s outcome.
    let _ = terminal::disable_raw_mode();
    let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen);

    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(area);

            let buffer_panel = Paragraph::new("Phase 1 scaffold — Ctrl+C to exit")
                .block(Block::default().borders(Borders::ALL).title("hjkl"));
            frame.render_widget(buffer_panel, chunks[0]);

            let status = Paragraph::new("-- NORMAL --");
            frame.render_widget(status, chunks[1]);
        })?;

        if let Event::Key(KeyEvent {
            code: KeyCode::Char('c'),
            modifiers,
            ..
        }) = event::read()?
        {
            if modifiers.contains(KeyModifiers::CONTROL) {
                break;
            }
        }
    }

    Ok(())
}
