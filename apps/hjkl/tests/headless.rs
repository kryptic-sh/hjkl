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
