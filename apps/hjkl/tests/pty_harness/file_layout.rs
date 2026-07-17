//! e2e: `-o` / `-O` / `-p` CLI layout flags lay extra files out as splits /
//! tab pages instead of the default bufferline (nvim compat).
//!
//! `-O b.txt a.txt` opens `b.txt` as the base buffer (window 0, built by
//! `App::new`) and `a.txt` as a vertical split of it — both files' content
//! must be visible side by side in the SAME frame, which is the only thing
//! a single-buffer bufferline (the pre-existing default) could never do.

use super::harness::TerminalSession;
use std::io::Write;
use std::path::PathBuf;

fn seed(content: &str) -> (tempfile::NamedTempFile, PathBuf) {
    let mut f = tempfile::Builder::new()
        .suffix(".txt")
        .tempfile()
        .expect("create temp file");
    f.write_all(content.as_bytes()).expect("seed temp file");
    f.flush().expect("flush temp file");
    let path = f.path().to_owned();
    (f, path)
}

/// Terminal height used by every constructor in this file (`spawn_with_file_and_args`
/// hardcodes 24x80 — see `TerminalSession::spawn_inner_args`). `rows`/`cols` are
/// private fields, so tests use the same literal other harness suites use
/// (e.g. `render_sync.rs::any_line_contains`).
const ROWS: u16 = 24;

/// `true` when any screen row (0-based) contains `needle`, in a single
/// screen snapshot — proves simultaneous visibility, not just eventual.
fn any_line_contains(s: &TerminalSession, needle: &str) -> bool {
    (0..ROWS).any(|row| s.line(row).contains(needle))
}

/// Poll up to ~2s for both markers to appear together in one frame.
fn wait_both_visible(s: &TerminalSession, a: &str, b: &str) -> bool {
    for _ in 0..100 {
        if any_line_contains(s, a) && any_line_contains(s, b) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    any_line_contains(s, a) && any_line_contains(s, b)
}

#[test]
fn dash_o_upper_vsplit_shows_both_files_at_once() {
    let (_keep_base, base_path) = seed("BASE_WINDOW_MARKER\n");
    let (_keep_split, split_path) = seed("SPLIT_WINDOW_MARKER\n");

    // `spawn_with_file_and_args(path, extra_args)` appends `path` AFTER
    // `extra_args`, so this spawns `hjkl -O <base_path> <split_path>`:
    // base_path becomes files[0] (the active buffer, window 0), split_path
    // becomes the one extra file that `-O` lays out as a vertical split.
    let s = TerminalSession::spawn_with_file_and_args(
        &split_path,
        &["-O", base_path.to_str().expect("utf8 tmp path")],
    );

    assert!(
        wait_both_visible(&s, "BASE_WINDOW_MARKER", "SPLIT_WINDOW_MARKER"),
        "-O must open both files as a vertical split, both visible at once — got:\n{}",
        (0..ROWS).map(|r| s.line(r)).collect::<Vec<_>>().join("\n")
    );
}

/// `-p` opens one tab page per file. The tabline (shown once `tabs.len() >
/// 1`) lists `"{n}: {filename}"` per tab, so both filenames must appear on
/// the tabline row even though only one tab's buffer is visible at a time.
#[test]
fn dash_p_tabs_shows_both_filenames_on_tabline() {
    let (_keep_base, base_path) = seed("first file\n");
    let (_keep_extra, extra_path) = seed("second file\n");
    let base_name = base_path
        .file_name()
        .expect("tmp file has a name")
        .to_str()
        .expect("utf8 filename")
        .to_string();
    let extra_name = extra_path
        .file_name()
        .expect("tmp file has a name")
        .to_str()
        .expect("utf8 filename")
        .to_string();

    // `hjkl -p <base_path> <extra_path>`: base_path is files[0] (window 0,
    // first tab); extra_path is the one extra file `-p` opens as a new tab.
    let s = TerminalSession::spawn_with_file_and_args(
        &extra_path,
        &["-p", base_path.to_str().expect("utf8 tmp path")],
    );

    assert!(
        wait_both_visible(&s, &base_name, &extra_name),
        "-p must list both filenames on the tabline — got:\n{}",
        (0..ROWS).map(|r| s.line(r)).collect::<Vec<_>>().join("\n")
    );
}
