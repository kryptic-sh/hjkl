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

/// How long to wait for a write (`:w` / `Ctrl+S`) to land on disk. A write is
/// asynchronous relative to the pty keystrokes that triggered it. Override with
/// `E2E_WRITE_MS` on a slow machine.
fn write_timeout() -> Duration {
    let ms = std::env::var("E2E_WRITE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000);
    Duration::from_millis(ms)
}

// ── Waiting on the on-disk buffer ─────────────────────────────────────────────

/// Wait for `path` to read back as `want`. Panics — loudly, as a *timeout* — if
/// it never does.
///
/// Panicking is the whole point. This used to poll for 2s and then silently
/// return whatever it last read, leaving the caller to `assert_eq!` on it. When
/// the editor never wrote (a slow, loaded CI runner), the last read was the
/// content the test had seeded, so the failure surfaced as
/// `assertion left == right failed` with the *unmodified input* as `left` —
/// byte-for-byte identical to what a genuine logic bug would produce. A timing
/// flake was indistinguishable from a regression, and cost real time to tell
/// apart (a rerun of the same commit passed).
///
/// So: a timeout now says it is a timeout, and shows whether the file changed
/// at all while we waited. It deliberately does not guess which kind of failure
/// it is — `last read` is not a computed answer, it is just whatever happened to
/// be on disk when the clock ran out.
/// The "changed while waiting" diagnostic line for a [`wait_for_contents`]
/// timeout, distinguishing the two failure modes: the file changed but never
/// to the expected value, versus it never changed at all (editor never ran).
///
/// Pure — extracted from [`wait_for_contents`] so the reporting can be
/// unit-tested deterministically. The old self-test drove this branch with a
/// real writer thread racing the wait budget, which flaked under parallel
/// load whenever the write landed before `wait_for_contents` snapshotted its
/// first read (making `first == last`). Testing the pure reporter removes the
/// race entirely.
fn timeout_churn(first: &str, last: &str) -> &'static str {
    if last == first {
        "no — the file never changed while we waited, so the editor most likely \
         never processed the keys or never ran its write command"
    } else {
        "yes — the file was written at least once, but never with the expected content"
    }
}

pub fn wait_for_contents(path: &Path, want: &str) -> String {
    let timeout = write_timeout();
    let deadline = std::time::Instant::now() + timeout;
    let first = std::fs::read_to_string(path).unwrap_or_default();
    let mut last = first.clone();
    loop {
        if last == want {
            return last;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
        last = std::fs::read_to_string(path).unwrap_or_default();
    }
    let churn = timeout_churn(&first, &last);
    panic!(
        "TIMED OUT after {timeout:?} waiting for the editor to write the expected content.\n\
         \n  path:      {}\
         \n  expected:  {want:?}\
         \n  last read: {last:?}\
         \n  changed while waiting: {churn}\n\
         \nThis is a harness timeout, NOT a value comparison. Do not read `last read`\n\
         as the editor's answer — if nothing was ever written it is just the seeded\n\
         input, which looks exactly like a wrong result. Re-run before concluding\n\
         this is a regression; raise E2E_WRITE_MS if the machine is slow.",
        path.display(),
    );
}

/// Poll `path` toward `want`, returning whatever it last read when the deadline
/// passes instead of panicking.
///
/// For the few tests whose real assertion is *weaker* than exact equality (the
/// content only has to start with something, or be empty-ish). There `want` is a
/// convergence hint rather than the expected value, so a non-match is not
/// automatically a failure and the caller must do its own asserting.
///
/// Prefer [`wait_for_contents`] everywhere else — it cannot misreport a timeout.
pub fn poll_contents(path: &Path, want: &str) -> String {
    let deadline = std::time::Instant::now() + write_timeout();
    let mut last = std::fs::read_to_string(path).unwrap_or_default();
    while std::time::Instant::now() < deadline {
        if last == want {
            return last;
        }
        std::thread::sleep(Duration::from_millis(25));
        last = std::fs::read_to_string(path).unwrap_or_default();
    }
    last
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
    /// Per-session XDG config dir. Kept alive (and unique per spawn) for the
    /// same reason as `cache_dir`: `hjkl` now writes runtime state back into
    /// `config.toml` (dock resize, and — #63 Phase C — `explorer.open`) via
    /// `hjkl_config::write_key_at`, not just reads it. A single shared
    /// `XDG_CONFIG_HOME` across every e2e test (the old behaviour) would let
    /// one test's explorer-toggle or dock-resize leak into every other pty
    /// test that spawns afterward, since nextest runs this binary's tests as
    /// parallel/sequential processes sharing whatever's on disk.
    #[allow(dead_code)]
    config_dir: tempfile::TempDir,
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
    /// `["--readonly"]`) and the default terminal size (80x24).
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
        // Force the terminal OSC 52 clipboard backend so copy/paste tests use
        // the deterministic register-fallback path on every platform. Without
        // this, macOS spawns the real NSPasteboard, which is a single shared
        // resource — parallel nextest processes contend on it, flaking the
        // copy→paste round-trip.
        cmd.env("HJKL_CLIPBOARD", "osc52");
        cmd.env("TERM", "xterm-256color");
        // Fresh per-session config dir (see the `config_dir` field doc) —
        // `hjkl` writes runtime state (dock resize, explorer.open) back into
        // config.toml, so a shared dir across tests would leak state between
        // unrelated pty processes.
        let config_dir = tempfile::tempdir().expect("e2e config tempdir");
        cmd.env("XDG_CONFIG_HOME", config_dir.path());
        let cache_dir = tempfile::tempdir().expect("e2e cache tempdir");
        cmd.env("XDG_CACHE_HOME", cache_dir.path());
        // Point XDG_STATE_HOME (where the cross-session cursor store lives) at
        // the same per-session tempdir as the cache — different app subdirs, no
        // collision — so real ~/.local/state is never touched by e2e runs.
        cmd.env("XDG_STATE_HOME", cache_dir.path());

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
            config_dir,
        };

        session.wait_ms(spawn_ms());
        session
    }

    /// Spawn `hjkl` opening `file`, using a caller-supplied `cache_dir` as
    /// `XDG_CACHE_HOME` instead of a freshly-generated one.
    ///
    /// The auto-generated per-session cache dir used by the other
    /// constructors isn't known until spawn, which makes it impossible for a
    /// test to pre-seed a swap file at the exact path the spawned process
    /// will look for. Crash-recovery tests need exactly that: write a
    /// `<cache_dir>/hjkl/swap/<hash>.swp` matching `hjkl_app::swap`'s layout
    /// *before* spawning, so the process finds a "newer than disk" swap on
    /// open and surfaces the recovery prompt. Takes ownership of `cache_dir`
    /// so the caller's `TempDir` (and therefore the swap file it wrote) stays
    /// alive for the session's lifetime, same as the auto-generated case.
    #[allow(dead_code)]
    pub fn spawn_with_file_and_cache_dir(path: &Path, cache_dir: tempfile::TempDir) -> Self {
        Self::spawn_inner_cwd_cache(Some(path), 24, 80, None, cache_dir, None, &[], None, None)
    }

    /// Spawn `hjkl` opening `file` with `XDG_CACHE_HOME` pinned to the
    /// caller-owned `cache_home` path (rather than a per-session TempDir), so
    /// several SEQUENTIAL spawns share the swap directory under
    /// `<cache_home>/hjkl/swap/`. The caller keeps `cache_home` alive across
    /// spawns and owns its cleanup. Used by the crash-recovery e2e test: spawn,
    /// edit, kill uncleanly (the swap survives in `cache_home`), respawn to
    /// recover from that same swap.
    #[allow(dead_code)]
    pub fn spawn_with_file_and_cache_home(file: &Path, cache_home: &Path) -> Self {
        let cache_dir = tempfile::tempdir().expect("e2e cache tempdir");
        Self::spawn_inner_cwd_cache(
            Some(file),
            24,
            80,
            None,
            cache_dir,
            None,
            &[],
            None,
            Some(cache_home),
        )
    }

    /// Spawn `hjkl` opening `file` with `XDG_STATE_HOME` pinned to the
    /// caller-owned `state_home`, so several sequential spawns share the
    /// cross-session cursor store under `<state_home>/hjkl/filestate.bin`.
    /// Cache + config stay per-session (fresh, isolated). The caller keeps
    /// `state_home` alive across spawns and owns its cleanup. Used by the
    /// cursor-restore e2e test (move cursor + `:wq`, respawn, assert restore).
    #[allow(dead_code)]
    pub fn spawn_with_file_and_state_home(file: &Path, state_home: &Path) -> Self {
        let cache_dir = tempfile::tempdir().expect("e2e cache tempdir");
        Self::spawn_inner_cwd_cache(
            Some(file),
            24,
            80,
            None,
            cache_dir,
            None,
            &[],
            Some(state_home),
            None,
        )
    }

    /// Spawn `hjkl` opening `file` with a caller-supplied `cache_dir` as
    /// `XDG_CACHE_HOME` (so the caller can inspect `<cache_dir>/hjkl/swap/…`
    /// after the session runs, exactly like [`Self::spawn_with_file_and_cache_dir`]),
    /// plus `extra_args` (e.g. `["-n"]`) before the file — neither existing
    /// constructor combines an inspectable cache dir with extra args.
    #[allow(dead_code)]
    pub fn spawn_with_file_cache_dir_and_args(
        path: &Path,
        cache_dir: tempfile::TempDir,
        extra_args: &[&str],
    ) -> Self {
        Self::spawn_inner_cwd_cache(
            Some(path),
            24,
            80,
            None,
            cache_dir,
            None,
            extra_args,
            None,
            None,
        )
    }

    /// Spawn `hjkl` with `dir` as the cwd, opening `file`, after pre-seeding
    /// a user `config.toml` (at the session's isolated XDG path) with
    /// `config_toml`, and passing `extra_args` (e.g. `["--clean"]`) before
    /// the file. Lets a test assert whether the seeded config took effect.
    #[allow(dead_code)]
    pub fn spawn_in_dir_with_file_config_args(
        dir: &Path,
        file: &Path,
        config_toml: &str,
        extra_args: &[&str],
    ) -> Self {
        let cache_dir = tempfile::tempdir().expect("e2e cache tempdir");
        Self::spawn_inner_cwd_cache(
            Some(file),
            24,
            80,
            Some(dir),
            cache_dir,
            Some(config_toml),
            extra_args,
            None,
            None,
        )
    }

    /// Wide-terminal (24×200) variant of [`Self::spawn_in_dir_with_file`].
    ///
    /// The quickfix dock renders each entry as `<absolute-path>:line:col msg`.
    /// On the macOS CI runner the temp cwd (`/private/var/folders/…`) is long
    /// enough to push the entry past the default 80 columns, truncating the
    /// `:line:col msg` tail so substring assertions fail there but pass on
    /// Linux's short `/tmp/…`. A wide terminal keeps the whole entry on screen
    /// so the assertion no longer depends on cwd length.
    pub fn spawn_in_dir_with_file_wide(dir: &Path, file: &Path) -> Self {
        Self::spawn_inner_cwd(Some(file), 24, 200, Some(dir))
    }

    /// Wide-terminal (24×200) variant of
    /// [`Self::spawn_in_dir_with_file_config_args`]. See
    /// [`Self::spawn_in_dir_with_file_wide`] for why the width matters.
    pub fn spawn_in_dir_with_file_config_args_wide(
        dir: &Path,
        file: &Path,
        config_toml: &str,
        extra_args: &[&str],
    ) -> Self {
        let cache_dir = tempfile::tempdir().expect("e2e cache tempdir");
        Self::spawn_inner_cwd_cache(
            Some(file),
            24,
            200,
            Some(dir),
            cache_dir,
            Some(config_toml),
            extra_args,
            None,
            None,
        )
    }

    fn spawn_inner(file: Option<&Path>, rows: u16, cols: u16) -> Self {
        Self::spawn_inner_cwd(file, rows, cols, None)
    }

    fn spawn_inner_cwd(file: Option<&Path>, rows: u16, cols: u16, cwd: Option<&Path>) -> Self {
        // Isolated, UNIQUE-per-session cache dir so swap files (written on
        // open since #185) never touch the real user cache and never collide
        // across concurrent sessions opening the same fixture (which would
        // trip the live-PID swap lock and open the file read-only). A shared
        // cache dir would also leave fixture swaps behind across runs and
        // surface the recovery prompt. Unique per spawn → fresh + clean.
        let cache_dir = tempfile::tempdir().expect("e2e cache tempdir");
        Self::spawn_inner_cwd_cache(file, rows, cols, cwd, cache_dir, None, &[], None, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_inner_cwd_cache(
        file: Option<&Path>,
        rows: u16,
        cols: u16,
        cwd: Option<&Path>,
        cache_dir: tempfile::TempDir,
        config_toml: Option<&str>,
        extra_args: &[&str],
        state_home: Option<&Path>,
        cache_home: Option<&Path>,
    ) -> Self {
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
        // Force the terminal OSC 52 clipboard backend so copy/paste tests use
        // the deterministic register-fallback path on every platform. Without
        // this, macOS spawns the real NSPasteboard, which is a single shared
        // resource — parallel nextest processes contend on it, flaking the
        // copy→paste round-trip.
        cmd.env("HJKL_CLIPBOARD", "osc52");
        cmd.env("TERM", "xterm-256color");
        // Fresh per-session config dir — see the `config_dir` field doc.
        // Deterministic AND isolated: no real user config leaks in, and no
        // state this session's `hjkl` writes back (dock resize,
        // explorer.open) leaks into any other test's session.
        let config_dir = tempfile::tempdir().expect("e2e config tempdir");
        cmd.env("XDG_CONFIG_HOME", config_dir.path());
        // A caller-supplied `cache_home` (borrowed path) overrides the owned
        // per-session `cache_dir` so several sequential spawns SHARE the swap
        // directory under `<cache_home>/hjkl/swap/` (the crash-recovery e2e
        // test). The owned `cache_dir` still backs the field as a keep-alive.
        cmd.env(
            "XDG_CACHE_HOME",
            cache_home.unwrap_or_else(|| cache_dir.path()),
        );
        // Cross-session cursor store lives under XDG_STATE_HOME. Default it to
        // this session's cache tempdir (distinct app subdir from swap, no
        // clash) so real ~/.local/state is never written by e2e runs. A
        // caller-supplied `state_home` overrides it so several sequential
        // spawns can SHARE the cursor store (the cursor-restore e2e test).
        cmd.env(
            "XDG_STATE_HOME",
            state_home.unwrap_or_else(|| cache_dir.path()),
        );

        // Pre-seed a user config at the XDG path so a `--clean` test can
        // prove the flag actually IGNORES it (and a non-clean control can
        // prove the same file IS read). Written before spawn so it exists
        // when `hjkl` resolves its config at startup.
        if let Some(toml) = config_toml {
            let hjkl_cfg_dir = config_dir.path().join("hjkl");
            std::fs::create_dir_all(&hjkl_cfg_dir).expect("mk config dir");
            std::fs::write(hjkl_cfg_dir.join("config.toml"), toml).expect("seed config.toml");
        }

        if let Some(d) = cwd {
            cmd.cwd(d);
        }

        // Extra CLI flags (e.g. `--clean`) precede the positional file arg,
        // matching how a user would invoke `hjkl --clean file`.
        for arg in extra_args {
            cmd.arg(arg);
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
            config_dir,
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
    ///
    /// A bare Esc (`<Esc>` followed by more keys) is flushed in its own
    /// write with a pacing gap: when an Esc and the byte after it land in
    /// one `read()`, crossterm decodes them as a single Alt+key (dropped
    /// by the app) instead of Esc-then-key. macOS ptys deliver a burst in
    /// one read far more consistently than Linux — this is the mechanism
    /// behind the "`:cmd\r` typed as literal Insert text" flake class that
    /// used to keep several suites linux-only (root-caused on sqeel's
    /// harness port, validated by its macOS CI lane). Escape SEQUENCES
    /// (arrows, `\x1b[A`, SS3 F-keys) must stay in one write — only a
    /// standalone Esc splits.
    pub fn keys(&mut self, seq: &str) {
        let bytes = vim_notation_to_bytes(seq);
        for chunk in split_after_bare_esc(&bytes) {
            self.writer.write_all(chunk).expect("write to pty");
            self.writer.flush().expect("flush pty");
            if chunk.last() == Some(&0x1b) {
                // Give the app's ESC-disambiguation timer room to fire so
                // the next byte can't fuse into an Alt+key.
                std::thread::sleep(Duration::from_millis(60));
            }
        }
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

    /// Send raw bytes to the pty and wait for the screen to settle.
    ///
    /// Use this for byte sequences that cannot be expressed as vim notation
    /// (e.g. CSI-u sequences from the Kitty keyboard protocol).
    #[allow(dead_code)]
    pub fn send_raw(&mut self, bytes: &[u8]) {
        self.writer
            .write_all(bytes)
            .expect("write raw bytes to pty");
        self.writer.flush().expect("flush pty");
        self.wait_ms(settle_ms());
    }

    /// Path to the config file this session's `hjkl` reads/writes —
    /// `$XDG_CONFIG_HOME/hjkl/config.toml` under this session's isolated
    /// `config_dir`. The file may not exist yet if nothing has written to
    /// it (`hjkl` only creates it lazily, on the first `write_key_at` call —
    /// see `hjkl_config::write::write_key_at`).
    #[allow(dead_code)]
    pub fn config_file_path(&self) -> std::path::PathBuf {
        self.config_dir.path().join("hjkl").join("config.toml")
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

    /// Poll up to `timeout_ms` for `needle` to appear on ANY screen row,
    /// returning `true` once it does (or `false` on timeout).
    ///
    /// Use this instead of a bare `(0..rows).any(|r| line(r).contains(..))`
    /// right after a `keys()` that triggers a redraw: `keys()` only waits a
    /// fixed `settle_ms`, which the slower/variable macOS pty redraw can
    /// outlast, so the immediate scan races the paint. Polling removes the
    /// race without slowing the common (already-painted) case.
    pub fn wait_for_screen_contains(&self, needle: &str, timeout_ms: u64) -> bool {
        let steps = (timeout_ms / 20).max(1);
        for _ in 0..steps {
            if (0..self.rows).any(|r| self.line(r).contains(needle)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        (0..self.rows).any(|r| self.line(r).contains(needle))
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

    /// Full screen as a newline-joined, row-numbered string — for failure
    /// diagnostics in assertions.
    #[allow(dead_code)]
    pub fn dump_screen(&self) -> String {
        (0..self.rows)
            .map(|r| format!("{r:>2}|{}", self.line(r)))
            .collect::<Vec<_>>()
            .join("\n")
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

/// Split `bytes` into chunks so every standalone Esc (`0x1b` NOT followed
/// by `[` or `O`, i.e. not a CSI/SS3 escape sequence) ends its chunk. The
/// caller writes chunks separately with a pacing gap after each Esc-final
/// chunk.
fn split_after_bare_esc(bytes: &[u8]) -> Vec<&[u8]> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            let next = bytes.get(i + 1);
            let is_sequence = matches!(next, Some(b'[') | Some(b'O'));
            if !is_sequence && i + 1 < bytes.len() {
                chunks.push(&bytes[start..=i]);
                start = i + 1;
            }
        }
        i += 1;
    }
    if start < bytes.len() {
        chunks.push(&bytes[start..]);
    }
    chunks
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
                // Shift+arrow / Shift+home / Shift+end — ANSI modifier param 2.
                "s-right" => out.extend_from_slice(b"\x1b[1;2C"),
                "s-left" => out.extend_from_slice(b"\x1b[1;2D"),
                "s-up" => out.extend_from_slice(b"\x1b[1;2A"),
                "s-down" => out.extend_from_slice(b"\x1b[1;2B"),
                "s-home" => out.extend_from_slice(b"\x1b[1;2H"),
                "s-end" => out.extend_from_slice(b"\x1b[1;2F"),
                // Function keys F1–F4 (SS3 sequences, xterm-256color).
                // crossterm decodes: ESC O P→F1, Q→F2, R→F3, S→F4.
                "f1" => out.extend_from_slice(b"\x1bOP"),
                "f2" => out.extend_from_slice(b"\x1bOQ"),
                "f3" => out.extend_from_slice(b"\x1bOR"),
                "f4" => out.extend_from_slice(b"\x1bOS"),
                // Shift+F3 — CSI modifier-key sequence: ESC [ 1 ; 2 R
                // crossterm parse_csi_modifier_key_code: final byte 'R' → F(3),
                // modifier-mask 2 → SHIFT.
                "s-f3" => out.extend_from_slice(b"\x1b[1;2R"),
                // Ctrl+Arrow — ANSI CSI modifier sequences (modifier=5 → CONTROL).
                "c-right" => out.extend_from_slice(b"\x1b[1;5C"),
                "c-left" => out.extend_from_slice(b"\x1b[1;5D"),
                "c-up" => out.extend_from_slice(b"\x1b[1;5A"),
                "c-down" => out.extend_from_slice(b"\x1b[1;5B"),
                // Ctrl+Shift+Arrow — modifier=6 → CONTROL|SHIFT.
                "c-s-right" => out.extend_from_slice(b"\x1b[1;6C"),
                "c-s-left" => out.extend_from_slice(b"\x1b[1;6D"),
                // Ctrl+Delete — CSI 3;5~ (modifier=5 → CONTROL, code=3 → Delete).
                "c-del" | "c-delete" => out.extend_from_slice(b"\x1b[3;5~"),
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
    fn bare_esc_splits_chunk() {
        // Esc mid-sequence ends its chunk; remainder is a new chunk.
        let bytes = vim_notation_to_bytes("ifoo<Esc>:w<Enter>");
        let chunks = split_after_bare_esc(&bytes);
        assert_eq!(chunks.len(), 2, "chunks: {chunks:?}");
        assert_eq!(chunks[0].last(), Some(&0x1b));
        assert_eq!(chunks[1], b":w\r");
    }

    #[test]
    fn escape_sequences_stay_whole() {
        // Arrows (CSI) and F-keys (SS3) must NOT split after their Esc.
        let bytes = vim_notation_to_bytes("<Up><F3>x");
        let chunks = split_after_bare_esc(&bytes);
        assert_eq!(chunks.len(), 1, "chunks: {chunks:?}");
    }

    #[test]
    fn trailing_esc_single_chunk() {
        let bytes = vim_notation_to_bytes("ihello<Esc>");
        let chunks = split_after_bare_esc(&bytes);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].last(), Some(&0x1b));
    }

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

    #[test]
    fn notation_function_keys() {
        // F3 → SS3 'R' (xterm application-cursor mode)
        assert_eq!(vim_notation_to_bytes("<F3>"), b"\x1bOR");
        // Shift+F3 → CSI modifier sequence
        assert_eq!(vim_notation_to_bytes("<S-F3>"), b"\x1b[1;2R");
    }

    #[test]
    fn notation_ctrl_arrow_keys() {
        assert_eq!(vim_notation_to_bytes("<C-Right>"), b"\x1b[1;5C");
        assert_eq!(vim_notation_to_bytes("<C-Left>"), b"\x1b[1;5D");
        assert_eq!(vim_notation_to_bytes("<C-S-Right>"), b"\x1b[1;6C");
        assert_eq!(vim_notation_to_bytes("<C-S-Left>"), b"\x1b[1;6D");
        assert_eq!(vim_notation_to_bytes("<C-Delete>"), b"\x1b[3;5~");
    }

    // ── Timeout reporting ────────────────────────────────────────────────────
    //
    // The harness itself is what misreported: a timeout used to come back as a
    // value mismatch, so a slow runner was indistinguishable from a regression.
    // These pin the reporting, not the editor.

    /// Give the file a short write budget so the timeout path runs fast.
    /// Nextest runs each test in its own process, so this cannot leak.
    fn short_write_budget() {
        // 400ms: short enough that the timeout-path self-tests stay fast,
        // long enough that a 2ms writer thread landing inside the budget is
        // certain even under full-suite parallel load (the previous 80ms
        // budget flaked — see wait_for_contents_reports_a_write_that_landed_wrong).
        unsafe { std::env::set_var("E2E_WRITE_MS", "400") };
    }

    #[test]
    fn wait_for_contents_timeout_says_it_timed_out() {
        short_write_budget();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"seeded\n").unwrap();
        f.flush().unwrap();

        let err = std::panic::catch_unwind(|| wait_for_contents(f.path(), "never happens\n"))
            .expect_err("must panic rather than return a non-matching value");
        let msg = err
            .downcast_ref::<String>()
            .expect("panic payload should be a String");

        // The whole point: it must announce itself as a timeout, and must not
        // let the caller mistake the seeded input for the editor's answer.
        assert!(msg.contains("TIMED OUT"), "got: {msg}");
        assert!(msg.contains("NOT a value comparison"), "got: {msg}");
        assert!(
            msg.contains("never changed while we waited"),
            "an untouched file must be reported as never written; got: {msg}"
        );
        assert!(
            msg.contains("\"seeded\\n\""),
            "must show the last read; got: {msg}"
        );
    }

    /// The churn diagnostic must distinguish "file changed but to the wrong
    /// value" from "file never touched" — the two timeout failure modes that
    /// were once indistinguishable. Tested against the pure `timeout_churn`
    /// reporter, so there is NO writer-thread race: the previous version
    /// spawned a 2ms writer against the wait budget and flaked under
    /// full-suite parallel load whenever the write landed BEFORE
    /// `wait_for_contents` snapshotted its first read (making `first == last`,
    /// so churn wrongly reported "never changed"). That was an ordering race
    /// no delay tuning could close; the pure reporter removes it.
    #[test]
    fn timeout_churn_distinguishes_changed_from_untouched() {
        // Changed-but-wrong: first != last.
        assert!(
            super::timeout_churn("seeded\n", "wrong\n")
                .contains("written at least once, but never with the expected content"),
            "a file that changed must be reported as written-but-wrong"
        );
        // Untouched: first == last.
        assert!(
            super::timeout_churn("seeded\n", "seeded\n").contains("never changed"),
            "an untouched file must be reported as never written"
        );
    }

    #[test]
    fn poll_contents_returns_last_read_instead_of_panicking() {
        short_write_budget();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"seeded\n").unwrap();
        f.flush().unwrap();

        // The weaker-assertion escape hatch: no panic, caller gets what is there.
        assert_eq!(poll_contents(f.path(), "never happens\n"), "seeded\n");
    }

    #[test]
    fn wait_for_contents_returns_as_soon_as_it_matches() {
        short_write_budget();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"done\n").unwrap();
        f.flush().unwrap();
        assert_eq!(wait_for_contents(f.path(), "done\n"), "done\n");
    }
}
