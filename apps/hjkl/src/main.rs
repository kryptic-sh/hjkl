//! `hjkl` — standalone vim-modal terminal editor.

// mimalloc as the global Rust allocator. Tree-sitter parsing dominates
// the hot path and is heavily allocation-bound (every subtree node is a
// short-lived `malloc`); mimalloc's segmented free-list outperforms
// glibc's ptmalloc on this exact workload. The TS C core is routed
// through mimalloc separately by `hjkl_bonsai::ensure_mimalloc_allocator()`.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod app;
mod completion;
mod embed;
mod headless;
mod host;
pub(crate) mod hover_popup;
mod keymap_actions;
mod keymap_translate;
pub(crate) mod menu;
mod nvim_api;
mod picker;
mod picker_action;
mod picker_git;
mod picker_sources;
mod render;
mod start_screen;
mod syntax;
mod theme;
mod which_key;

use anyhow::Result;
use clap::Parser;
use crossterm::{event, execute, terminal};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, stdout};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

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
/// natively; vim-style `+N`, `+/pattern`, `+picker` are
/// pre-processed out of `argv` before clap sees it (see [`split_vim_tokens`]).
#[derive(Parser, Debug)]
#[command(
    name = "hjkl",
    version,
    about = "vim-modal terminal editor",
    long_about = LONG_ABOUT,
    after_help = "Vim-style tokens (interspersed with FILEs):\n  +N           jump to 1-based line N on open\n  +/PATTERN    search for PATTERN on open\n  +picker      open the file picker\n  +CMD         run any other text as an ex command (e.g. +vsp, +'vsp other.rs', +set\\ nomouse)",
)]
struct Cli {
    /// Open files read-only.
    #[arg(short = 'R', long)]
    readonly: bool,

    /// Override the user config path (default: $XDG_CONFIG_HOME/hjkl/config.toml).
    /// Bundled defaults are still applied; the file is layered on top.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Run without a terminal: load FILEs, dispatch any +cmd / -c CMD ex
    /// commands in order, write back to disk if a command asks (e.g. +':wq'),
    /// then exit. No ratatui, no crossterm. Useful for scripted edits in CI.
    #[arg(long)]
    headless: bool,

    /// Run without a terminal: speak JSON-RPC 2.0 over stdin/stdout.
    /// Requests: one JSON object per line. Responses: one JSON object per line.
    /// See `docs/embed-rpc.md` for the method catalogue. Implies --headless;
    /// if both are passed it is a no-op. Note: +cmd / -c CMD flags are ignored
    /// in embed mode — commands come over RPC instead.
    #[arg(long)]
    embed: bool,

    /// Run without a terminal: speak msgpack-rpc over stdin/stdout using
    /// nvim-compatible method names. The wire protocol matches the neovim
    /// msgpack-rpc spec so existing nvim-rs clients can drive hjkl unchanged.
    /// See `docs/embed-rpc.md` ("nvim-api mode") for the method catalogue.
    /// Implies headless; +cmd / -c CMD are ignored.
    #[arg(long = "nvim-api")]
    nvim_api: bool,

    /// Ex command to run after loading FILEs (without leading ':'). Repeatable.
    /// All -c commands run first, then all +cmd tokens in argv order. Works
    /// in both TUI and headless mode (e.g. `hjkl -c 'vsp other.rs' main.rs`).
    #[arg(short = 'c', long = "command", value_name = "CMD", action = clap::ArgAction::Append)]
    commands: Vec<String>,

    /// Override the keybinding discipline: `vim` (default, modal) or `vscode`
    /// (non-modal, always in "insert" mode). Overrides `editor.keybindings`
    /// in the user config. Useful for testing without editing the config file.
    #[arg(long, value_name = "MODE")]
    keybindings: Option<String>,

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
    pub picker: bool,
    pub config: Option<PathBuf>,
    /// Run without a terminal (no ratatui/crossterm). Phase 1 of issue #26.
    pub headless: bool,
    /// Run as JSON-RPC 2.0 server over stdin/stdout. Phase 2 of issue #26.
    /// Implies headless; +cmd / -c CMD are ignored in this mode.
    pub embed: bool,
    /// Run as msgpack-rpc server with nvim-compatible methods. Phase 3 of issue #26.
    /// Implies headless; +cmd / -c CMD are ignored in this mode.
    pub nvim_api: bool,
    /// Ex commands to dispatch in headless mode. `-c` commands precede `+cmd`
    /// tokens; argv interleaving within each group is preserved.
    pub commands: Vec<String>,
    /// Optional `--keybindings` override (`"vim"` | `"vscode"`). `None` means
    /// use whatever the config specifies.
    pub keybindings: Option<String>,
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
/// of warnings (reserved — currently always empty). The caller decides how
/// to surface them; `main` prints to stderr.
///
/// Last-write-wins: repeated `+N` overwrites `args.line`; repeated `+/PAT`
/// overwrites `args.pattern`.
///
/// Unrecognised `+<text>` tokens (and `+:<text>` with a leading colon) are
/// treated as ex commands and pushed onto `args.commands` regardless of
/// `args.headless`. TUI dispatch runs them after files are loaded; headless
/// dispatch runs them in the existing scripted-mode path. Errors surface at
/// the ex layer (unknown commands, bad ranges) rather than here, matching
/// vim's `+cmd` behaviour. The leading `:` is stripped so `+vsp` and `+:vsp`
/// work identically.
fn apply_vim_tokens(args: &mut Args, vim_tokens: &[String]) -> Vec<String> {
    let warnings: Vec<String> = Vec::new();
    for tok in vim_tokens {
        let rest = &tok[1..];
        if let Some(pat) = rest.strip_prefix('/') {
            args.pattern = Some(pat.to_string());
        } else if let Ok(n) = rest.parse::<usize>() {
            args.line = Some(n);
        } else if rest == "picker" {
            args.picker = true;
        } else {
            // Strip the optional leading colon so `+:vsp` and `+vsp` both work.
            let cmd = rest.strip_prefix(':').unwrap_or(rest).to_string();
            args.commands.push(cmd);
        }
    }
    warnings
}

/// Parse a raw `argv` (including `argv[0]` binary name) into `(args, warnings)`.
/// Pure function — no env, no stderr — so tests can drive every branch.
///
/// Ordering of ex commands in headless mode: all `-c` flags (from clap) come
/// first, then all `+cmd` tokens in the order they appear in argv. This
/// simplifies the implementation at the cost of strict argv interleaving;
/// document this in your scripts if ordering matters.
fn parse_argv(raw: Vec<String>) -> Result<(Args, Vec<String>)> {
    let (clap_argv, vim_tokens) = split_vim_tokens(raw);
    let cli = Cli::parse_from(clap_argv);
    // Seed headless flag + -c commands before apply_vim_tokens so that the
    // `args.headless` check inside it can route unknown +cmd tokens correctly.
    let mut args = Args {
        files: cli.files,
        line: None,
        pattern: None,
        readonly: cli.readonly,
        picker: false,
        config: cli.config,
        headless: cli.headless || cli.embed || cli.nvim_api,
        embed: cli.embed,
        nvim_api: cli.nvim_api,
        // -c commands come first; +cmd tokens are appended by apply_vim_tokens.
        commands: cli.commands,
        keybindings: cli.keybindings,
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

/// Prepend the anvil `bin/` directory to `$PATH` so any tool installed via
/// `:Anvil install <name>` is visible to LSP spawners and shell invocations.
///
/// # Safety
///
/// `set_var` is `unsafe` in Rust 2024 because it is not thread-safe. This
/// function is called at the very top of `main`, strictly before any threads
/// are spawned (tracing subscriber, LSP manager, etc.), so the invariant is
/// upheld here.
fn prepend_anvil_path() {
    let Ok(bin_dir) = hjkl_anvil::store::bin_dir() else {
        return; // no $HOME — give up silently
    };
    if !bin_dir.exists() {
        let _ = std::fs::create_dir_all(&bin_dir);
    }
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = std::env::split_paths(&existing).collect::<Vec<_>>();
    // Deduplicate: remove any stale prepend from a previous hjkl session.
    entries.retain(|p| p != &bin_dir);
    entries.insert(0, bin_dir);
    if let Ok(joined) = std::env::join_paths(&entries) {
        // SAFETY: single-threaded at this call site — no threads have been
        // spawned yet. The OS process environment is mutable without risk.
        unsafe {
            std::env::set_var("PATH", joined);
        }
    }
}

fn main() -> Result<()> {
    // Prepend the anvil bin dir to PATH before any threads (tracing, LSP) start.
    prepend_anvil_path();

    init_tracing();

    let args = parse_args()?;

    // nvim-api mode (msgpack-rpc server, nvim-compatible) — check FIRST since
    // it is the most specific. +cmd / -c CMD are silently ignored.
    if args.nvim_api {
        let code = nvim_api::run(args.files)?;
        std::process::exit(code);
    }

    // Embed mode (JSON-RPC 2.0 server) — check BEFORE headless since it is
    // more specific. +cmd / -c CMD are silently ignored in this mode.
    if args.embed {
        let code = embed::run(args.files)?;
        std::process::exit(code);
    }

    // Headless script mode — no TUI, no crossterm, no ratatui.
    if args.headless {
        let code = headless::run(args.files, args.commands)?;
        std::process::exit(code);
    }

    // Load user config. `--config <PATH>` reads an explicit file; otherwise
    // we use the XDG path. In both cases the bundled `src/config.toml`
    // defaults are applied first and the user file is deep-merged on top.
    let cfg = match args.config.as_deref() {
        Some(path) => hjkl_app::config::load_from(path)
            .map(|c| (c, hjkl_config::ConfigSource::File(path.to_path_buf()))),
        None => hjkl_app::config::load(),
    };
    let cfg = match cfg {
        Ok((c, _src)) => c,
        Err(e) => {
            eprintln!("hjkl: config error: {e}");
            std::process::exit(2);
        }
    };
    // Bounds-check the parsed config (tab_width range, etc.). Schema-level
    // validation already ran during parse; this catches semantic values that
    // parsed cleanly but would break the editor.
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
    let base_app = app::App::new(
        args.files.first().cloned(),
        args.readonly,
        args.line,
        args.pattern,
    )?
    .with_config(cfg.clone());

    let mut app = if cfg.lsp.enabled {
        let mgr = hjkl_lsp::LspManager::spawn(cfg.lsp.clone());
        base_app.with_lsp(mgr)
    } else {
        base_app
    };
    // `--keybindings` CLI flag overrides the config value.
    if let Some(ref kb) = args.keybindings {
        app.keybinding_mode = hjkl_engine::KeybindingMode::from_config(kb);
        // Re-propagate VSCode-specific per-editor settings now that the mode
        // is finalised (the CLI flag can override the config-derived mode).
        app.propagate_vscode_settings();
    }
    // Load any additional files into extra slots (argv order). Errors are
    // printed to stderr but do not abort — the editor opens with whatever
    // could be loaded.
    for path in args.files.into_iter().skip(1) {
        if let Err(e) = app.open_extra(path) {
            eprintln!("hjkl: {e}");
        }
    }
    if args.picker {
        app.open_picker();
    }
    // Recover any orphan scratch swaps from a previous crashed session.
    // This runs after all CLI files are open so recovered unnamed buffers
    // come last in the slot list; it does NOT run inside App::new so that
    // tests and headless/embed modes are free of real-XDG scanning.
    app.recover_orphan_scratch_buffers();
    // Run any +cmd / -c CMD tokens before entering raw mode. Errors surface
    // as toasts on the notification bus and become visible on the first frame.
    // Matches vim/nvim: `nvim +vsp file.txt` opens the file then runs `:vsp`.
    for cmd in &args.commands {
        app.dispatch_ex(cmd);
        if app.exit_requested {
            // A `+wq` / `+q!` style command requested exit before the loop
            // even runs — honour it without entering the alternate screen.
            return Ok(());
        }
    }

    terminal::enable_raw_mode()?;
    execute!(
        stdout(),
        terminal::EnterAlternateScreen,
        event::EnableFocusChange,
        // Bracketed paste: the terminal wraps pasted text in ESC[200~ … ESC[201~
        // so it arrives as one atomic `Event::Paste(text)` with real newlines.
        // Without this, in raw mode crossterm maps a pasted `\n` (0x0A) to Ctrl+J
        // — which the insert dispatcher drops — so pasted lines bunch together.
        event::EnableBracketedPaste
    )?;
    if app.mouse_enabled {
        execute!(stdout(), event::EnableMouseCapture)?;
    }
    // Push kitty keyboard enhancement (DISAMBIGUATE_ESCAPE_CODES) unconditionally.
    // Non-supporting terminals silently ignore this escape; no blocking query needed.
    let _ = hjkl_kitty::enable(&mut stdout());
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    // Start event-driven autoreload (#242). Best-effort: on failure the
    // poll-based `:checktime` / focus-regain path still autoreloads.
    app.enable_fs_watch();

    let result = app.run(&mut terminal);

    // Restore terminal regardless of outcome. The capture command
    // sequence is idempotent on most terminals; emit unconditionally
    // to recover from a runtime `:set mouse` that may have toggled
    // state since startup.
    let _ = hjkl_kitty::disable(&mut io::stdout());
    let _ = terminal::disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        event::DisableMouseCapture,
        event::DisableFocusChange,
        event::DisableBracketedPaste,
        terminal::LeaveAlternateScreen
    );

    result
}

fn init_tracing() {
    let data_dir = match hjkl_config::data_dir("hjkl") {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("hjkl: tracing disabled (data_dir): {e}");
            return;
        }
    };

    let log_dir = data_dir.join("logs");
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        eprintln!(
            "hjkl: tracing disabled (create log dir {}): {e}",
            log_dir.display()
        );
        return;
    }

    let log_path = log_dir.join("hjkl.log");
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "hjkl: tracing disabled (open log file {}): {e}",
                log_path.display()
            );
            return;
        }
    };

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_ansi(false)
        .with_writer(move || file.try_clone().expect("clone hjkl.log file handle"))
        .finish();

    if let Err(e) = tracing::subscriber::set_global_default(subscriber) {
        eprintln!("hjkl: tracing disabled (set global subscriber): {e}");
    }
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
        let raw: Vec<String> = ["hjkl", "src/main.rs", "+42", "+/foo", "+picker", "-R"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (clap_argv, vim) = split_vim_tokens(raw);
        assert_eq!(clap_argv, vec!["hjkl", "src/main.rs", "-R"]);
        assert_eq!(vim, vec!["+42", "+/foo", "+picker"]);
    }

    /// `+debug` is an unrecognised `+cmd` token → queued as the `:debug` ex
    /// command, run at startup (enables debug mode).
    #[test]
    fn apply_vim_tokens_plus_debug_queues_ex_command() {
        let mut args = blank_args();
        let warnings = apply_vim_tokens(&mut args, &["+debug".into()]);
        assert_eq!(args.commands, vec!["debug".to_string()]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn apply_vim_tokens_sets_line_pattern_picker() {
        let mut args = blank_args();
        let warnings = apply_vim_tokens(
            &mut args,
            &["+42".into(), "+/needle".into(), "+picker".into()],
        );
        assert_eq!(args.line, Some(42));
        assert_eq!(args.pattern.as_deref(), Some("needle"));
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

    /// Unknown `+cmd` tokens are pushed onto `args.commands` for the TUI /
    /// headless dispatcher to run as ex commands (vim/nvim parity). The
    /// leading `:` is stripped, so `+:vsp` and `+vsp` produce the same
    /// command. Errors surface at the ex layer rather than as warnings.
    #[test]
    fn apply_vim_tokens_unknown_token_pushes_ex_command() {
        let mut args = blank_args();
        args.line = Some(7); // pre-existing state must survive
        let warnings = apply_vim_tokens(
            &mut args,
            &["+vsp".into(), "+:wq".into(), "+set nomouse".into()],
        );
        assert!(
            warnings.is_empty(),
            "no warnings expected, got: {warnings:?}"
        );
        assert_eq!(
            args.commands,
            vec![
                "vsp".to_string(),
                "wq".to_string(),
                "set nomouse".to_string()
            ],
            "unknown +cmd tokens should land on args.commands with leading `:` stripped"
        );
        // Pre-existing state untouched.
        assert_eq!(args.line, Some(7));
        assert_eq!(args.pattern, None);
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
    /// together; unknown +cmd tokens land on args.commands for the TUI /
    /// headless dispatcher.
    #[test]
    fn parse_argv_round_trip_mixed_args() {
        let raw: Vec<String> = ["hjkl", "-R", "+42", "src/main.rs", "+/foo", "+vsp"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (args, warnings) = parse_argv(raw).expect("parse_argv");
        assert!(args.readonly);
        assert_eq!(args.line, Some(42));
        assert_eq!(args.pattern.as_deref(), Some("foo"));
        assert_eq!(args.files, vec![PathBuf::from("src/main.rs")]);
        assert!(!args.picker);
        assert!(
            warnings.is_empty(),
            "no warnings expected, got: {warnings:?}"
        );
        assert_eq!(args.commands, vec!["vsp".to_string()]);
    }

    fn blank_args() -> Args {
        Args {
            files: vec![],
            line: None,
            pattern: None,
            readonly: false,
            picker: false,
            config: None,
            headless: false,
            embed: false,
            nvim_api: false,
            commands: vec![],
            keybindings: None,
        }
    }
}
