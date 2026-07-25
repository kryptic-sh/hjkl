//! e2e: `:w` preserves trailing blank lines (audit B2).
//!
//! The TUI save path used to strip a trailing blank line that was present in the
//! on-disk file because the load path strips exactly one trailing newline and
//! the save path then compared `body.ends_with(b"\n")` — so `"x\n\n"` loaded
//! as `"x\n"` and `ends_with` decided no terminator was needed, shrinking the
//! file on a no-edit `:w`. Headless and embed already used the correct condition
//! (`!body.is_empty()`); this brings the TUI path in line.

use super::harness::TerminalSession;
use std::io::{Read, Write};
use std::path::PathBuf;

/// Create a writable temp file seeded with `content` bytes.
fn seed_bytes(content: &[u8]) -> (tempfile::NamedTempFile, PathBuf) {
    let mut f = tempfile::Builder::new()
        .tempfile()
        .expect("create temp file");
    f.write_all(content).expect("seed temp file");
    f.flush().expect("flush temp file");
    let path = f.path().to_owned();
    (f, path)
}

/// Read the entire contents of a file.
fn read_file(path: &std::path::Path) -> Vec<u8> {
    let mut f = std::fs::File::open(path).expect("open file");
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).expect("read file");
    buf
}

/// A file ending with a blank line (`"x\n\n"`) must not lose the trailing
/// newline on a no-edit `:w`.
#[test]
fn save_preserves_trailing_blank_line() {
    let content = b"x\n\n";
    let (_keep, path) = seed_bytes(content);

    {
        let mut s = TerminalSession::spawn_with_file(&path);
        s.keys(":wq<Enter>");
    }

    let on_disk = read_file(&path);
    assert_eq!(
        on_disk, content,
        "no-edit :w must preserve trailing blank line; got {on_disk:?}"
    );
}

/// A file without a trailing blank line (`"a\nb\n"`) must stay intact
/// (regression guard for the unaffected shape).
#[test]
fn save_preserves_file_without_trailing_blank_line() {
    let content = b"a\nb\n";
    let (_keep, path) = seed_bytes(content);

    {
        let mut s = TerminalSession::spawn_with_file(&path);
        s.keys(":wq<Enter>");
    }

    let on_disk = read_file(&path);
    assert_eq!(
        on_disk, content,
        "no-edit :w must preserve file without trailing blank line; got {on_disk:?}"
    );
}
