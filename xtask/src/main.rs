//! `cargo xtask <subcmd>` — build automation for hjkl-bonsai.
//!
//! Subcommands:
//!   sync-bonsai    Regenerate ../bonsai.toml from upstream sources.

use std::env;
use std::process::ExitCode;

mod sync_bonsai;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let cmd = match args.next() {
        Some(c) => c,
        None => {
            eprintln!("usage: cargo xtask <subcommand>");
            eprintln!("subcommands:");
            eprintln!("  sync-bonsai    regenerate ../bonsai.toml");
            return ExitCode::from(2);
        }
    };
    let rest: Vec<String> = args.collect();

    let result = match cmd.as_str() {
        "sync-bonsai" => sync_bonsai::run(&rest),
        other => {
            eprintln!("unknown subcommand: {other}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
