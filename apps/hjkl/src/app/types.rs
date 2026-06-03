//! Shared types: per-mode mouse flags, disk state, LSP structs, and buffer slot.

use std::hash::Hasher;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use hjkl_buffer::Buffer;
use hjkl_buffer_tui::Sign;
use hjkl_engine::{Editor, VimMode};

use crate::host::TuiHost;
use crate::syntax::BufferId;

/// Per-mode mouse-enable flags — mirrors Vim's `:set mouse=<flags>`.
///
/// Default (all enabled) corresponds to `mouse=a`.  Set individual fields to
/// `false` to disable mouse in that mode.  The event loop checks
/// [`App::mouse_flags`] via [`mouse_enabled_for`] before processing any mouse
/// event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseFlags {
    /// Mouse active in Normal mode (`n`).
    pub normal: bool,
    /// Mouse active in Visual / VisualLine / VisualBlock mode (`v`).
    pub visual: bool,
    /// Mouse active in Insert mode (`i`).
    pub insert: bool,
    /// Mouse active in Command-line / prompt mode (`c`).
    pub command: bool,
    /// Mouse active in Help buffers (`h`). Parsed for compatibility but unused today.
    pub help: bool,
}

impl MouseFlags {
    /// All modes enabled — equivalent to `mouse=a`.
    pub fn all() -> Self {
        Self {
            normal: true,
            visual: true,
            insert: true,
            command: true,
            help: true,
        }
    }

    /// All modes disabled — equivalent to `set nomouse` / `mouse=`.
    pub fn none() -> Self {
        Self {
            normal: false,
            visual: false,
            insert: false,
            command: false,
            help: false,
        }
    }

    /// Parse a Vim-style flags string (`"a"`, `"nvi"`, `""`, …).
    ///
    /// - `"a"` → all modes on.
    /// - Each char `n/v/i/c/h` enables the corresponding mode.
    /// - Unknown chars are silently ignored (forward-compat).
    /// - Empty string → all modes off.
    pub fn from_flags(s: &str) -> Self {
        if s == "a" {
            return Self::all();
        }
        let mut f = Self::none();
        for c in s.chars() {
            match c {
                'n' => f.normal = true,
                'v' => f.visual = true,
                'i' => f.insert = true,
                'c' => f.command = true,
                'h' => f.help = true,
                'a' => {
                    // 'a' anywhere in string still means all.
                    return Self::all();
                }
                _ => {}
            }
        }
        f
    }

    /// Return a canonical flags string suitable for `:set mouse?` display.
    pub fn as_flags_str(&self) -> String {
        if self.normal && self.visual && self.insert && self.command && self.help {
            return "a".into();
        }
        let mut s = String::new();
        if self.normal {
            s.push('n');
        }
        if self.visual {
            s.push('v');
        }
        if self.insert {
            s.push('i');
        }
        if self.command {
            s.push('c');
        }
        if self.help {
            s.push('h');
        }
        s
    }
}

impl Default for MouseFlags {
    fn default() -> Self {
        Self::all()
    }
}

/// Return `true` when mouse events should be processed for the given Vim mode.
///
/// Used by the event loop at the top of `Event::Mouse` to gate events by mode.
/// Extracted as a pure function so it can be unit-tested without a running App.
pub fn mouse_enabled_for(mode: VimMode, flags: &MouseFlags) -> bool {
    match mode {
        VimMode::Normal => flags.normal,
        VimMode::Visual | VimMode::VisualLine | VimMode::VisualBlock => flags.visual,
        VimMode::Insert => flags.insert,
    }
}

/// Whether the on-disk file is in sync with what was last loaded/saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskState {
    /// File matches what we loaded/saved last.
    Synced,
    /// File changed on disk since last load/save (and buffer is dirty — no auto-reload).
    ChangedOnDisk,
    /// File no longer exists on disk.
    DeletedOnDisk,
}

/// LSP diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagSeverity {
    Error = 1,
    Warning = 2,
    Info = 3,
    Hint = 4,
}

/// A single LSP diagnostic stored on a `BufferSlot`.
#[derive(Debug, Clone)]
pub struct LspDiag {
    /// 0-based start row.
    pub start_row: usize,
    /// 0-based start char-column.
    pub start_col: usize,
    /// 0-based end row.
    pub end_row: usize,
    /// 0-based end char-column.
    pub end_col: usize,
    pub severity: DiagSeverity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
}

/// Snapshot of a running LSP server's state, tracked by the app.
pub struct LspServerInfo {
    pub initialized: bool,
    pub capabilities: serde_json::Value,
}

/// Tracks an in-flight LSP request so the response handler knows what to do.
/// Each variant carries the buffer context and cursor origin so the result can
/// be acted on (jump, picker, popup) without re-reading app state at response
/// time (the active buffer may have changed by then).
#[derive(Debug, Clone)]
pub enum LspPendingRequest {
    GotoDefinition {
        buffer_id: hjkl_lsp::BufferId,
        /// 0-based (row, col) of the cursor when the request was sent.
        origin: (usize, usize),
    },
    GotoDeclaration {
        buffer_id: hjkl_lsp::BufferId,
        origin: (usize, usize),
    },
    GotoTypeDefinition {
        buffer_id: hjkl_lsp::BufferId,
        origin: (usize, usize),
    },
    GotoImplementation {
        buffer_id: hjkl_lsp::BufferId,
        origin: (usize, usize),
    },
    GotoReferences {
        buffer_id: hjkl_lsp::BufferId,
        origin: (usize, usize),
    },
    Hover {
        buffer_id: hjkl_lsp::BufferId,
        origin: (usize, usize),
    },
    /// Mouse-hover variant of `Hover` — result goes to the floating
    /// [`HoverPopup`] instead of `info_popup`. Phase 5 mouse support.
    HoverAtMouse {
        buffer_id: hjkl_lsp::BufferId,
        origin: (usize, usize),
    },
    Completion {
        buffer_id: hjkl_lsp::BufferId,
        /// 0-based cursor position when the request was sent.
        anchor_row: usize,
        anchor_col: usize,
        /// `true` when fired implicitly as the user types (identifier / trigger
        /// char), `false` when invoked manually (`<C-n>`/`<C-p>`). Auto requests
        /// stay silent when the server returns nothing.
        auto: bool,
    },
    /// `textDocument/codeAction` — Phase 5.
    CodeAction {
        buffer_id: hjkl_lsp::BufferId,
        anchor_row: usize,
        anchor_col: usize,
    },
    /// `textDocument/rename` — Phase 5.
    Rename {
        buffer_id: hjkl_lsp::BufferId,
        anchor_row: usize,
        anchor_col: usize,
        new_name: String,
    },
    /// `textDocument/formatting` — Phase 5.
    Format {
        buffer_id: hjkl_lsp::BufferId,
        /// `None` = full document; `Some((sr, sc, er, ec))` = range (Phase 5 always None).
        range: Option<(usize, usize, usize, usize)>,
    },
}

/// Hash + byte-length of the buffer's canonical line content (lines
/// joined by `\n` — same shape as what `:w` writes, modulo the trailing
/// newline). Used to detect "buffer matches the saved snapshot" so undo
/// back to the saved state clears the dirty flag.
///
/// Uses `ahash` rather than `std::DefaultHasher` (SipHash-1-3) — SipHash
/// is overkill for collision detection on local content and ~5–10× slower
/// than `ahash` on multi-MB inputs. Profile on a busy edit run showed
/// ~10 % of per-keystroke self time inside `SipHasher::write`; ahash
/// brings that to ~1–2 %.
fn buffer_signature(editor: &Editor<Buffer, TuiHost>) -> (u64, usize) {
    // Stream the rope chunks straight into ahash — no full-document
    // `Arc<String>` materialization. `Buffer::rope()` is an O(1) Arc-clone.
    let rope = editor.buffer().rope();
    let mut hasher = ahash::AHasher::default();
    let mut len = 0usize;
    for chunk in rope.chunks() {
        let bytes = chunk.as_bytes();
        hasher.write(bytes);
        len += bytes.len();
    }
    (hasher.finish(), len)
}

/// Per-buffer feature switches. Lets special buffers (e.g. the file explorer
/// scratch buffer) opt out of editor features they don't want. All on by default.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BufferFeatures {
    /// Tree-sitter syntax highlighting.
    pub syntax: bool,
    /// LSP attach + hover/diagnostics for this buffer.
    pub lsp: bool,
    /// Hover popups (mouse-idle hover and `K`).
    pub hover: bool,
}
impl Default for BufferFeatures {
    fn default() -> Self {
        Self {
            syntax: true,
            lsp: true,
            hover: true,
        }
    }
}

/// Per-buffer state. Phase B: App holds `Vec<BufferSlot>` + `active: usize`.
/// Phase C will add bnext / bdelete / switch-or-create.
///
/// After v0.22.0 the per-window [`Editor`] lives on [`AppWindow`] rather than
/// here. `BufferSlot` continues to hold one editor for backward compatibility
/// with LSP / syntax / save paths that need buffer content from a slot — they
/// use `slot.editor` when no specific window is in scope. Focused-window
/// operations go through `App::active_window().editor` / `App::active_window_mut().editor`.
pub struct BufferSlot {
    /// Stable id used to multiplex the SyntaxLayer / Worker.
    pub buffer_id: BufferId,
    /// `true` when this slot backs the explorer buffer window (#55).
    /// Drives: key interception, buffer-cycle exclusion, gutterless render.
    pub(crate) is_explorer: bool,
    /// Per-buffer feature opt-outs. Default: all enabled.
    pub(crate) features: BufferFeatures,
    /// The slot-level editor. Holds buffer content, undo stack, syntax spans,
    /// LSP attachment. The focused window's [`AppWindow::editor`] is the
    /// source of truth for cursor and scroll during dispatch; after each key
    /// dispatch `sync_slot_from_window` writes cursor/scroll back here so
    /// slot-level operations (`:w`, dirty-check, LSP didChange) see the
    /// current buffer state.
    pub editor: Editor<Buffer, TuiHost>,
    /// File path shown in status line and used for `:w` saves.
    pub filename: Option<PathBuf>,
    /// Persistent dirty flag. Set when `editor.take_dirty()` returns `true`;
    /// cleared after a successful `:w` save.
    pub dirty: bool,
    /// True when a file was requested but not found on disk — shows
    /// "[New File]" annotation in the status line until the first edit
    /// or successful `:w`.
    pub is_new_file: bool,
    /// `true` when the current file is in a git repo but not in HEAD —
    /// drives the `[Untracked]` status-line tag. Refreshed alongside
    /// `git_signs`.
    pub is_untracked: bool,
    /// Diagnostic gutter signs (tree-sitter ERROR / MISSING) for the
    /// current viewport. Refreshed by `recompute_and_install`; read by
    /// `render::buffer_pane`.
    pub diag_signs: Vec<Sign>,
    /// LSP diagnostic gutter signs. Separate from `diag_signs` so the
    /// oracle/syntax source can be cleared independently of LSP.
    pub diag_signs_lsp: Vec<Sign>,
    /// Full LSP diagnostic list for the buffer. Replaced wholesale each
    /// time `textDocument/publishDiagnostics` arrives (server is source
    /// of truth — empty notification clears all diags).
    pub lsp_diags: Vec<LspDiag>,
    /// `dirty_gen` of the buffer the last time we sent `textDocument/didChange`
    /// to the LSP. `None` = never sent.
    pub(crate) last_lsp_dirty_gen: Option<u64>,
    /// Git diff signs (`+` / `~` / `_`) against HEAD. Recomputed
    /// whenever the buffer's `dirty_gen` advances so unsaved edits
    /// show in the gutter live. Filtered to the viewport per-frame
    /// in the renderer.
    pub git_signs: Vec<Sign>,
    /// `dirty_gen` of the buffer when `git_signs` was last rebuilt.
    /// `None` = stale, force recompute on next render.
    pub(super) last_git_dirty_gen: Option<u64>,
    /// Wall-clock time of the last successful git_signs refresh — used
    /// to throttle the libgit2 diff to ~4 Hz during active typing on
    /// large files.
    pub(super) last_git_refresh_at: Instant,
    /// Hash + byte-length of the buffer content as it was at the most
    /// recent save (or load).
    pub(super) saved_hash: u64,
    pub(super) saved_len: usize,
    /// Cached `(hash, len)` of the buffer at a specific `dirty_gen`.
    /// Hashing the full document on every keystroke is O(N) in buffer size
    /// (~3 MB on a 100 K-line file = ~9 % of per-keystroke CPU); cache by
    /// `dirty_gen` so the hash runs at most once per content change.
    pub(super) signature_cache: Option<(u64, (u64, usize))>,
    /// mtime of the file on disk at the most recent load or save.
    pub disk_mtime: Option<SystemTime>,
    /// Byte length of the file on disk at the most recent load or save.
    pub disk_len: Option<u64>,
    /// Whether the on-disk file is in sync, changed, or deleted.
    pub disk_state: DiskState,
    /// Path to the swap file for this slot, if one has been computed.
    /// `None` for scratch buffers (no filename) or before first write.
    pub swap_path: Option<std::path::PathBuf>,
    /// `dirty_gen` of the buffer the last time the swap file was written.
    /// `None` = never written.  Used to skip redundant writes when the
    /// buffer has not changed since the last swap flush.
    pub last_swap_dirty_gen: Option<u64>,
    /// `dirty_gen` at which auto-folds (from `foldmethod=expr`) were last
    /// applied to the buffer. `None` = never applied.
    /// Used to skip re-extraction when the tree hasn't changed since the
    /// last fold pass.
    pub(super) last_fold_dirty_gen: Option<u64>,
    /// Cached per-row git blame. `blame[row]` is `None` when the row has no
    /// attribution (new file, untracked, or row past blame output).
    /// Cleared when `blame_inline` is toggled off.
    pub(crate) blame: Vec<Option<hjkl_app::git::BlameInfo>>,
    /// `dirty_gen` of the buffer when `blame` was last rebuilt.
    /// `None` = stale or never computed.
    pub(crate) last_blame_dirty_gen: Option<u64>,
    /// Wall-clock time of the last successful blame refresh — used
    /// to throttle the libgit2 blame call to ~4 Hz during active typing.
    pub(crate) last_blame_refresh_at: Instant,
}

/// Walk up from `start` looking for a project-root marker file.
///
/// Markers: `.git`, `Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`,
/// `setup.py`, `composer.json`, `.hg`.  Returns the first directory that
/// contains one of these files, or `start` itself as a fallback.
pub(crate) fn find_project_root(start: &std::path::Path) -> PathBuf {
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "setup.py",
        "composer.json",
        ".hg",
    ];
    let mut dir = start.to_owned();
    loop {
        for marker in MARKERS {
            if dir.join(marker).exists() {
                return dir;
            }
        }
        match dir.parent() {
            Some(p) => dir = p.to_owned(),
            None => return start.to_owned(),
        }
    }
}

impl BufferSlot {
    /// Snapshot the loaded content so undo-to-saved clears dirty.
    pub(super) fn snapshot_saved(&mut self) {
        let (h, l) = self.cached_signature();
        self.saved_hash = h;
        self.saved_len = l;
        self.dirty = false;
    }

    /// Sync `self.dirty` against a fresh content comparison.
    ///
    /// Fast path: when the current buffer length differs from the saved
    /// length, the buffer is definitely dirty — skip the hash. Only when
    /// lengths match do we compute (and cache) the hash to disambiguate
    /// "user undid back to saved state" from "user typed different
    /// content of the same length". On a sustained edit session this
    /// short-circuit fires on every keystroke, dropping ~10 % of
    /// per-keystroke main-thread CPU.
    pub(super) fn refresh_dirty_against_saved(&mut self) -> u128 {
        let t = std::time::Instant::now();
        // `Buffer::byte_len()` is cached against dirty_gen and computes
        // the length by summing per-row `.len()` under one lock — no
        // join, no allocation. `content_joined().len()` was forcing the
        // full ~3 MB joined `String` build on huge files just to read
        // a single integer.
        let current_len = self.editor.buffer().byte_len();
        if current_len != self.saved_len {
            self.dirty = true;
            return t.elapsed().as_micros();
        }
        let (h, _) = self.cached_signature();
        self.dirty = h != self.saved_hash;
        t.elapsed().as_micros()
    }

    /// Return `(hash, len)` of the current buffer content. Memoized by
    /// `dirty_gen`: while the buffer is unchanged subsequent calls return
    /// the cached value without re-hashing. Without this, every keystroke
    /// re-hashes the full `content_joined()` Arc (~3 MB on a 100 K-line
    /// file, ~9 % of per-keystroke CPU in profiling).
    fn cached_signature(&mut self) -> (u64, usize) {
        let dg = self.editor.buffer().dirty_gen();
        if let Some((cached_dg, sig)) = self.signature_cache
            && cached_dg == dg
        {
            return sig;
        }
        let sig = buffer_signature(&self.editor);
        self.signature_cache = Some((dg, sig));
        sig
    }
}
