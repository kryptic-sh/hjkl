//! `hjkl` — standalone vim-modal terminal editor.
//!
//! Phase 5: polish — readonly mode, +linenum, +/pattern, file-not-found UX,
//! terminal resize, status-line truncation.

mod app;
mod editorconfig;
mod git;
mod host;
mod picker;
mod picker_sources;
mod render;
mod syntax;

use anyhow::Result;
use crossterm::{execute, terminal};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, stdout};

/// Parsed arguments after pre-processing the `+N` / `+/pattern` tokens.
pub struct Args {
    /// Files to open. The first is the active buffer; the rest are loaded into
    /// additional slots in argv order. If empty a new empty buffer is started.
    pub files: Vec<std::path::PathBuf>,
    /// Jump to this 1-based line after load (`+N`).
    pub line: Option<usize>,
    /// Search for this pattern after load (`+/pattern`).
    pub pattern: Option<String>,
    /// Open file in read-only mode (`-R`).
    pub readonly: bool,
    /// Enable the `:perf` overlay at startup (`+perf`).
    pub perf: bool,
    /// Open the file picker at startup (`+picker`).
    pub picker: bool,
}

fn parse_args() -> Result<Args> {
    let raw: Vec<String> = std::env::args().collect();
    let mut line: Option<usize> = None;
    let mut pattern: Option<String> = None;
    let mut readonly = false;
    let mut perf = false;
    let mut picker = false;
    let mut files: Vec<std::path::PathBuf> = Vec::new();
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
            // `+perf` — enable the :perf overlay at startup.
            if let Some(pat) = rest.strip_prefix('/') {
                pattern = Some(pat.to_string());
            } else if let Ok(n) = rest.parse::<usize>() {
                line = Some(n);
            } else if rest == "perf" {
                perf = true;
            } else if rest == "picker" {
                picker = true;
            } else {
                eprintln!("hjkl: ignoring unknown +cmd: {arg}");
            }
        } else if arg.starts_with('-') {
            eprintln!("hjkl: unknown flag: {arg}");
            std::process::exit(1);
        } else {
            files.push(std::path::PathBuf::from(arg));
        }
        i += 1;
    }
    Ok(Args {
        files,
        line,
        pattern,
        readonly,
        perf,
        picker,
    })
}

fn print_help() {
    println!(
        "hjkl {} — vim-modal terminal editor\n\nUSAGE:\n  hjkl [OPTIONS] [FILE]...\n\nOPTIONS:\n  -R, --readonly   Open file read-only\n  +N               Jump to line N on open\n  +/PATTERN        Search for PATTERN on open\n  +perf            Enable :perf overlay at startup\n  +picker          Open the file picker at startup\n  -h, --help       Show this help\n  -V, --version    Print version",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() -> Result<()> {
    let args = parse_args()?;

    // Build app state (may read file from disk) before entering alternate screen
    // so we can print errors to the normal terminal if the file is unreadable.
    let mut app = app::App::new(
        args.files.first().cloned(),
        args.readonly,
        args.line,
        args.pattern,
    )?;
    // Load any additional files into extra slots (argv order). Errors are
    // printed to stderr but do not abort — the editor opens with whatever
    // could be loaded.
    for path in args.files.into_iter().skip(1) {
        if let Err(e) = app.open_extra(path) {
            eprintln!("hjkl: {e}");
        }
    }
    if args.perf {
        app.perf_overlay = true;
    }
    if args.picker {
        app.open_picker();
    }

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
