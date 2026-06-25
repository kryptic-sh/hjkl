//! PTY-based test harness for end-to-end render-sync testing.
//!
//! [`TerminalSession`] spawns the `hjkl` binary under a real pseudo-terminal,
//! feeds keystrokes, and lets you query the rendered screen state via the
//! [`vt100`] parser. This catches the bug class where the engine's internal
//! cursor/viewport state moves but the rendered output visible to the user
//! doesn't follow.
//!
//! # Timing
//!
//! After sending keys we wait for the pty output to stabilise. The default
//! settle timeout is 200 ms; override it with the `E2E_SETTLE_MS` environment
//! variable. The initial spawn wait (waiting for the first frame) is 300 ms
//! by default; override with `E2E_SPAWN_MS`.

use portable_pty::{Child, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem as _};
use std::io::{Read as _, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ── Defaults ─────────────────────────────────────────────────────────────────

fn settle_ms() -> u64 {
    std::env::var("E2E_SETTLE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200)
}

fn spawn_ms() -> u64 {
    std::env::var("E2E_SPAWN_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300)
}

// ── TerminalSession ───────────────────────────────────────────────────────────

/// An active hjkl session running under a real pty.
pub struct TerminalSession {
    /// Master side of the pty (send input / read output). Kept alive so the pty
    /// stays open; reader thread holds a separate clone for reading.
    #[allow(dead_code)]
    master: Box<dyn MasterPty + Send>,
    /// Writer to the master — kept separately so we can write and read concurrently.
    writer: Box<dyn Write + Send>,
    /// Child process handle (for kill-on-drop).
    child: Box<dyn Child + Send + Sync>,
    /// Shared vt100 parser state updated by the reader thread.
    parser: Arc<Mutex<vt100::Parser>>,
    /// Terminal height in rows (used for screen iteration bounds).
    #[allow(dead_code)]
    rows: u16,
    /// Terminal width in columns.
    cols: u16,
    /// Per-session XDG cache dir. Kept alive so swap files written by the
    /// spawned hjkl land in an isolated, auto-cleaned dir — NOT the real
    /// user cache. Without this, write-on-open swaps for the shared fixtures
    /// survive the kill-on-drop (no graceful `:q`) and the next open hits the
    /// crash-recovery prompt, swallowing the test's keystrokes (#185).
    #[allow(dead_code)]
    cache_dir: tempfile::TempDir,
}

impl TerminalSession {
    // ── Constructors ─────────────────────────────────────────────────────────

    /// Spawn `hjkl` with no file argument and the default terminal size (80x24).
    #[allow(dead_code)]
    pub fn spawn() -> Self {
        Self::spawn_inner(None, 24, 80)
    }

    /// Spawn `hjkl` with a file argument and the default terminal size (80x24).
    pub fn spawn_with_file(path: &Path) -> Self {
        Self::spawn_inner(Some(path), 24, 80)
    }

    /// Spawn `hjkl` with a file argument and an explicit terminal size.
    #[allow(dead_code)]
    pub fn spawn_with_size(rows: u16, cols: u16) -> Self {
        Self::spawn_inner(None, rows, cols)
    }

    /// Spawn `hjkl` with no file argument but with the working directory set to
    /// `dir`. Used by explorer tests: the explorer roots at the process cwd, so
    /// this controls the tree shown by `<leader>e`.
    #[allow(dead_code)]
    pub fn spawn_in_dir(dir: &Path) -> Self {
        Self::spawn_inner_cwd(None, 24, 80, Some(dir))
    }

    /// Spawn `hjkl` opening `file` with the working directory set to `dir`.
    /// Event-driven autoreload (#242) roots its watcher at the process cwd, so
    /// the watched file must live under `dir` for the watch to fire.
    #[allow(dead_code)]
    pub fn spawn_in_dir_with_file(dir: &Path, file: &Path) -> Self {
        Self::spawn_inner_cwd(Some(file), 24, 80, Some(dir))
    }

    /// Spawn `hjkl` with a file argument plus extra CLI arguments (e.g.
    /// `["--keybindings", "vscode"]`) and the default terminal size (80x24).
    #[allow(dead_code)]
    pub fn spawn_with_file_and_args(path: &Path, extra_args: &[&str]) -> Self {
        Self::spawn_inner_args(Some(path), 24, 80, extra_args)
    }

    fn spawn_inner_args(file: Option<&Path>, rows: u16, cols: u16, extra_args: &[&str]) -> Self {
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        let hjkl_bin = env!("CARGO_BIN_EXE_hjkl");
        let mut cmd = CommandBuilder::new(hjkl_bin);
        cmd.env("HJKL_LOG", "off");
        cmd.env("TERM", "xterm-256color");
        cmd.env(
            "XDG_CONFIG_HOME",
            std::env::temp_dir().join("hjkl-e2e-config"),
        );
        let cache_dir = tempfile::tempdir().expect("e2e cache tempdir");
        cmd.env("XDG_CACHE_HOME", cache_dir.path());

        for arg in extra_args {
            cmd.arg(arg);
        }
        if let Some(p) = file {
            cmd.arg(p);
        }

        let child = pair.slave.spawn_command(cmd).expect("spawn hjkl");

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));

        let parser_clone = Arc::clone(&parser);
        let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut p = parser_clone.lock().unwrap();
                        p.process(&buf[..n]);
                    }
                }
            }
        });

        let writer = pair.master.take_writer().expect("take pty writer");

        let session = Self {
            master: pair.master,
            writer,
            child,
            parser,
            rows,
            cols,
            cache_dir,
        };

        session.wait_ms(spawn_ms());
        session
    }

    fn spawn_inner(file: Option<&Path>, rows: u16, cols: u16) -> Self {
        Self::spawn_inner_cwd(file, rows, cols, None)
    }

    fn spawn_inner_cwd(file: Option<&Path>, rows: u16, cols: u16, cwd: Option<&Path>) -> Self {
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        let hjkl_bin = env!("CARGO_BIN_EXE_hjkl");
        let mut cmd = CommandBuilder::new(hjkl_bin);
        // Suppress logging noise in test output.
        cmd.env("HJKL_LOG", "off");
        cmd.env("TERM", "xterm-256color");
        // Deterministic config: use an empty tmp dir so no user config leaks in.
        cmd.env(
            "XDG_CONFIG_HOME",
            std::env::temp_dir().join("hjkl-e2e-config"),
        );
        // Isolated, UNIQUE-per-session cache dir so swap files (written on
        // open since #185) never touch the real user cache and never collide
        // across concurrent sessions opening the same fixture (which would
        // trip the live-PID swap lock and open the file read-only). A shared
        // cache dir would also leave fixture swaps behind across runs and
        // surface the recovery prompt. Unique per spawn → fresh + clean.
        let cache_dir = tempfile::tempdir().expect("e2e cache tempdir");
        cmd.env("XDG_CACHE_HOME", cache_dir.path());

        if let Some(d) = cwd {
            cmd.cwd(d);
        }

        if let Some(p) = file {
            cmd.arg(p);
        }

        let child = pair.slave.spawn_command(cmd).expect("spawn hjkl");

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));

        // Spawn a reader thread that pipes pty output into the vt100 parser.
        let parser_clone = Arc::clone(&parser);
        let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut p = parser_clone.lock().unwrap();
                        p.process(&buf[..n]);
                    }
                }
            }
        });

        let writer = pair.master.take_writer().expect("take pty writer");

        let session = Self {
            master: pair.master,
            writer,
            child,
            parser,
            rows,
            cols,
            cache_dir,
        };

        // Wait for the first frame to appear.
        session.wait_ms(spawn_ms());

        session
    }

    // ── Input ─────────────────────────────────────────────────────────────────

    /// Send a vim-notation key sequence and wait for the screen to settle.
    ///
    /// Accepted notation: bare characters, `<Esc>`, `<Enter>`, `<Tab>`,
    /// `<Backspace>`, `<Space>`, `<C-x>` (ctrl-x), `<C-w>j` (ctrl-w then j),
    /// digit repetitions like `30j` (sends `3`, `0`, `j` as separate keys).
    pub fn keys(&mut self, seq: &str) {
        let bytes = vim_notation_to_bytes(seq);
        self.writer.write_all(&bytes).expect("write to pty");
        self.writer.flush().expect("flush pty");
        self.wait_ms(settle_ms());
    }

    /// Send `text` as a terminal **bracketed paste**: wrap it in the
    /// `ESC[200~` … `ESC[201~` markers exactly as a real terminal does when the
    /// user presses Ctrl+Shift+V. The spawned `hjkl` (which enables bracketed
    /// paste) decodes this into a single `Event::Paste(text)`. `text` is sent
    /// verbatim — embedded newlines are preserved, which is the whole point.
    #[allow(dead_code)]
    pub fn paste(&mut self, text: &str) {
        self.writer
            .write_all(b"\x1b[200~")
            .expect("write paste start");
        self.writer
            .write_all(text.as_bytes())
            .expect("write paste body");
        self.writer
            .write_all(b"\x1b[201~")
            .expect("write paste end");
        self.writer.flush().expect("flush pty");
        self.wait_ms(settle_ms());
    }

    // ── Screen queries ────────────────────────────────────────────────────────

    /// Snapshot the current screen state.
    pub fn screen(&self) -> vt100::Screen {
        self.parser.lock().unwrap().screen().clone()
    }

    /// 0-based (row, col) of the SOFTWARE cursor — the cell the editor paints
    /// as the cursor (block = reversed cell, bar = `▏` glyph). The editor no
    /// longer drives the terminal's hardware cursor (it trailed during scroll),
    /// so `cursor()` is stale for editor windows; tests that need the editor
    /// cursor position use this. Returns the first matching cell scanning
    /// top-to-bottom, left-to-right. `None` when no software cursor is on screen.
    pub fn cursor_cell(&self) -> Option<(u16, u16)> {
        let screen = self.screen();
        for row in 0..self.rows {
            for col in 0..self.cols {
                if let Some(cell) = screen.cell(row, col)
                    && (cell.inverse() || cell.contents() == "▏")
                {
                    return Some((row, col));
                }
            }
        }
        None
    }

    /// Like [`Self::cursor_cell`] but polls until the software cursor appears,
    /// up to ~2s. The PTY harness drives the real binary asynchronously, so a
    /// single read right after `keys()` can race the cursor-bearing frame
    /// (especially on loaded CI). Panics if the cursor never renders.
    pub fn cursor_cell_wait(&self) -> (u16, u16) {
        for _ in 0..200 {
            if let Some(pos) = self.cursor_cell() {
                return pos;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("software cursor never rendered within 2s");
    }

    /// Poll until row `row` contains `expected`, up to `timeout_ms` (20 ms
    /// granularity). Returns `true` as soon as it appears, `false` on timeout.
    /// Used by event-driven tests that wait on an async external change with no
    /// keypress to settle on (the standard `keys()` settle doesn't apply).
    #[allow(dead_code)]
    pub fn wait_for_line(&self, row: u16, expected: &str, timeout_ms: u64) -> bool {
        let steps = (timeout_ms / 20).max(1);
        for _ in 0..steps {
            if self.line(row).contains(expected) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.line(row).contains(expected)
    }

    /// Foreground color of the first cell on `row` whose rendered content equals
    /// `symbol`. Returns `None` if no such cell is present. Used to assert a
    /// glyph (e.g. a tabline devicon) is painted with a specific color.
    #[allow(dead_code)]
    pub fn cell_fg_of_symbol(&self, row: u16, symbol: &str) -> Option<vt100::Color> {
        let screen = self.screen();
        for col in 0..self.cols {
            if let Some(cell) = screen.cell(row, col)
                && cell.contents() == symbol
            {
                return Some(cell.fgcolor());
            }
        }
        None
    }

    /// Poll up to `timeout_ms` for a cell on `row` rendering `symbol`, returning
    /// its foreground color once it appears (or `None` on timeout).
    #[allow(dead_code)]
    pub fn wait_cell_fg_of_symbol(
        &self,
        row: u16,
        symbol: &str,
        timeout_ms: u64,
    ) -> Option<vt100::Color> {
        let steps = (timeout_ms / 20).max(1);
        for _ in 0..steps {
            if let Some(c) = self.cell_fg_of_symbol(row, symbol) {
                return Some(c);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.cell_fg_of_symbol(row, symbol)
    }

    /// Rendered text of a 0-based screen row (trailing spaces stripped).
    pub fn line(&self, row: u16) -> String {
        let screen = self.screen();
        let mut s = String::new();
        for col in 0..self.cols {
            let cell = screen.cell(row, col);
            let ch = cell.map(|c| c.contents()).unwrap_or("");
            if ch.is_empty() {
                s.push(' ');
            } else {
                s.push_str(ch);
            }
        }
        s.trim_end().to_string()
    }

    // ── Internals ─────────────────────────────────────────────────────────────

    fn wait_ms(&self, ms: u64) {
        // Simple sleep: the reader thread keeps the parser updated continuously,
        // so after sleeping the parser reflects whatever the binary emitted.
        std::thread::sleep(Duration::from_millis(ms));
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // Best-effort kill — process may already be gone.
        let _ = self.child.kill();
    }
}

// ── Key translation ───────────────────────────────────────────────────────────

/// Translate a vim-style notation string to raw bytes suitable for pty input.
///
/// Supports:
/// - Bare printable characters (sent as-is, including digits for counts like `30j`)
/// - `<Esc>` → `\x1b`
/// - `<Enter>` / `<CR>` / `<Return>` → `\r`
/// - `<Tab>` → `\t`
/// - `<Backspace>` / `<BS>` → `\x7f`
/// - `<Space>` → ` `
/// - `<C-x>` → ctrl byte (x & 0x1f)
/// - `<C-w>j` → ctrl-w followed by `j`
/// - `<Up>` / `<Down>` / `<Left>` / `<Right>` → ANSI escape sequences
fn vim_notation_to_bytes(seq: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chars = seq.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '<' {
            // Collect tag content.
            let mut tag = String::new();
            let mut closed = false;
            for next in chars.by_ref() {
                if next == '>' {
                    closed = true;
                    break;
                }
                tag.push(next);
            }
            if !closed {
                // Malformed — emit literally.
                out.push(b'<');
                out.extend_from_slice(tag.as_bytes());
                continue;
            }
            let lower = tag.to_ascii_lowercase();
            match lower.as_str() {
                "esc" | "escape" => out.push(0x1b),
                "enter" | "cr" | "return" => out.push(b'\r'),
                "tab" => out.push(b'\t'),
                "bs" | "backspace" => out.push(0x7f),
                "space" => out.push(b' '),
                "up" => out.extend_from_slice(b"\x1b[A"),
                "down" => out.extend_from_slice(b"\x1b[B"),
                "right" => out.extend_from_slice(b"\x1b[C"),
                "left" => out.extend_from_slice(b"\x1b[D"),
                "home" => out.extend_from_slice(b"\x1b[H"),
                "end" => out.extend_from_slice(b"\x1b[F"),
                "pageup" => out.extend_from_slice(b"\x1b[5~"),
                "pagedown" => out.extend_from_slice(b"\x1b[6~"),
                "del" | "delete" => out.extend_from_slice(b"\x1b[3~"),
                _ => {
                    // Modifier combos: C-x, S-x, C-w (then remainder after tag).
                    if let Some(bytes) = parse_modifier_tag(&tag) {
                        out.extend_from_slice(&bytes);
                    } else {
                        // Unknown tag: emit raw.
                        out.push(b'<');
                        out.extend_from_slice(tag.as_bytes());
                        out.push(b'>');
                    }
                }
            }
        } else {
            // Bare character — encode as UTF-8.
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        }
    }

    out
}

/// Parse a modifier-combo tag like `C-w`, `C-x`, `S-F1`, etc.
/// Returns `None` for unrecognised tags.
fn parse_modifier_tag(tag: &str) -> Option<Vec<u8>> {
    let parts: Vec<&str> = tag.split('-').collect();
    if parts.len() < 2 {
        return None;
    }

    let mut i = 0;
    let mut ctrl = false;
    let mut _shift = false;
    let mut _alt = false;

    while i < parts.len() - 1 {
        match parts[i].to_ascii_uppercase().as_str() {
            "C" => {
                ctrl = true;
                i += 1;
            }
            "S" => {
                _shift = true;
                i += 1;
            }
            "A" | "M" => {
                _alt = true;
                i += 1;
            }
            _ => break,
        }
    }

    let key = parts[i..].join("-");
    let mut bytes = Vec::new();

    if ctrl {
        // Ctrl-x: single byte (x & 0x1f).
        if key.len() == 1 {
            let c = key.chars().next().unwrap();
            bytes.push((c as u8) & 0x1f);
        } else {
            // Ctrl-named-key (e.g. C-Enter) — best effort.
            let lower = key.to_ascii_lowercase();
            match lower.as_str() {
                "enter" | "cr" => bytes.push(b'\r'),
                "tab" => bytes.push(b'\t'),
                _ => return None,
            }
        }
    } else {
        return None;
    }

    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notation_simple_keys() {
        assert_eq!(vim_notation_to_bytes("abc"), b"abc");
        assert_eq!(vim_notation_to_bytes("<Esc>"), &[0x1b]);
        assert_eq!(vim_notation_to_bytes("<Enter>"), b"\r");
        assert_eq!(vim_notation_to_bytes("<CR>"), b"\r");
        assert_eq!(vim_notation_to_bytes("<BS>"), &[0x7f]);
        assert_eq!(vim_notation_to_bytes("<Tab>"), b"\t");
        assert_eq!(vim_notation_to_bytes("<Space>"), b" ");
    }

    #[test]
    fn notation_ctrl_keys() {
        assert_eq!(vim_notation_to_bytes("<C-w>"), &[0x17]); // ctrl-w
        assert_eq!(vim_notation_to_bytes("<C-u>"), &[0x15]); // ctrl-u
        assert_eq!(vim_notation_to_bytes("<C-d>"), &[0x04]); // ctrl-d
    }

    #[test]
    fn notation_composite() {
        // ":100<Enter>" → colon, 1, 0, 0, CR
        let b = vim_notation_to_bytes(":100<Enter>");
        assert_eq!(b, b":100\r");
        // "30j" → three bare bytes
        assert_eq!(vim_notation_to_bytes("30j"), b"30j");
        // ctrl-w then j
        assert_eq!(vim_notation_to_bytes("<C-w>j"), &[0x17, b'j']);
    }
}
