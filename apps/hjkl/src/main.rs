//! `hjkl` — standalone vim-modal terminal editor.
//!
//! Phase 5: polish — readonly mode, +linenum, +/pattern, file-not-found UX,
//! terminal resize, status-line truncation.

mod app;
mod host;
mod render;

use anyhow::Result;
use crossterm::{execute, terminal};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, stdout};

/// Parsed arguments after pre-processing the `+N` / `+/pattern` tokens.
pub struct Args {
    /// File to open. If the file does not exist a new empty buffer is started.
    pub file: Option<std::path::PathBuf>,
    /// Jump to this 1-based line after load (`+N`).
    pub line: Option<usize>,
    /// Search for this pattern after load (`+/pattern`).
    pub pattern: Option<String>,
    /// Open file in read-only mode (`-R`).
    pub readonly: bool,
}

fn parse_args() -> Result<Args> {
    let raw: Vec<String> = std::env::args().collect();
    let mut line: Option<usize> = None;
    let mut pattern: Option<String> = None;
    let mut readonly = false;
    let mut file: Option<std::path::PathBuf> = None;
    let mut i = 1usize;
    while i < raw.len() {
        let arg = &raw[i];
        if arg == "-R" || arg == "--readonly" {
            readonly = true;
        } else if arg == "--version" || arg == "-V" {
            println!("hjkl {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        } else if arg == "--help" || arg == "-h" {
            print_help();
            std::process::exit(0);
        } else if let Some(rest) = arg.strip_prefix('+') {
            // `+N` — jump to line N.
            // `+/pattern` — search for pattern.
            if let Some(pat) = rest.strip_prefix('/') {
                pattern = Some(pat.to_string());
            } else if let Ok(n) = rest.parse::<usize>() {
                line = Some(n);
            } else {
                eprintln!("hjkl: ignoring unknown +cmd: {arg}");
            }
        } else if arg.starts_with('-') {
            eprintln!("hjkl: unknown flag: {arg}");
            std::process::exit(1);
        } else {
            file = Some(std::path::PathBuf::from(arg));
        }
        i += 1;
    }
    Ok(Args {
        file,
        line,
        pattern,
        readonly,
    })
}

fn print_help() {
    println!(
        "hjkl {} — vim-modal terminal editor\n\nUSAGE:\n  hjkl [OPTIONS] [FILE]\n\nOPTIONS:\n  -R, --readonly   Open file read-only\n  +N               Jump to line N on open\n  +/PATTERN        Search for PATTERN on open\n  -h, --help       Show this help\n  -V, --version    Print version",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() -> Result<()> {
    let args = parse_args()?;

    // Build app state (may read file from disk) before entering alternate screen
    // so we can print errors to the normal terminal if the file is unreadable.
    let mut app = app::App::new(args.file, args.readonly, args.line, args.pattern)?;

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
