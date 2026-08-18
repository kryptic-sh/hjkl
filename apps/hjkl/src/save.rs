/// Per-buffer end-of-line state: vim's `'endofline'` plus the zero-line
/// ("empty buffer") bit vim tracks internally as `ML_EMPTY`.
///
/// Both are derived from the bytes a buffer was **loaded** with — never from
/// the buffer content, which cannot tell a 0-byte file from a 1-byte `"\n"`
/// file (hjkl's line model excludes the file-final newline, so both join back
/// to an empty `body`). Keeping the load-time facts is the only way `:w` can
/// round-trip either one.
///
/// The write rule lives in [`EolState::trailing_newline`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EolState {
    /// Vim `'endofline'`: the file this buffer was loaded from ended with a
    /// newline. Queried/set by `:set eol` / `:set noeol` / `:set eol?`.
    ///
    /// nvim leaves `'endofline'` at its default (on) for a 0-byte file — there
    /// are no lines, so nothing sets it off — which is why the default here is
    /// `true` and the 0-byte case is carried by `loaded_empty` instead.
    /// Verified against nvim 0.12: `""`, `"\n"`, `"abc\n"`, `"a\n\n"` all load
    /// with `&eol == 1`; only `"abc"` loads with `&eol == 0`.
    pub endofline: bool,
    /// Vim's `ML_EMPTY`: the buffer was loaded with **no lines at all** — a
    /// 0-byte file, or no file (a new/scratch buffer). Such a buffer writes 0
    /// bytes no matter what `'endofline'`/`'fixendofline'` say.
    ///
    /// Only consulted while the buffer is still empty, so typing into a
    /// 0-byte-loaded buffer needs no explicit invalidation — see
    /// [`EolState::trailing_newline`].
    pub loaded_empty: bool,
}

impl Default for EolState {
    /// A buffer with no file behind it (scratch, `:enew`, or a named file that
    /// doesn't exist yet): `'endofline'` at vim's default (on), and no lines
    /// loaded. Writing it while empty produces a 0-byte file — matching
    /// `nvim -c wq newfile` — while writing it with content appends the final
    /// newline even under `nofixeol`, also matching nvim.
    fn default() -> Self {
        Self {
            endofline: true,
            loaded_empty: true,
        }
    }
}

impl EolState {
    /// Derive the state from the raw bytes just read off disk.
    pub fn from_loaded(content: &str) -> Self {
        Self {
            // A 0-byte file leaves `'endofline'` at its default (on) in vim
            // because it has no last line to be unterminated.
            endofline: content.is_empty() || content.ends_with('\n'),
            loaded_empty: content.is_empty(),
        }
    }

    /// Whether `:w` should append a final `\n` after `body` (the buffer's
    /// lines joined by `\n`, with no terminator).
    ///
    /// The rule, matching nvim 0.12 measured byte-for-byte:
    ///
    /// | loaded file | `body` | `fixeol` | out | `nofixeol` | out |
    /// | --- | --- | --- | --- | --- | --- |
    /// | `""`      | `""`    | — | 0 (`""`)      | — | 0 (`""`)      |
    /// | `"\n"`    | `""`    | — | 1 (`"\n"`)    | — | 1 (`"\n"`)    |
    /// | `"abc"`   | `"abc"` | — | 4 (`"abc\n"`) | — | 3 (`"abc"`)   |
    /// | `"abc\n"` | `"abc"` | — | 4 (`"abc\n"`) | — | 4 (`"abc\n"`) |
    /// | `"a\n\n"` | `"a\n"` | — | 3 (`"a\n\n"`) | — | 3 (`"a\n\n"`) |
    ///
    /// The `loaded_empty && body.is_empty()` gate is vim's `ML_EMPTY` buffer:
    /// zero lines, so zero bytes. It needs no explicit invalidation on edit —
    /// the moment the buffer holds any text, `body` is non-empty and the gate
    /// stops applying, which is why a 0-byte file edited to `"abc"` still
    /// writes `"abc\n"` (nvim agrees).
    ///
    /// **Known divergence (decided, not an oversight).** nvim distinguishes a
    /// buffer *loaded* from `"\n"` (one empty line → writes 1 byte) from one
    /// *emptied by editing* (`dd` on the last line → zero lines → writes 0
    /// bytes). Both are "one empty line" in hjkl: the rope model has no
    /// zero-lines state, and `loaded_empty` deliberately records only what was
    /// on disk. So `"a\n"` + `dd` + `:w` writes 1 byte here where nvim writes
    /// 0. The decision is to match the byte output for files **as loaded** —
    /// the case that silently truncated a `"\n"` file to empty before this
    /// existed — and accept the emptied-by-editing corner.
    pub fn trailing_newline(self, body: &str, fixendofline: bool) -> bool {
        if body.is_empty() && self.loaded_empty {
            return false;
        }
        self.endofline || fixendofline
    }
}

/// Classification of one `:set` token naming vim's `'endofline'`.
///
/// `'endofline'` is buffer-local host state (see [`EolState`]), so hjkl-ex
/// cannot apply it off an `Editor` the way it does every other boolean
/// option. Each host (TUI, `--headless`, embed) pulls these tokens out of the
/// `:set` line itself and applies them to its own [`EolState`]; this parser is
/// the single place the accepted spellings are defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndOfLineToken {
    /// `endofline` / `eol` (`true`), `noendofline` / `noeol` (`false`).
    Set(bool),
    /// `endofline!` / `eol!`.
    Toggle,
    /// `endofline?` / `eol?`.
    Query,
}

/// Recognise a `:set` token naming `'endofline'`. `None` for every other
/// token, which the caller must pass through to hjkl-ex untouched.
pub fn parse_endofline_token(token: &str) -> Option<EndOfLineToken> {
    match token {
        "endofline" | "eol" => Some(EndOfLineToken::Set(true)),
        "noendofline" | "noeol" => Some(EndOfLineToken::Set(false)),
        "endofline!" | "eol!" => Some(EndOfLineToken::Toggle),
        "endofline?" | "eol?" => Some(EndOfLineToken::Query),
        _ => None,
    }
}

/// Apply the `'endofline'` tokens in `tokens` to `eol`, returning the tokens
/// that were NOT consumed (for the caller to forward to hjkl-ex) plus one
/// reply string per `eol?` query, in order.
///
/// Shared by all three save paths so `:set eol` means the same thing in the
/// TUI, `--headless`, and embed mode.
pub fn take_endofline_tokens<'a>(
    tokens: impl Iterator<Item = &'a str>,
    eol: &mut EolState,
) -> (Vec<&'a str>, Vec<String>) {
    let mut rest = Vec::new();
    let mut replies = Vec::new();
    for tok in tokens {
        match parse_endofline_token(tok) {
            Some(EndOfLineToken::Set(v)) => eol.endofline = v,
            Some(EndOfLineToken::Toggle) => eol.endofline = !eol.endofline,
            // `name=on` / `name=off`, matching the shape hjkl-ex's
            // `query_option_value` produces for every other boolean option
            // (vim prints a bare `endofline` / `noendofline` instead).
            Some(EndOfLineToken::Query) => replies.push(format!(
                "endofline={}",
                if eol.endofline { "on" } else { "off" }
            )),
            None => rest.push(tok),
        }
    }
    (rest, replies)
}

/// Save `body` (plus an optional trailing newline) to `path` atomically and
/// durably: write a temp file in the target's own directory, fsync it, then
/// `rename` it over the target (atomic on the same filesystem). A crash or
/// ENOSPC mid-write can no longer leave the target truncated the way the old
/// in-place `File::create` (truncate-then-write) could.
///
/// The mechanism itself lives in [`hjkl_fs`] — the single seam for hjkl's disk
/// I/O — so `:w`, swap, undo and config all share one temp→fsync→rename
/// implementation. What stays here is the policy this call site owns: the RPC
/// filesystem guard, symlink resolution, and the write-permission probe.
///
/// Behavior notes:
/// - Symlinks are preserved: the target is canonicalized first, so the temp
///   file is written next to — and renamed onto — the REAL file; the symlink
///   itself keeps pointing where it did.
/// - The existing file's permission mode is copied onto the new file
///   ([`hjkl_fs::WriteOptions::document`]'s `preserve_mode`).
/// - Hardlinks ARE broken: `rename` replaces the inode, so other links keep
///   the old content. Accepted tradeoff (same as vim's default
///   `backupcopy=auto` rename strategy).
/// - Where temp+rename can't work (unwritable parent dir, cross-device
///   rename, exotic filesystems) `document()` falls back to a non-atomic
///   in-place write so saving never regresses. That fallback uses
///   `File::create` (O_TRUNC), so a mid-write failure **can** leave the
///   target truncated — only the atomic path guarantees no data loss.
pub fn save_file_durable(
    path: &std::path::Path,
    body: &[u8],
    trailing_nl: bool,
    cwd: &std::path::Path,
) -> std::io::Result<()> {
    use std::io::Write;

    fn write_body(f: &mut std::fs::File, body: &[u8], trailing_nl: bool) -> std::io::Result<()> {
        // Two pieces so the trailing newline doesn't force a full-buffer
        // clone just to append a byte.
        f.write_all(body)?;
        if trailing_nl {
            f.write_all(b"\n")?;
        }
        Ok(())
    }

    // In a confined RPC filesystem policy, refuse writes that escape the
    // working directory. Check the caller-supplied `path` *before* canonicalize
    // (which would resolve `..` away). No-op when the policy is off (TUI).
    hjkl_engine::policy::check_fs_path(path)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e))?;

    // Resolve symlinks so we replace the real file, not the link itself.
    // When the FS policy is active, use `resolve_under` which also rejects
    // symlink escapes that a purely lexical check would miss.
    // `canonicalize` fails when the file doesn't exist yet (new file) —
    // write at the given path in that case.
    let target = if hjkl_engine::policy::fs_restricted() {
        match hjkl_fs::resolve_under(cwd, path) {
            Ok(resolved) => resolved,
            Err(e) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("{path}: {e}", path = path.display()),
                ));
            }
        }
    } else {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    };

    // `:w` on a file we lack write permission for must still fail, exactly
    // like the old `File::create` did — the rename trick would otherwise
    // bypass the file's own permission bits. `probe_writable_nofollow` is a
    // single non-truncating write-open (contents and mtime untouched). A single
    // open (rather than a separate `exists()` check followed by an open) closes
    // the TOCTOU gap where the target could be swapped between the two
    // syscalls. `O_NOFOLLOW` (Unix) hardens this further: if the target was
    // swapped with a symlink after canonicalize resolved it, the open fails
    // rather than following the link to an unintended file (security finding
    // M4). `NotFound` is allowed through by the probe — that's a brand-new file.
    hjkl_fs::probe_writable_nofollow(&target)?;

    // temp → fsync → rename → fsync-parent, with mode preservation and the
    // pre-write-only non-atomic fallback, all expressed by `document()`.
    // `write_atomic_with` streams the body so the trailing newline never forces
    // a full-buffer clone; the closure is `Fn` and may run twice (temp-name
    // collision, fallback), so it must stay a pure function of `body`.
    hjkl_fs::write_atomic_with(&target, &hjkl_fs::WriteOptions::document(), |f| {
        write_body(f, body, trailing_nl)
    })
}

/// Serialise the editor's buffer and write it to `path`, honouring
/// `'endofline'`/`'fixendofline'`. Shared by the headless and embed
/// `write_buffer` helpers, whose `Some(p)` arms were byte-identical.
pub fn write_editor_to_file(
    editor: &hjkl_engine::Editor<hjkl_buffer::View, hjkl_engine::types::DefaultHost>,
    path: &std::path::Path,
    eol: EolState,
) -> Result<(), String> {
    let joined = editor.buffer().content_joined();
    // vim `'endofline'`/`'fixendofline'` — see
    // `save::EolState::trailing_newline`.
    let trailing_nl = eol.trailing_newline(&joined, editor.settings().fixendofline);
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    save_file_durable(path, joined.as_bytes(), trailing_nl, &cwd)
        .map_err(|e| format!("hjkl: {}: {e}", path.display()))
}

#[cfg(test)]
mod save_file_durable_tests {
    use super::save_file_durable;

    /// Overwriting an existing file replaces its content and leaves no
    /// stray temp file behind.
    #[test]
    fn overwrites_existing_file_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "old contents\n").unwrap();

        save_file_durable(&p, b"new contents", true, &std::env::current_dir().unwrap()).unwrap();

        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new contents\n");
        // No leftover `.a.txt.hjkl-tmp.*` files.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("hjkl-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file leaked: {leftovers:?}");
    }

    /// Creating a brand-new file (no existing target) works.
    #[test]
    fn creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("fresh.txt");
        save_file_durable(&p, b"hello", true, &std::env::current_dir().unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello\n");
    }

    /// Saving through a symlink must update the file the link points at and
    /// leave the symlink itself intact (NOT replace it with a regular file).
    #[cfg(unix)]
    #[test]
    fn preserves_symlink_and_updates_target() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&real, "old\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        save_file_durable(&link, b"new", true, &std::env::current_dir().unwrap()).unwrap();

        // The link is still a symlink pointing at the same place…
        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink(), "symlink was replaced");
        assert_eq!(std::fs::read_link(&link).unwrap(), real);
        // …and the real file got the new content.
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new\n");
    }

    /// The existing file's permission mode survives the rename replace.
    #[cfg(unix)]
    #[test]
    fn preserves_permission_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("script.sh");
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();

        save_file_durable(
            &p,
            b"#!/bin/sh\necho hi",
            true,
            &std::env::current_dir().unwrap(),
        )
        .unwrap();

        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "permission mode not preserved");
    }

    /// The `O_NOFOLLOW` write probe (security finding M4) must still be in the
    /// path after the move onto the `hjkl-fs` seam.
    ///
    /// A dangling symlink is the observable version of the race the flag
    /// defends against: `canonicalize` can't resolve it, so the probe runs on
    /// the link itself. With `O_NOFOLLOW` the open fails (`ELOOP`) and the save
    /// errors; a plain `open` would instead report `NotFound`, be treated as a
    /// brand-new file, and write *through* the link to whatever it names.
    #[cfg(unix)]
    #[test]
    fn dangling_symlink_target_is_refused_by_nofollow_probe() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&victim, &link).unwrap();

        assert!(
            save_file_durable(&link, b"pwned", true, &std::env::current_dir().unwrap()).is_err(),
            "write followed a symlink — O_NOFOLLOW probe lost"
        );
        assert!(!victim.exists(), "wrote through the symlink to its target");
    }

    /// A read-only target must still make the save fail (old `File::create`
    /// semantics) rather than being silently replaced via rename.
    #[cfg(unix)]
    #[test]
    fn readonly_target_still_errors() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ro.txt");
        std::fs::write(&p, "locked\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o444)).unwrap();

        assert!(save_file_durable(&p, b"nope", true, &std::env::current_dir().unwrap()).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "locked\n");
    }
}

#[cfg(test)]
mod eol_state_tests {
    use super::{EndOfLineToken, EolState, parse_endofline_token, take_endofline_tokens};

    /// One row of the nvim ground-truth tables: the bytes a file was loaded
    /// with, and the bytes `:w` must write back for it, unmodified.
    ///
    /// Measured against nvim 0.12 (`nvim --clean --headless -c wq FILE`, and
    /// the same with `-c 'set nofixeol'`), NOT derived from hjkl's own model.
    /// `apps/hjkl/src/app/tests/ex.rs` and `apps/hjkl/tests/headless.rs` drive
    /// the identical rows through the real save paths.
    struct Row {
        /// Bytes the file held when it was loaded.
        loaded: &'static str,
        /// What nvim writes back with `fixendofline` on (the default).
        fixeol_out: &'static str,
        /// What nvim writes back with `nofixeol`.
        nofixeol_out: &'static str,
    }

    const ROWS: &[Row] = &[
        Row {
            loaded: "",
            fixeol_out: "",
            nofixeol_out: "",
        },
        Row {
            loaded: "\n",
            fixeol_out: "\n",
            nofixeol_out: "\n",
        },
        Row {
            loaded: "abc",
            fixeol_out: "abc\n",
            nofixeol_out: "abc",
        },
        Row {
            loaded: "abc\n",
            fixeol_out: "abc\n",
            nofixeol_out: "abc\n",
        },
        Row {
            loaded: "a\n\n",
            fixeol_out: "a\n\n",
            nofixeol_out: "a\n\n",
        },
    ];

    /// hjkl's line model: load strips at most ONE trailing newline, and the
    /// buffer joins back with no terminator. Mirrors every load site.
    fn body_of(loaded: &str) -> &str {
        loaded.strip_suffix('\n').unwrap_or(loaded)
    }

    fn written(loaded: &str, fixendofline: bool) -> String {
        let body = body_of(loaded);
        let mut out = body.to_string();
        if EolState::from_loaded(loaded).trailing_newline(body, fixendofline) {
            out.push('\n');
        }
        out
    }

    #[test]
    fn reproduces_nvim_fixendofline_table() {
        for row in ROWS {
            let got = written(row.loaded, true);
            assert_eq!(
                got,
                row.fixeol_out,
                "fixendofline: loading {:?} then :w must write {:?} ({} bytes), got {:?}",
                row.loaded,
                row.fixeol_out,
                row.fixeol_out.len(),
                got
            );
        }
    }

    #[test]
    fn reproduces_nvim_nofixendofline_table() {
        for row in ROWS {
            let got = written(row.loaded, false);
            assert_eq!(
                got,
                row.nofixeol_out,
                "nofixeol: loading {:?} then :w must write {:?} ({} bytes), got {:?}",
                row.loaded,
                row.nofixeol_out,
                row.nofixeol_out.len(),
                got
            );
        }
    }

    /// The bug this whole feature exists for: a file holding exactly one
    /// newline used to save as 0 bytes, silently truncating it.
    #[test]
    fn lone_newline_file_is_not_truncated() {
        assert_eq!(written("\n", true), "\n");
        assert_eq!(written("\n", false), "\n");
        // …and a genuinely empty file still writes zero bytes.
        assert_eq!(written("", true), "");
        assert_eq!(written("", false), "");
    }

    /// `'endofline'` mirrors what nvim reports at load (`echo &eol`):
    /// on for `""`, `"\n"`, `"abc\n"`, `"a\n\n"`; off only for `"abc"`.
    #[test]
    fn endofline_matches_nvim_load_flags() {
        for (loaded, want) in [
            ("", true),
            ("\n", true),
            ("abc", false),
            ("abc\n", true),
            ("a\n\n", true),
        ] {
            assert_eq!(
                EolState::from_loaded(loaded).endofline,
                want,
                "&eol after loading {loaded:?}"
            );
        }
    }

    /// A 0-byte file is vim's `ML_EMPTY` buffer: zero lines, zero bytes out
    /// regardless of either option.
    #[test]
    fn zero_byte_file_writes_zero_bytes_under_every_flag_combination() {
        let eol = EolState::from_loaded("");
        for fixendofline in [true, false] {
            for endofline in [true, false] {
                let e = EolState { endofline, ..eol };
                assert!(
                    !e.trailing_newline("", fixendofline),
                    "0-byte file must stay 0 bytes (eol={endofline}, fixeol={fixendofline})"
                );
            }
        }
    }

    /// The `ML_EMPTY` gate must stop applying the moment the buffer holds
    /// text — nvim writes `"abc\n"` after typing into a 0-byte file.
    #[test]
    fn editing_a_zero_byte_file_re_enables_the_trailing_newline() {
        let eol = EolState::from_loaded("");
        assert!(eol.trailing_newline("abc", true));
        // `'endofline'` is on for a 0-byte file, so even `nofixeol` writes it.
        assert!(eol.trailing_newline("abc", false));
    }

    /// A brand-new (nonexistent) file behaves like nvim's new buffer:
    /// empty → 0 bytes, non-empty → final newline even under `nofixeol`.
    #[test]
    fn new_buffer_default_matches_nvim_new_file() {
        let eol = EolState::default();
        assert!(!eol.trailing_newline("", true), "empty new file → 0 bytes");
        assert!(!eol.trailing_newline("", false), "empty new file → 0 bytes");
        assert!(eol.trailing_newline("abc", true));
        assert!(
            eol.trailing_newline("abc", false),
            "new buffers have 'endofline' on, so nofixeol still writes it"
        );
    }

    /// `:set noeol` + `:set nofixeol` strips the final newline; `:set eol`
    /// adds one back. Both verified against nvim.
    #[test]
    fn endofline_override_controls_the_written_newline() {
        let mut eol = EolState::from_loaded("abc\n");
        eol.endofline = false;
        assert!(
            !eol.trailing_newline("abc", false),
            "noeol + nofixeol → abc"
        );
        assert!(eol.trailing_newline("abc", true), "fixeol still adds it");

        let mut eol = EolState::from_loaded("abc");
        eol.endofline = true;
        assert!(
            eol.trailing_newline("abc", false),
            "eol + nofixeol → abc\\n"
        );

        // A `"\n"` file with `noeol nofixeol` writes 0 bytes in nvim: the
        // buffer's single empty line contributes nothing and no EOL is added.
        let mut eol = EolState::from_loaded("\n");
        eol.endofline = false;
        assert!(!eol.trailing_newline("", false));
    }

    /// Documented divergence: hjkl's rope has no zero-lines state, so a
    /// buffer emptied by editing still writes the newline its load-time
    /// `'endofline'` implies (nvim writes 0 bytes). Pinned so the next
    /// reader sees it was a decision, not an oversight.
    #[test]
    fn emptied_by_editing_diverges_from_nvim_by_design() {
        let eol = EolState::from_loaded("a\n");
        assert!(
            eol.trailing_newline("", true),
            "hjkl writes 1 byte here; nvim writes 0 — accepted divergence"
        );
    }

    #[test]
    fn parses_every_endofline_spelling() {
        for name in ["endofline", "eol"] {
            assert_eq!(
                parse_endofline_token(name),
                Some(EndOfLineToken::Set(true)),
                "`:set {name}`"
            );
            assert_eq!(
                parse_endofline_token(&format!("no{name}")),
                Some(EndOfLineToken::Set(false)),
                "`:set no{name}`"
            );
            assert_eq!(
                parse_endofline_token(&format!("{name}!")),
                Some(EndOfLineToken::Toggle),
                "`:set {name}!`"
            );
            assert_eq!(
                parse_endofline_token(&format!("{name}?")),
                Some(EndOfLineToken::Query),
                "`:set {name}?`"
            );
        }
        // Everything else must fall through to hjkl-ex untouched.
        for other in ["number", "fixendofline", "fixeol", "nofixeol", "eolx"] {
            assert_eq!(parse_endofline_token(other), None, "`:set {other}`");
        }
    }

    #[test]
    fn take_tokens_consumes_only_endofline_and_reports_queries() {
        let mut eol = EolState::from_loaded("abc\n");
        let (rest, replies) =
            take_endofline_tokens(["number", "noeol", "fixeol", "eol?"].into_iter(), &mut eol);
        assert_eq!(
            rest,
            vec!["number", "fixeol"],
            "non-eol tokens must flow through to hjkl-ex"
        );
        assert!(!eol.endofline, "`noeol` must clear 'endofline'");
        assert_eq!(replies, vec!["endofline=off".to_string()]);

        let (rest, replies) = take_endofline_tokens(["eol!"].into_iter(), &mut eol);
        assert!(rest.is_empty());
        assert!(replies.is_empty());
        assert!(eol.endofline, "`eol!` must toggle 'endofline' back on");
    }
}
