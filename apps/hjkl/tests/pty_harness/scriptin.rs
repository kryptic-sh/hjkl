//! e2e: `-s <scriptin>` replays keystrokes from a file at startup, exactly
//! like `vim -s`. Proves the replay actually runs `:` ex commands (not just
//! Normal-mode motions / Insert text) by observing the scripted `:wq` write
//! the file to disk and exit the process, without a single interactive
//! keystroke sent over the pty.

use super::harness::{TerminalSession, wait_for_contents};
use std::io::Write;
use std::path::PathBuf;

fn seed(content: &str, suffix: &str) -> (tempfile::NamedTempFile, PathBuf) {
    let mut f = tempfile::Builder::new()
        .suffix(suffix)
        .tempfile()
        .expect("create temp file");
    f.write_all(content.as_bytes()).expect("seed temp file");
    f.flush().expect("flush temp file");
    let path = f.path().to_owned();
    (f, path)
}

#[test]
fn dash_s_scriptin_replays_insert_and_wq() {
    let (_keep_file, path) = seed("", ".txt");
    // Raw scriptin bytes: `i` enters Insert, `hello` types text, `\x1b` is
    // Esc back to Normal, `:wq\r` is a scripted ex command that saves and
    // quits — the whole point of routing replay through
    // `App::handle_keypress` instead of the lower-level engine dispatch.
    let (_keep_script, script_path) = seed("ihello\x1b:wq\r", ".hjklscript");

    let script_arg = script_path.display().to_string();
    let _session = TerminalSession::spawn_with_file_and_args(&path, &["-s", &script_arg]);

    let got = wait_for_contents(&path, "hello\n");
    assert_eq!(got, "hello\n");
}
