//! `hjkl` — standalone vim-modal terminal editor.

mod app;
mod editorconfig;
mod git;
mod host;
mod lang;
mod picker;
mod picker_sources;
mod render;
mod syntax;
mod theme;

use anyhow::Result;
use clap::Parser;
use crossterm::{execute, terminal};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, stdout};
use std::path::PathBuf;

/// ASCII-art banner. Regenerate with:
///
/// ```sh
/// figlet -f "ANSI Regular" hjkl > apps/hjkl/src/art.txt
/// ```
const LONG_ABOUT: &str = concat!(
    "\n",
    include_str!("art.txt"),
    "\nvim-modal terminal editor · v",
    env!("CARGO_PKG_VERSION"),
);

/// Pre-flight CLI surface. clap handles `--help` / `--version` / `-R`
/// natively; vim-style `+N`, `+/pattern`, `+perf`, `+picker` are
/// pre-processed out of `argv` before clap sees it (see [`split_vim_tokens`]).
#[derive(Parser, Debug)]
#[command(
    name = "hjkl",
    version,
    about = "vim-modal terminal editor",
    long_about = LONG_ABOUT,
    after_help = "Vim-style tokens (interspersed with FILEs):\n  +N           jump to 1-based line N on open\n  +/PATTERN    search for PATTERN on open\n  +perf        enable the :perf overlay\n  +picker      open the file picker",
)]
struct Cli {
    /// Open files read-only.
    #[arg(short = 'R', long)]
    readonly: bool,

    /// Files to open. First is the active buffer; the rest are loaded into
    /// additional slots in argv order. If empty, a fresh buffer is started.
    files: Vec<PathBuf>,
}

/// Parsed arguments after pre-processing the `+N` / `+/pattern` tokens.
pub struct Args {
    pub files: Vec<PathBuf>,
    pub line: Option<usize>,
    pub pattern: Option<String>,
    pub readonly: bool,
    pub perf: bool,
    pub picker: bool,
}

/// Split raw `argv` into (tokens-clap-handles, vim-style-`+`-prefixed-tokens).
/// Preserves the binary name in the clap stream so clap's prog detection
/// stays correct.
fn split_vim_tokens(raw: Vec<String>) -> (Vec<String>, Vec<String>) {
    let mut clap_args: Vec<String> = Vec::with_capacity(raw.len());
    let mut vim_tokens: Vec<String> = Vec::new();
    for (i, arg) in raw.into_iter().enumerate() {
        if i > 0 && arg.starts_with('+') && arg.len() > 1 {
            vim_tokens.push(arg);
        } else {
            clap_args.push(arg);
        }
    }
    (clap_args, vim_tokens)
}

/// Apply parsed `+`-prefixed tokens onto an `Args` builder. Unknown
/// `+` tokens are warned to stderr (matches the legacy parser behavior).
fn apply_vim_tokens(args: &mut Args, vim_tokens: &[String]) {
    for tok in vim_tokens {
        let rest = &tok[1..];
        if let Some(pat) = rest.strip_prefix('/') {
            args.pattern = Some(pat.to_string());
        } else if let Ok(n) = rest.parse::<usize>() {
            args.line = Some(n);
        } else if rest == "perf" {
            args.perf = true;
        } else if rest == "picker" {
            args.picker = true;
        } else {
            eprintln!("hjkl: ignoring unknown +cmd: {tok}");
        }
    }
}

fn parse_args() -> Result<Args> {
    let raw: Vec<String> = std::env::args().collect();
    let (clap_argv, vim_tokens) = split_vim_tokens(raw);
    let cli = Cli::parse_from(clap_argv);
    let mut args = Args {
        files: cli.files,
        line: None,
        pattern: None,
        readonly: cli.readonly,
        perf: false,
        picker: false,
    };
    apply_vim_tokens(&mut args, &vim_tokens);
    Ok(args)
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

#[cfg(test)]
mod cli_tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn version_flag_returns_pkg_version() {
        let cmd = Cli::command();
        let version = cmd.render_version();
        assert!(
            version.contains(env!("CARGO_PKG_VERSION")),
            "render_version output {version:?} missing CARGO_PKG_VERSION"
        );
    }

    #[test]
    fn long_help_contains_ascii_art() {
        let mut cmd = Cli::command();
        let help = cmd.render_long_help().to_string();
        assert!(
            help.contains(include_str!("art.txt")),
            "long_help missing embedded art.txt block; got:\n{help}"
        );
    }

    #[test]
    fn long_help_contains_pkg_version() {
        let mut cmd = Cli::command();
        let help = cmd.render_long_help().to_string();
        assert!(
            help.contains(env!("CARGO_PKG_VERSION")),
            "long_help missing CARGO_PKG_VERSION; got:\n{help}"
        );
    }

    #[test]
    fn split_vim_tokens_separates_plus_args() {
        let raw: Vec<String> = ["hjkl", "src/main.rs", "+42", "+/foo", "+perf", "-R"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (clap_argv, vim) = split_vim_tokens(raw);
        assert_eq!(clap_argv, vec!["hjkl", "src/main.rs", "-R"]);
        assert_eq!(vim, vec!["+42", "+/foo", "+perf"]);
    }

    #[test]
    fn apply_vim_tokens_sets_line_pattern_perf_picker() {
        let mut args = Args {
            files: vec![],
            line: None,
            pattern: None,
            readonly: false,
            perf: false,
            picker: false,
        };
        apply_vim_tokens(
            &mut args,
            &[
                "+42".into(),
                "+/needle".into(),
                "+perf".into(),
                "+picker".into(),
            ],
        );
        assert_eq!(args.line, Some(42));
        assert_eq!(args.pattern.as_deref(), Some("needle"));
        assert!(args.perf);
        assert!(args.picker);
    }
}
