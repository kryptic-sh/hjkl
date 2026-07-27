//! Integration tests for `hjkl --headless` script mode.
//!
//! Uses `env!("CARGO_BIN_EXE_hjkl")` to locate the compiled binary and
//! `tempfile` (already a dev-dependency) for scratch files.

use std::io::Write as _;
use std::process::Command;

fn hjkl() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hjkl"))
}

/// Substitute all occurrences and write-quit. File must be updated.
#[test]
fn headless_substitute_writes_back() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(b"foo bar foo\n").unwrap();
    let path = f.path().to_path_buf();

    let status = hjkl()
        .args(["--headless", "+:%s/foo/baz/g", "+:wq"])
        .arg(&path)
        .status()
        .expect("hjkl binary");

    assert!(status.success(), "exit code: {status}");
    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(contents, "baz bar baz\n", "file not updated: {contents:?}");
}

/// Substitute without an explicit write — file must remain unchanged.
#[test]
fn headless_no_write_without_explicit_save() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(b"foo bar foo\n").unwrap();
    let path = f.path().to_path_buf();

    let status = hjkl()
        .args(["--headless", "+:%s/foo/baz/g"])
        .arg(&path)
        .status()
        .expect("hjkl binary");

    assert!(status.success(), "exit code: {status}");
    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        contents, "foo bar foo\n",
        "file was unexpectedly modified: {contents:?}"
    );
}

/// An unknown ex command must set exit code 1 and print to stderr.
#[test]
fn headless_unknown_command_sets_exit_1() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(b"hello\n").unwrap();
    let path = f.path().to_path_buf();

    let output = hjkl()
        .args(["--headless", "+:doesnotexist", "+:q"])
        .arg(&path)
        .output()
        .expect("hjkl binary");

    assert_eq!(output.status.code(), Some(1), "expected exit 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("doesnotexist"),
        "stderr should mention the bad command; got: {stderr:?}"
    );
}

/// -c and + tokens: all -c commands run first, then all + tokens.
/// With the implemented ordering, `:%s/a/b/g` (via -c) runs before
/// `:%s/b/c/g` (via +), so the final content should be "c\n".
#[test]
fn headless_dash_c_and_plus_interleave() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(b"a\n").unwrap();
    let path = f.path().to_path_buf();

    let status = hjkl()
        .args(["--headless", "-c", ":%s/a/b/g", "+:%s/b/c/g", "+:wq"])
        .arg(&path)
        .status()
        .expect("hjkl binary");

    assert!(status.success(), "exit code: {status}");
    let contents = std::fs::read_to_string(&path).unwrap();
    // -c runs first (a→b), then +:%s/b/c/g (b→c).
    assert_eq!(contents, "c\n", "unexpected contents: {contents:?}");
}

// ── vim `'endofline'` / `'fixendofline'` parity ──────────────────────────────
//
// Ground truth measured against nvim 0.12 (`nvim --clean --headless -c wq
// FILE`, and the same with `-c 'set nofixeol'`). Same rows as
// `save::eol_state_tests` and the app-layer `:w` tests, driven here through
// the real `--headless` binary.

/// Write `input` to a temp file, run it through `--headless` with `commands`,
/// and return the exact bytes left on disk.
fn headless_eol_round_trip(input: &[u8], commands: &[&str]) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("eol_case.txt");
    std::fs::write(&path, input).unwrap();

    let mut cmd = hjkl();
    cmd.arg("--headless");
    for c in commands {
        cmd.args(["-c", c]);
    }
    let status = cmd.arg(&path).status().expect("hjkl binary");
    assert!(status.success(), "exit code: {status}");
    std::fs::read(&path).unwrap()
}

#[test]
fn headless_write_matches_nvim_with_fixendofline_on() {
    for (input, want) in [
        ("", ""),
        ("\n", "\n"),
        ("abc", "abc\n"),
        ("abc\n", "abc\n"),
        ("a\n\n", "a\n\n"),
    ] {
        let got = headless_eol_round_trip(input.as_bytes(), &[":wq"]);
        assert_eq!(
            got,
            want.as_bytes(),
            "fixeol: {input:?} must save as {want:?}, got {:?}",
            String::from_utf8_lossy(&got)
        );
    }
}

#[test]
fn headless_write_matches_nvim_with_nofixendofline() {
    for (input, want) in [
        ("", ""),
        ("\n", "\n"),
        ("abc", "abc"),
        ("abc\n", "abc\n"),
        ("a\n\n", "a\n\n"),
    ] {
        let got = headless_eol_round_trip(input.as_bytes(), &[":set nofixeol", ":wq"]);
        assert_eq!(
            got,
            want.as_bytes(),
            "nofixeol: {input:?} must save as {want:?}, got {:?}",
            String::from_utf8_lossy(&got)
        );
    }
}

/// `:set noeol` is honoured in headless mode too (the token is intercepted
/// before hjkl-ex, exactly like in the TUI).
#[test]
fn headless_set_noeol_strips_the_trailing_newline() {
    assert_eq!(
        headless_eol_round_trip(b"abc\n", &[":set noeol nofixeol", ":wq"]),
        b"abc"
    );
}

/// --headless with no files and no commands: warn to stderr, exit 0.
#[test]
fn headless_no_files_exits_clean() {
    let output = hjkl().arg("--headless").output().expect("hjkl binary");

    assert!(output.status.success(), "exit code: {}", output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no commands or files"),
        "expected warning on stderr; got: {stderr:?}"
    );
}
