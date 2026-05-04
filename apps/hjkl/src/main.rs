//! `hjkl` — standalone vim-modal terminal editor.

mod app;
mod config;
mod editorconfig;
mod git;
mod host;
mod lang;
mod picker;
mod picker_git;
mod picker_sources;
mod render;
mod start_screen;
mod syntax;
mod theme;

use anyhow::Result;
use clap::Parser;
use crossterm::{event, execute, terminal};
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

    /// Override the user config path (default: $XDG_CONFIG_HOME/hjkl/config.toml).
    /// Bundled defaults are still applied; the file is layered on top.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

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
    pub config: Option<PathBuf>,
}

/// Split raw `argv` into (tokens-clap-handles, vim-style-`+`-prefixed-tokens).
/// Preserves the binary name in the clap stream so clap's prog detection
/// stays correct.
///
/// Conventions:
/// - The argv\[0\] (binary name) is never treated as a vim token.
/// - A bare `+` (length 1) is *not* a vim token — it falls through to clap
///   as a positional (treated as a literal filename `+`).
/// - `--` ends vim-token processing: everything after stays in the clap
///   stream verbatim, matching POSIX end-of-options semantics. This lets
///   `hjkl -- +42` open a file literally named `+42`.
fn split_vim_tokens(raw: Vec<String>) -> (Vec<String>, Vec<String>) {
    let mut clap_args: Vec<String> = Vec::with_capacity(raw.len());
    let mut vim_tokens: Vec<String> = Vec::new();
    let mut after_dashdash = false;
    for (i, arg) in raw.into_iter().enumerate() {
        if !after_dashdash && i > 0 && arg == "--" {
            after_dashdash = true;
            clap_args.push(arg);
            continue;
        }
        if !after_dashdash && i > 0 && arg.starts_with('+') && arg.len() > 1 {
            vim_tokens.push(arg);
        } else {
            clap_args.push(arg);
        }
    }
    (clap_args, vim_tokens)
}

/// Apply parsed `+`-prefixed tokens onto an `Args` builder. Returns a list
/// of warnings (currently: unknown `+cmd` tokens). The caller decides how
/// to surface them — `main` prints to stderr, tests assert the contents.
///
/// Last-write-wins: repeated `+N` overwrites `args.line`; repeated `+/PAT`
/// overwrites `args.pattern`.
fn apply_vim_tokens(args: &mut Args, vim_tokens: &[String]) -> Vec<String> {
    let mut warnings: Vec<String> = Vec::new();
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
            warnings.push(format!("hjkl: ignoring unknown +cmd: {tok}"));
        }
    }
    warnings
}

/// Parse a raw `argv` (including `argv[0]` binary name) into `(args, warnings)`.
/// Pure function — no env, no stderr — so tests can drive every branch.
fn parse_argv(raw: Vec<String>) -> Result<(Args, Vec<String>)> {
    let (clap_argv, vim_tokens) = split_vim_tokens(raw);
    let cli = Cli::parse_from(clap_argv);
    let mut args = Args {
        files: cli.files,
        line: None,
        pattern: None,
        readonly: cli.readonly,
        perf: false,
        picker: false,
        config: cli.config,
    };
    let warnings = apply_vim_tokens(&mut args, &vim_tokens);
    Ok((args, warnings))
}

fn parse_args() -> Result<Args> {
    let raw: Vec<String> = std::env::args().collect();
    let (args, warnings) = parse_argv(raw)?;
    for w in warnings {
        eprintln!("{w}");
    }
    Ok(args)
}

fn main() -> Result<()> {
    let args = parse_args()?;

    // Load user config. `--config <PATH>` reads an explicit file; otherwise
    // we use the XDG path. In both cases the bundled `src/config.toml`
    // defaults are applied first and the user file is deep-merged on top.
    let cfg = match args.config.as_deref() {
        Some(path) => config::load_from(path)
            .map(|c| (c, hjkl_config::ConfigSource::File(path.to_path_buf()))),
        None => config::load(),
    };
    let cfg = match cfg {
        Ok((c, _src)) => c,
        Err(e) => {
            eprintln!("hjkl: config error: {e}");
            std::process::exit(2);
        }
    };
    // Bounds-check the parsed config (tab_width range, huge_file_threshold > 0).
    // Schema-level validation already ran during parse; this catches semantic
    // values that parsed cleanly but would break the editor.
    {
        use hjkl_config::Validate;
        if let Err(e) = cfg.validate() {
            eprintln!("hjkl: config validation: {e}");
            std::process::exit(2);
        }
    }
    // Theme validation: only "dark" is bundled today. Warn (don't fail) on
    // unknown names so the editor still starts with the dark palette.
    if cfg.theme.name != "dark" {
        eprintln!(
            "hjkl: warning: theme.name = {:?} is not bundled; falling back to \"dark\"",
            cfg.theme.name
        );
    }

    // Build app state (may read file from disk) before entering alternate screen
    // so we can print errors to the normal terminal if the file is unreadable.
    let mut app = app::App::new(
        args.files.first().cloned(),
        args.readonly,
        args.line,
        args.pattern,
    )?
    .with_config(cfg);
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
    execute!(
        stdout(),
        terminal::EnterAlternateScreen,
        event::EnableFocusChange
    )?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = app.run(&mut terminal);

    // Restore terminal regardless of outcome.
    let _ = terminal::disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        event::DisableFocusChange,
        terminal::LeaveAlternateScreen
    );

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
    fn long_help_advertises_config_flag() {
        let mut cmd = Cli::command();
        let help = cmd.render_long_help().to_string();
        assert!(
            help.contains("--config"),
            "long_help should advertise --config; got:\n{help}"
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
        let mut args = blank_args();
        let warnings = apply_vim_tokens(
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
        assert!(warnings.is_empty());
    }

    /// A bare `+` (length 1) is not a vim token — it survives into the
    /// clap stream as a positional, equivalent to opening a file literally
    /// named `+`. Vim has the same behavior.
    #[test]
    fn split_vim_tokens_passes_bare_plus_to_clap() {
        let raw: Vec<String> = ["hjkl", "+", "file.txt"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (clap_argv, vim) = split_vim_tokens(raw);
        assert_eq!(clap_argv, vec!["hjkl", "+", "file.txt"]);
        assert!(vim.is_empty());
    }

    /// `--` ends vim-token processing. Anything after — including
    /// `+`-prefixed tokens — is passed to clap verbatim. This lets users
    /// open a file literally named `+42` via `hjkl -- +42`.
    #[test]
    fn split_vim_tokens_honors_dashdash_separator() {
        let raw: Vec<String> = ["hjkl", "+10", "--", "+42", "+/notapattern"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (clap_argv, vim) = split_vim_tokens(raw);
        assert_eq!(clap_argv, vec!["hjkl", "--", "+42", "+/notapattern"]);
        assert_eq!(vim, vec!["+10"]);
    }

    /// Repeated `+N` / `+/PAT` overwrite — last write wins. Matches vim's
    /// behavior when both `+10` and `+20` are given.
    #[test]
    fn apply_vim_tokens_last_write_wins() {
        let mut args = blank_args();
        let _ = apply_vim_tokens(
            &mut args,
            &[
                "+10".into(),
                "+20".into(),
                "+/first".into(),
                "+/second".into(),
            ],
        );
        assert_eq!(args.line, Some(20));
        assert_eq!(args.pattern.as_deref(), Some("second"));
    }

    /// Unknown `+cmd` tokens produce a warning string (returned, not
    /// printed). Other state on `args` is left untouched.
    #[test]
    fn apply_vim_tokens_unknown_token_produces_warning() {
        let mut args = blank_args();
        args.line = Some(7); // pre-existing state must survive
        let warnings = apply_vim_tokens(&mut args, &["+bogus".into(), "+also-bogus".into()]);
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("+bogus"), "got: {:?}", warnings[0]);
        assert!(
            warnings[1].contains("+also-bogus"),
            "got: {:?}",
            warnings[1]
        );
        // Pre-existing state untouched.
        assert_eq!(args.line, Some(7));
        assert_eq!(args.pattern, None);
        assert!(!args.perf);
        assert!(!args.picker);
    }

    /// `+/` with empty pattern currently sets `pattern = Some("")`. This
    /// is a documented quirk of the current implementation — a downstream
    /// search-engine layer will treat the empty pattern as a no-op. If the
    /// behavior changes, this test pins the new contract.
    #[test]
    fn apply_vim_tokens_empty_pattern_is_some_empty_string() {
        let mut args = blank_args();
        let _ = apply_vim_tokens(&mut args, &["+/".into()]);
        assert_eq!(args.pattern.as_deref(), Some(""));
    }

    /// End-to-end `parse_argv`: clap-handled flags + vim tokens work
    /// together; warnings are returned in order.
    #[test]
    fn parse_argv_round_trip_mixed_args() {
        let raw: Vec<String> = ["hjkl", "-R", "+42", "src/main.rs", "+/foo", "+xyzzy"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (args, warnings) = parse_argv(raw).expect("parse_argv");
        assert!(args.readonly);
        assert_eq!(args.line, Some(42));
        assert_eq!(args.pattern.as_deref(), Some("foo"));
        assert_eq!(args.files, vec![PathBuf::from("src/main.rs")]);
        assert!(!args.perf);
        assert!(!args.picker);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("+xyzzy"));
    }

    fn blank_args() -> Args {
        Args {
            files: vec![],
            line: None,
            pattern: None,
            readonly: false,
            perf: false,
            picker: false,
            config: None,
        }
    }
}
