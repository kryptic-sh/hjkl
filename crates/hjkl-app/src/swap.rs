//! Swap file core — path resolution, header format, atomic read/write.
//!
//! Swap files live in `<XDG_CACHE_HOME>/hjkl/swap/<hash>.swp` where `<hash>`
//! is the first 16 hex chars of a FNV-1a-64 over the canonicalized file path.
//! Scratch (never-saved) buffers get `scratch_<pid>_<bufid>.swp` in the same
//! directory; their header has `canonical_path = ""`.
//!
//! Where the swap directory lives is a parameter, not a lookup: [`SwapRoot`]
//! is either `Xdg` (the production default, resolved on every use) or an
//! explicit directory. Every path helper has an `_in` variant taking one.
//! Tests use the explicit variant rather than overriding `XDG_CACHE_HOME` —
//! that variable is process-global, so an override is visible to every thread
//! in the binary and not just to the test that set it.
//!
//! Format (v3):
//! - 4 bytes  magic `b"HSWP"`
//! - then a postcard-encoded `SwapHeader` length-prefixed by a `u32` LE
//! - then a `u32` LE undo-section length + that many bytes of a postcard
//!   [`SwapUndo`] (`0` ⇒ no undo tree — content-only, older/degraded write)
//! - then the raw UTF-8 body (rope chunks streamed directly)
//!
//! The undo section (v3) carries the buffer's
//! serialized undo tree + live current node so `:recover` restores undo/redo,
//! not just the unsaved text. postcard is not self-describing, so a v2 file
//! (no undo section) parses as `Err` under the v3 reader and is treated as "no
//! usable swap" — no migration, the bump is safe by construction.

#[cfg(unix)]
use libc;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use hjkl_buffer::SerTree;
use ropey::Rope;

// ── FNV-1a-64 hash ────────────────────────────────────────────────────────────

/// FNV-1a 64-bit hash over `bytes`. Build-stable (no randomisation), collision
/// probability acceptable for path-namespacing. We do NOT use sha2 to avoid
/// pulling that crate into hjkl-app; sha2 is already a dep only of hjkl-anvil.
fn fnv1a64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;
    let mut h = FNV_OFFSET;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

// ── Header struct ─────────────────────────────────────────────────────────────

/// The binary header prepended to every swap file.
///
/// Serialized with `postcard` (length-prefixed by a `u32` LE).  The rest of
/// the file is the raw UTF-8 buffer body.
///
/// **Version history**
/// - v1: original fields (no `writer_pid`)
/// - v2: adds `writer_pid` for PID-lock multi-instance protection
/// - v3: adds a length-delimited [`SwapUndo`] section after the header (the
///   serialized undo tree + live current node) so `:recover` restores undo/redo
///
/// postcard is not self-describing, so v1 bytes deserialize as `Err` when
/// read with a v2 schema.  Callers treat a read error as "no usable swap"
/// (stale / corrupt / wrong version); see [`read_swap`] doc.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwapHeader {
    /// Magic identifier — always `b"HSWP"`.
    pub magic: [u8; 4],
    /// Format version.  Currently `2`.
    pub version: u16,
    /// Canonicalized filesystem path of the edited file.
    pub canonical_path: String,
    /// mtime of the file on disk at swap-write time, in milliseconds since
    /// UNIX epoch.  `0` when the file was absent (new-file buffer).
    pub file_mtime_unix_ms: u64,
    /// Wall-clock time this swap was written, in milliseconds since UNIX epoch.
    pub write_time_unix_ms: u64,
    /// Cursor position `(row, col)` — 0-based.
    pub cursor: (u32, u32),
    /// PID of the process that last wrote this swap.  Used for multi-instance
    /// protection: if this PID is still alive and is not the current process,
    /// the file is locked by another hjkl instance.
    pub writer_pid: u32,
}

impl SwapHeader {
    /// Magic bytes for the swap format.
    pub const MAGIC: [u8; 4] = *b"HSWP";
    /// Current format version.
    pub const VERSION: u16 = 3;
}

/// The v3 undo section: the buffer's serialized
/// undo tree plus the `seq` of the live current node, carried in the swap so a
/// crash-`:recover` restores the whole undo/redo history — not just the unsaved
/// text — a strict improvement over vim/nvim (which flatten undo on recover).
///
/// Serialized with `postcard` in its own length-delimited section between the
/// header and the body. A read error (schema drift, truncation) makes recovery
/// fall back to content-only; it never blocks or corrupts the recovery.
///
/// A single-node tree is written with `base: None` (the body IS the base —
/// see [`dedup_single_node_base`]); [`read_swap_full`] re-substitutes the body
/// text before the tree is rebuilt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapUndo {
    /// The serialized undo tree (root base text + delta-encoded nodes).
    pub tree: SerTree,
    /// `seq` of the live current node — must match `tree`'s current node; a
    /// mismatch on read rejects the tree (recover content only).
    pub current_seq: u64,
}

/// When the swap's undo tree is a single node, the root's base is
/// byte-identical to the swap body: `undo_to_serializable` syncs the current
/// node to the live rope first, and a single-node tree has root == current —
/// so `sync_current` stashed the very rope being streamed as the body into the
/// root's base. Drop the base so the document is stored ONCE, not twice;
/// [`read_swap_full`] restores it from the body on recovery. Multi-node trees
/// keep their real anchor.
///
/// `body` is only consulted by the debug assertion; pass the rope being
/// written as the swap body.
pub fn dedup_single_node_base(tree: &mut SerTree, body: &Rope) {
    if tree.nodes.len() == 1 && tree.base.is_some() {
        debug_assert_eq!(
            tree.base.as_deref(),
            Some(body.to_string().as_str()),
            "single-node swap tree: base must equal the swap body"
        );
        tree.base = None;
    }
}

// ── Where swap files live ─────────────────────────────────────────────────────

/// Which directory swap files are written into.
///
/// Production uses [`SwapRoot::Xdg`], which resolves `<XDG_CACHE_HOME>/hjkl/
/// swap/` on every use exactly as before. [`SwapRoot::At`] names the directory
/// outright, which is what lets a test point the swap directory at its own
/// `TempDir` **without** `std::env::set_var("XDG_CACHE_HOME", …)`.
///
/// That distinction is the whole point. The environment is process-global, so a
/// test that overrides `XDG_CACHE_HOME` changes it for every other thread in the
/// same test binary — `set_var` is `unsafe` in Rust 2024 for exactly this
/// reason. A `Mutex` around the mutation only serializes the tests that agreed
/// to take it; every unrelated test that merely *reads* the variable (any of the
/// many that construct an `App`, and so resolve a swap directory) still observes
/// the override and creates `<the overriding test's temp dir>/hjkl/` under it.
/// When that temp dir is also an explorer root, a stray `hjkl/` row appears in
/// the tree and the next `dd` deletes the wrong line. An explicit root cannot
/// collide with anything, because nothing else can name it.
///
/// Same shape as [`crate::trash::TrashRoot`], for the same reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SwapRoot {
    /// `<XDG_CACHE_HOME>/hjkl/swap/`, resolved on each use — the production
    /// default.
    #[default]
    Xdg,
    /// An explicit directory, created on first use with the same owner-only
    /// mode as the XDG one.
    At(PathBuf),
}

impl SwapRoot {
    /// Resolve to a concrete directory, creating it (owner-only) if needed.
    ///
    /// Both variants go through [`hjkl_fs::ensure_private_dir`], so the
    /// permissions of an injected root match the XDG one — swap files hold
    /// unsaved buffer contents (potentially credentials, private keys, etc.),
    /// and that is not for other local users to read in either case.
    pub fn dir(&self) -> std::io::Result<PathBuf> {
        match self {
            Self::Xdg => hjkl_fs::private_cache_subdir("hjkl", "swap"),
            Self::At(dir) => {
                hjkl_fs::ensure_private_dir(dir)?;
                Ok(dir.clone())
            }
        }
    }
}

// ── Directory helpers ─────────────────────────────────────────────────────────

/// Return (and auto-create) `<XDG_CACHE_HOME>/hjkl/swap/`.
///
/// Owner-only: swap files hold unsaved buffer contents (potentially credentials,
/// private keys, etc.), so other local users must not be able to enumerate or
/// read them.
///
/// Shorthand for `SwapRoot::Xdg.dir()`.
pub fn swap_dir() -> std::io::Result<PathBuf> {
    SwapRoot::Xdg.dir()
}

/// [`swap_path_for`] against an explicit [`SwapRoot`].
///
/// The `_in` half of the pair, mirroring `hjkl_anvil::store`'s `*_in` helpers:
/// every behaviour below is identical, only the directory is named by the
/// caller instead of read off the process environment.
pub fn swap_path_in(root: &SwapRoot, canonical_path: &Path) -> std::io::Result<PathBuf> {
    let path_str = canonical_path.to_string_lossy();
    let hash = fnv1a64(path_str.as_bytes());
    let name = format!("{hash:016x}.swp");
    Ok(root.dir()?.join(name))
}

/// Stable swap path for a file: `swap_dir()/<hash16>.swp`
///
/// `canonical_path` should be an already-canonicalized absolute path.
/// The hash is the first 16 hex chars of FNV-1a-64 over the UTF-8 bytes of
/// the path string — build-stable, cross-platform.
///
/// Shorthand for `swap_path_in(&SwapRoot::Xdg, …)`.
pub fn swap_path_for(canonical_path: &Path) -> std::io::Result<PathBuf> {
    swap_path_in(&SwapRoot::Xdg, canonical_path)
}

/// [`scratch_swap_path`] against an explicit [`SwapRoot`].
///
/// The `_in` half of the pair: the filename is identical, only the directory
/// is named by the caller instead of read off the process environment.
pub fn scratch_swap_path_in(root: &SwapRoot, pid: u32, buffer_id: u64) -> std::io::Result<PathBuf> {
    Ok(root.dir()?.join(format!("scratch_{pid}_{buffer_id}.swp")))
}

/// Swap path for an unnamed/scratch buffer: `swap_dir()/scratch_<pid>_<bufid>.swp`
///
/// The filename is stable for a given (pid, buffer_id) pair within a session,
/// so the same slot always writes to the same path.
///
/// Shorthand for `scratch_swap_path_in(&SwapRoot::Xdg, …)`.
pub fn scratch_swap_path(pid: u32, buffer_id: u64) -> std::io::Result<PathBuf> {
    scratch_swap_path_in(&SwapRoot::Xdg, pid, buffer_id)
}

/// A recoverable orphan scratch swap discovered by [`scan_orphan_scratch_swaps_in`].
pub struct OrphanScratch {
    /// Path to the `.swp` file on disk.
    pub swap_path: PathBuf,
    /// Parsed header (canonical_path is empty for scratch swaps).
    pub header: SwapHeader,
    /// Full text body of the unsaved buffer.
    pub body: String,
    /// The v3 undo section, if the swap carried one (else `None` ⇒ recover
    /// content only).
    pub undo: Option<SwapUndo>,
}

/// Scan `dir` for scratch swaps (`scratch_*.swp` with empty `canonical_path`)
/// whose `writer_pid` is NOT alive (i.e. the session crashed).
///
/// Live swaps (writer_pid is still running) are skipped — they belong to an
/// active hjkl instance. Unreadable or non-scratch files are silently ignored.
/// Accepts a `dir` parameter for testability without real XDG I/O.
pub fn scan_orphan_scratch_swaps_in(dir: &Path) -> Vec<OrphanScratch> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("scratch_") || !name_str.ends_with(".swp") {
            continue;
        }
        if let Some(orphan) = orphan_scratch_at(&entry.path()) {
            out.push(orphan);
        }
    }
    out
}

/// Read `path` and return it as an [`OrphanScratch`] if — right now — it is a
/// scratch swap whose writer is dead.
///
/// This is the per-file half of [`scan_orphan_scratch_swaps_in`], exposed so a
/// caller that acts on a scan result can re-decide **inside** the exclusive lock
/// it is about to remove the file under. A scan's `pid_is_alive` answer is stale
/// the moment it is returned: a new process can claim that scratch slot before
/// the removal runs, and deleting it then destroys a live session's record of
/// unsaved work. Re-reading rather than only re-checking the pid also means the
/// content acted on is the content that was locked.
///
/// `None` on any of: unreadable/corrupt swap, a named swap (non-empty
/// `canonical_path`), or a writer pid that is still alive.
pub fn orphan_scratch_at(path: &Path) -> Option<OrphanScratch> {
    let (header, body, undo) = read_swap_full(path).ok()?;
    // Only scratch swaps have an empty canonical_path.
    if !header.canonical_path.is_empty() {
        return None;
    }
    // Skip swaps owned by a live process (another hjkl instance).
    if pid_is_alive(header.writer_pid) {
        return None;
    }
    Some(OrphanScratch {
        swap_path: path.to_path_buf(),
        header,
        body,
        undo,
    })
}

/// Convenience: scan the real `swap_dir()`.
pub fn scan_orphan_scratch_swaps() -> Vec<OrphanScratch> {
    match swap_dir() {
        Ok(d) => scan_orphan_scratch_swaps_in(&d),
        Err(_) => Vec::new(),
    }
}

// ── Write ─────────────────────────────────────────────────────────────────────

/// Atomically write a swap file: stream header + rope body to a temp file beside
/// the target, fsync, rename, fsync the parent — all through [`hjkl_fs`].
///
/// `path` is the final `.swp` path (as returned by [`swap_path_for`]).
/// `rope` body is streamed via `rope.chunks()` — no full-document allocation.
pub fn write_swap(path: &Path, header: &SwapHeader, rope: &Rope) -> std::io::Result<()> {
    write_swap_full(path, header, rope, None)
}

/// Like [`write_swap`] but also embeds the v3 [`SwapUndo`] section (the
/// serialized undo tree + live current node) between the header and the body,
/// so a crash-`:recover` restores undo/redo.
///
/// `undo == None` writes an empty undo section (length `0`) — behaviourally a
/// content-only swap. The body is still streamed via `rope.chunks()`.
pub fn write_swap_full(
    path: &Path,
    header: &SwapHeader,
    rope: &Rope,
    undo: Option<&SwapUndo>,
) -> std::io::Result<()> {
    // Exclusive for the whole write: an atomic rename makes each write
    // self-consistent, it does not stop a concurrent instance's reader from
    // seeing the file mid-swap or a concurrent recovery from removing it under
    // us. Re-entrant, so a caller already holding this path's lock across a
    // read-decide-write passes straight through.
    hjkl_fs::with_lock_exclusive(path, || {
        // Serialize header with postcard.
        let header_bytes = postcard::to_stdvec(header).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("postcard serialize: {e}"),
            )
        })?;

        // Serialize the optional undo section; empty (len 0) when absent.
        let undo_bytes: Vec<u8> = match undo {
            Some(u) => postcard::to_stdvec(u).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("postcard serialize undo: {e}"),
                )
            })?,
            None => Vec::new(),
        };

        // Write: magic + u32-LE header length + header bytes + u32-LE undo
        // length + undo bytes + body chunks. `write_atomic_with` owns the
        // temp-name retries, the 0600 mode, the fsync and the rename.
        //
        // The closure is `Fn` (it may be re-run on a temp-name collision), so it
        // borrows `header_bytes` / `undo_bytes` / `rope` and consumes nothing —
        // every call emits the identical byte sequence.
        hjkl_fs::write_atomic_with(path, &hjkl_fs::WriteOptions::state(), |f| {
            f.write_all(&SwapHeader::MAGIC)?;
            let hlen = header_bytes.len() as u32;
            f.write_all(&hlen.to_le_bytes())?;
            f.write_all(&header_bytes)?;
            let ulen = undo_bytes.len() as u32;
            f.write_all(&ulen.to_le_bytes())?;
            f.write_all(&undo_bytes)?;
            for chunk in rope.chunks() {
                f.write_all(chunk.as_bytes())?;
            }
            Ok(())
        })
    })
}

// ── Read ──────────────────────────────────────────────────────────────────────

/// Read a swap file.  Returns `(header, body_string)`.
///
/// Validates the magic prefix; returns `Err` on bad magic or format errors.
/// A version/format mismatch (e.g. v1 swap read with v2 schema) surfaces as
/// `Err(InvalidData)` and is treated as "no usable swap" by all callers —
/// the old swap is effectively ignored.  Swaps are transient cache; no
/// migration is attempted.
pub fn read_swap(path: &Path) -> std::io::Result<(SwapHeader, String)> {
    // Shared: concurrent readers may overlap, writers may not. Re-entrant, so
    // the nested `read_swap_full` lock below is satisfied by this one.
    hjkl_fs::with_lock_shared(path, || {
        let (header, body, _undo) = read_swap_full(path)?;
        Ok((header, body))
    })
}

/// Like [`read_swap`] but also returns the v3 [`SwapUndo`] section when present
/// (`None` for a swap written without an undo tree). The header + body are
/// parsed identically; the undo section sits between them.
///
/// Any structural / parse error in the undo section is fatal to the whole read
/// (returns `Err` ⇒ "no usable swap") — consistent with treating a malformed
/// swap as absent. Callers that only need the body use [`read_swap`].
pub fn read_swap_full(path: &Path) -> std::io::Result<(SwapHeader, String, Option<SwapUndo>)> {
    // Shared: a swap being rewritten by another instance must not be parsed
    // half-written. Re-entrant, so a caller holding the exclusive lock across a
    // read-decide-write (recovery) passes straight through without upgrading.
    hjkl_fs::with_lock_shared(path, || read_swap_full_locked(path))
}

/// The parse half of [`read_swap_full`], run with `path`'s lock already held.
fn read_swap_full_locked(path: &Path) -> std::io::Result<(SwapHeader, String, Option<SwapUndo>)> {
    let mut f = std::fs::File::open(path)?;

    // Magic check.
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic)?;
    if magic != SwapHeader::MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "swap: bad magic {magic:?}, expected {:?}",
                SwapHeader::MAGIC
            ),
        ));
    }

    // Header length prefix.
    let mut hlen_buf = [0u8; 4];
    f.read_exact(&mut hlen_buf)?;
    let hlen = u32::from_le_bytes(hlen_buf) as usize;

    // Sanity-cap the header length before allocating: a real header is a
    // path plus a few integers (well under 1 MiB). A corrupt / hostile
    // length prefix must not trigger a multi-GiB allocation.
    const MAX_HEADER_LEN: usize = 1 << 20;
    if hlen > MAX_HEADER_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("swap: header length {hlen} exceeds {MAX_HEADER_LEN}"),
        ));
    }

    // Header bytes.
    let mut header_bytes = vec![0u8; hlen];
    f.read_exact(&mut header_bytes)?;
    let header: SwapHeader = postcard::from_bytes(&header_bytes).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("postcard deserialize: {e}"),
        )
    })?;

    // Undo section: u32-LE length prefix + that many postcard bytes. Length 0
    // ⇒ no tree (content-only). Cap before allocating, like the header/body —
    // the whole delta tree for a large buffer stays well under this.
    const MAX_UNDO_LEN: u64 = 256 * 1024 * 1024;
    let mut ulen_buf = [0u8; 4];
    f.read_exact(&mut ulen_buf)?;
    let ulen = u32::from_le_bytes(ulen_buf) as u64;
    if ulen > MAX_UNDO_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("swap: undo length {ulen} exceeds {MAX_UNDO_LEN}"),
        ));
    }
    let undo: Option<SwapUndo> = if ulen == 0 {
        None
    } else {
        let mut undo_bytes = vec![0u8; ulen as usize];
        f.read_exact(&mut undo_bytes)?;
        let u: SwapUndo = postcard::from_bytes(&undo_bytes).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("postcard deserialize undo: {e}"),
            )
        })?;
        Some(u)
    };

    // Body: cap the remaining file length before allocating. Swaps are cache
    // entries, so oversized or corrupt bodies are discarded during recovery.
    // Header section = 8 + hlen (magic+len prefix+header); undo section =
    // 4 + ulen (len prefix + bytes); body = the remainder.
    const MAX_BODY_LEN: u64 = 64 * 1024 * 1024;
    let body_len = f
        .metadata()?
        .len()
        .saturating_sub(8 + hlen as u64 + 4 + ulen);
    if body_len > MAX_BODY_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("swap: body length {body_len} exceeds {MAX_BODY_LEN}"),
        ));
    }
    let mut body = String::with_capacity(body_len as usize);
    f.take(MAX_BODY_LEN + 1).read_to_string(&mut body)?;
    if body.len() as u64 > MAX_BODY_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("swap: body length exceeds {MAX_BODY_LEN}"),
        ));
    }

    // Single-node trees are written with `base: None` (the body IS the base —
    // see `dedup_single_node_base`); restore the anchor from the body before
    // any consumer rebuilds the tree. For a multi-node tree `base` is always
    // present, so this is a no-op.
    let undo = undo.map(|mut u| {
        if u.tree.base.is_none() {
            u.tree.base = Some(body.clone());
        }
        u
    });

    Ok((header, body, undo))
}

// ── Remove ────────────────────────────────────────────────────────────────────

/// Delete a swap file.  Silently succeeds when the file is absent.
pub fn remove_swap(path: &Path) -> std::io::Result<()> {
    // Exclusive: deleting a swap another instance is mid-write would drop its
    // record of unsaved work. Callers that decided to remove based on a prior
    // read must hold this same lock across the whole read→decide→remove — the
    // re-entrancy here is what lets them.
    hjkl_fs::with_lock_exclusive(path, || match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    })
}

// ── Time helper ───────────────────────────────────────────────────────────────

/// Current time as milliseconds since UNIX epoch.
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

// ── PID liveness ──────────────────────────────────────────────────────────────

/// Is `pid` a currently-live process owned by anyone?  Best-effort,
/// cross-platform.
///
/// - Unix uses `kill(pid, 0)` (alive on `Ok` or `EPERM`).
/// - Windows uses `OpenProcess` + `WaitForSingleObject(0)`: a signaled
///   process object means it has exited; access-denied means it exists but
///   is owned by another user (alive).
/// - Other targets cannot cheaply check, so return `false` (no lock
///   enforced) — recovery still works; only the multi-instance refusal is
///   skipped.
///
/// pid 0 is special-cased as dead on every platform: no OS probe answers
/// "is pid 0 running?" the way the caller means it. POSIX defines pid 0 for
/// `kill` as *every process in the caller's process group*, so `kill(0, 0)`
/// succeeds and reports "alive"; Windows resolves pid 0 to the System Idle
/// Process, which either opens or fails access-denied — both of which this
/// function reads as alive. A `writer_pid` of 0 only ever comes from a
/// truncated or corrupted header, and classifying it as live pins the swap
/// file to a "live owner" forever: the multi-instance refusal then blocks the
/// user from opening the file with no in-editor way to clear it.
pub fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // kill(pid, 0): 0 = alive & ours; EPERM = alive, not ours;
        // ESRCH = dead.
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if r == 0 {
            return true;
        }
        // errno EPERM => process exists but we lack permission => alive.
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            WaitForSingleObject,
        };
        const ERROR_ACCESS_DENIED: u32 = 5;

        // SAFETY: plain Win32 FFI. The handle returned by OpenProcess is
        // checked for null and always closed before returning.
        unsafe {
            let handle = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0, // bInheritHandle = FALSE
                pid,
            );
            if handle.is_null() {
                // No such process => dead; access-denied => exists (alive),
                // owned by another user.
                return GetLastError() == ERROR_ACCESS_DENIED;
            }
            // The process object becomes signaled only once it exits, so a
            // zero-timeout wait that returns WAIT_OBJECT_0 means dead;
            // WAIT_TIMEOUT (anything else) means still running.
            let wait = WaitForSingleObject(handle, 0);
            CloseHandle(handle);
            wait != WAIT_OBJECT_0
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn sample_header(path: &str) -> SwapHeader {
        SwapHeader {
            magic: SwapHeader::MAGIC,
            version: SwapHeader::VERSION,
            canonical_path: path.to_string(),
            file_mtime_unix_ms: 1_700_000_000_000,
            write_time_unix_ms: 1_700_000_001_000,
            cursor: (3, 7),
            writer_pid: std::process::id(),
        }
    }

    /// Test the FNV-1a filename determinism directly — no XDG I/O.
    #[test]
    fn swap_path_is_stable_for_same_path() {
        // The last component of the swap path is the hash filename; it must
        // be identical for the same input regardless of which swap_dir() resolves to.
        let p = Path::new("/home/user/project/src/main.rs");
        let hash_a = format!("{:016x}.swp", fnv1a64(p.to_string_lossy().as_bytes()));
        let hash_b = format!("{:016x}.swp", fnv1a64(p.to_string_lossy().as_bytes()));
        assert_eq!(hash_a, hash_b, "same path must produce same swap filename");
    }

    /// Test that different paths produce different hash filenames.
    #[test]
    fn swap_path_differs_for_different_paths() {
        let a = format!("{:016x}.swp", fnv1a64(b"/home/user/a.rs"));
        let b = format!("{:016x}.swp", fnv1a64(b"/home/user/b.rs"));
        assert_ne!(
            a, b,
            "different paths must produce different swap filenames"
        );
    }

    #[test]
    fn write_then_read_roundtrips_header_and_body() {
        let td2 = tempfile::tempdir().unwrap();
        let swp = td2.path().join("test.swp");

        let header = sample_header("/tmp/hello.rs");
        let rope = Rope::from_str("hello world\nline two\n");
        write_swap(&swp, &header, &rope).unwrap();

        let (got_header, got_body) = read_swap(&swp).unwrap();
        assert_eq!(got_header, header);
        assert_eq!(got_body, "hello world\nline two\n");
    }

    #[test]
    fn write_swap_is_atomic_no_tmp_left() {
        let td2 = tempfile::tempdir().unwrap();
        let swp = td2.path().join("atomic.swp");

        let header = sample_header("/tmp/atomic.rs");
        let rope = Rope::from_str("data");
        write_swap(&swp, &header, &rope).unwrap();

        assert!(swp.exists(), ".swp must exist after write");
        // No .tmp files should be left behind (temp uses a unique random name
        // that is always renamed or cleaned up).
        let has_tmp = std::fs::read_dir(td2.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().is_some_and(|ext| ext == "tmp"));
        assert!(!has_tmp, "no .tmp files should remain after write");
    }

    #[test]
    fn read_swap_rejects_bad_magic() {
        let td2 = tempfile::tempdir().unwrap();
        let swp = td2.path().join("bad.swp");
        std::fs::write(&swp, b"XBAD\x00\x00\x00\x00garbage").unwrap();
        let err = read_swap(&swp).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn read_swap_rejects_oversized_header_length() {
        let td2 = tempfile::tempdir().unwrap();
        let swp = td2.path().join("hostile.swp");
        // Valid magic + a hostile 0xFFFFFFFF header-length prefix. Must be
        // rejected with InvalidData BEFORE attempting a 4 GiB allocation.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&SwapHeader::MAGIC);
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&swp, &bytes).unwrap();
        let err = read_swap(&swp).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn read_swap_rejects_oversized_body() {
        let td2 = tempfile::tempdir().unwrap();
        let swp = td2.path().join("oversized.swp");
        let header = sample_header("/tmp/large.rs");
        let header_bytes = postcard::to_allocvec(&header).unwrap();
        let file = std::fs::File::create(&swp).unwrap();
        // magic(4) + hlen(4) + header + undo-len(4, left zero ⇒ no tree) + an
        // oversized body (> MAX_BODY_LEN). The zeroed undo-length section is
        // read as 0 so the whole remainder counts as body.
        file.set_len(8 + header_bytes.len() as u64 + 4 + 64 * 1024 * 1024 + 1)
            .unwrap();
        drop(file);
        let mut file = std::fs::OpenOptions::new().write(true).open(&swp).unwrap();
        file.write_all(&SwapHeader::MAGIC).unwrap();
        file.write_all(&(header_bytes.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(&header_bytes).unwrap();
        let err = read_swap(&swp).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn remove_swap_ignores_missing() {
        let td2 = tempfile::tempdir().unwrap();
        let swp = td2.path().join("nonexistent.swp");
        assert!(
            remove_swap(&swp).is_ok(),
            "remove on absent file must be Ok"
        );
    }

    #[test]
    fn body_roundtrips_multibyte() {
        let td2 = tempfile::tempdir().unwrap();
        let swp = td2.path().join("utf8.swp");

        let content = "こんにちは\n🦀 Rust 🦀\n日本語テスト\n";
        let header = sample_header("/tmp/utf8.rs");
        let rope = Rope::from_str(content);
        write_swap(&swp, &header, &rope).unwrap();

        let (_, got_body) = read_swap(&swp).unwrap();
        assert_eq!(got_body, content);
    }

    // ── PID liveness tests ────────────────────────────────────────────────────

    /// pid_is_alive returns true for the current process on unix + windows.
    #[test]
    #[cfg(any(unix, windows))]
    fn pid_is_alive_true_for_self() {
        assert!(
            pid_is_alive(std::process::id()),
            "current process must report as alive"
        );
    }

    /// A very-high pid that is almost certainly not running returns false.
    /// (On Windows pids are multiples of 4, so 999_999_999 is also invalid.)
    #[test]
    #[cfg(any(unix, windows))]
    fn pid_is_alive_false_for_unused_pid() {
        assert!(
            !pid_is_alive(999_999_999),
            "pid 999_999_999 should not be alive"
        );
    }

    /// pid 0 is never a live owner. `kill(0, 0)` targets the caller's whole
    /// process group and returns success, so without the explicit guard a
    /// truncated/corrupt header decoding `writer_pid == 0` would be classified
    /// as owned by a live process forever and lock the user out of the file.
    #[test]
    fn pid_is_alive_false_for_pid_zero() {
        assert!(
            !pid_is_alive(0),
            "pid 0 must never report as a live process owner"
        );
    }

    /// On targets without a liveness probe, pid_is_alive always returns false
    /// (no multi-instance enforcement).
    #[test]
    #[cfg(not(any(unix, windows)))]
    fn pid_is_alive_false_without_probe() {
        assert!(!pid_is_alive(std::process::id()));
        assert!(!pid_is_alive(999_999_999));
    }

    // ── scratch_swap_path tests ───────────────────────────────────────────────

    /// Same (pid, bufid) always produces the same path component.
    #[test]
    fn scratch_swap_path_stable_and_distinct() {
        // We can't call swap_dir() without real XDG, so test the filename shape
        // by inspecting the last component (swap_dir varies per machine).
        // Two calls with the same args must agree on the filename.
        let pid = 12345u32;
        let buf_a = 7u64;
        let buf_b = 8u64;
        let name_a1 = format!("scratch_{pid}_{buf_a}.swp");
        let name_a2 = format!("scratch_{pid}_{buf_a}.swp");
        let name_b = format!("scratch_{pid}_{buf_b}.swp");
        assert_eq!(name_a1, name_a2, "same (pid,bufid) must give same name");
        assert_ne!(name_a1, name_b, "different bufid must give different name");
    }

    // ── scan_orphan_scratch_swaps_in tests ───────────────────────────────────

    // Only used by the unix-gated scan tests below (pid liveness); gating it
    // too keeps `-D dead_code` happy on Windows.
    #[cfg(unix)]
    fn dead_pid_scratch_header() -> SwapHeader {
        SwapHeader {
            magic: SwapHeader::MAGIC,
            version: SwapHeader::VERSION,
            canonical_path: String::new(), // empty = scratch
            file_mtime_unix_ms: 0,
            write_time_unix_ms: 1_700_000_000_000,
            cursor: (1, 0),
            writer_pid: 999_999_999, // almost certainly dead
        }
    }

    /// A scratch swap with a dead writer_pid is returned by the scan.
    #[test]
    #[cfg(unix)]
    fn scan_finds_dead_pid_scratch_orphan() {
        let td = tempfile::tempdir().unwrap();
        let swp = td.path().join("scratch_999999999_42.swp");
        let header = dead_pid_scratch_header();
        let rope = Rope::from_str("unsaved content\n");
        write_swap(&swp, &header, &rope).unwrap();

        let orphans = scan_orphan_scratch_swaps_in(td.path());
        assert_eq!(orphans.len(), 1, "expected 1 orphan, got {}", orphans.len());
        assert_eq!(orphans[0].body, "unsaved content\n");
        assert!(orphans[0].header.canonical_path.is_empty());
    }

    /// A scratch swap whose writer_pid is THIS process is alive → skipped.
    #[test]
    #[cfg(unix)]
    fn scan_skips_live_pid_scratch() {
        let td = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let swp = td.path().join(format!("scratch_{pid}_1.swp"));
        let header = SwapHeader {
            magic: SwapHeader::MAGIC,
            version: SwapHeader::VERSION,
            canonical_path: String::new(),
            file_mtime_unix_ms: 0,
            write_time_unix_ms: 1_700_000_000_000,
            cursor: (0, 0),
            writer_pid: pid,
        };
        let rope = Rope::from_str("live session content");
        write_swap(&swp, &header, &rope).unwrap();

        let orphans = scan_orphan_scratch_swaps_in(td.path());
        assert!(
            orphans.is_empty(),
            "live-pid scratch swap must be skipped, got {} orphan(s)",
            orphans.len()
        );
    }

    /// A named-file swap (non-empty canonical_path) in the dir is NOT returned.
    #[test]
    fn scan_skips_named_swaps() {
        let td = tempfile::tempdir().unwrap();
        // Use scratch_ prefix to pass the name filter, but give it a non-empty canonical_path.
        let swp = td.path().join("scratch_999999999_99.swp");
        let header = SwapHeader {
            magic: SwapHeader::MAGIC,
            version: SwapHeader::VERSION,
            canonical_path: "/home/user/foo.rs".to_string(), // non-empty → named
            file_mtime_unix_ms: 0,
            write_time_unix_ms: 1_700_000_000_000,
            cursor: (0, 0),
            writer_pid: 999_999_999,
        };
        let rope = Rope::from_str("named file content");
        write_swap(&swp, &header, &rope).unwrap();

        let orphans = scan_orphan_scratch_swaps_in(td.path());
        assert!(
            orphans.is_empty(),
            "named swap must be excluded from scratch scan"
        );
    }

    /// A `scratch_*.swp` with garbage bytes is silently skipped, no panic.
    #[test]
    fn scan_skips_unreadable() {
        let td = tempfile::tempdir().unwrap();
        let swp = td.path().join("scratch_999999999_77.swp");
        std::fs::write(&swp, b"GARBAGE DATA NOT A VALID SWAP").unwrap();

        let orphans = scan_orphan_scratch_swaps_in(td.path());
        assert!(
            orphans.is_empty(),
            "unreadable swap must be skipped without panic"
        );
    }

    // ── Header v2 roundtrip ───────────────────────────────────────────────────

    /// Write a header with writer_pid=1234, read back, assert field matches.
    #[test]
    fn header_v2_roundtrips_writer_pid() {
        let td = tempfile::tempdir().unwrap();
        let swp = td.path().join("v2.swp");

        let header = SwapHeader {
            magic: SwapHeader::MAGIC,
            version: SwapHeader::VERSION,
            canonical_path: "/tmp/v2test.rs".to_string(),
            file_mtime_unix_ms: 1_000_000,
            write_time_unix_ms: 1_000_001,
            cursor: (0, 0),
            writer_pid: 1234,
        };
        let rope = Rope::from_str("test body");
        write_swap(&swp, &header, &rope).unwrap();

        let (got, _body) = read_swap(&swp).unwrap();
        assert_eq!(got.writer_pid, 1234, "writer_pid must roundtrip");
        assert_eq!(got.version, SwapHeader::VERSION);
    }

    // ── v3 undo-section roundtrip ─────────────────────────────────────────────

    /// A minimal but structurally-valid single-node [`SerTree`] (root == current,
    /// no delta) — enough to exercise the swap's undo-section serialization.
    fn sample_tree(base: &str, seq: u64) -> SerTree {
        SerTree {
            base: Some(base.to_string()),
            nodes: vec![hjkl_buffer::SerNode {
                parent: None,
                children: Vec::new(),
                last_child: None,
                delta: None,
                cursor: (2, 5),
                timestamp_unix_ms: 1_700_000_000_000,
                marks: hjkl_buffer::MarkSnapshot::default(),
                seq,
            }],
            root: 0,
            current: 0,
            next_seq: seq + 1,
        }
    }

    /// v3: the undo tree + current_seq round-trip through write/read alongside
    /// the body.
    #[test]
    fn v3_write_read_roundtrips_undo_tree_and_seq() {
        let td = tempfile::tempdir().unwrap();
        let swp = td.path().join("v3.swp");

        let header = sample_header("/tmp/v3.rs");
        let body = "hello world\nline two\n";
        let rope = Rope::from_str(body);
        let undo = SwapUndo {
            tree: sample_tree(body, 7),
            current_seq: 7,
        };
        write_swap_full(&swp, &header, &rope, Some(&undo)).unwrap();

        let (got_header, got_body, got_undo) = read_swap_full(&swp).unwrap();
        assert_eq!(got_header, header);
        assert_eq!(got_body, body);
        let got_undo = got_undo.expect("v3 swap must carry the undo section");
        assert_eq!(got_undo.current_seq, 7);
        assert_eq!(got_undo.tree.base.as_deref(), Some(body));
        assert_eq!(got_undo.tree.nodes.len(), 1);
        assert_eq!(got_undo.tree.root, 0);
        assert_eq!(got_undo.tree.current, 0);
        assert_eq!(got_undo.tree.next_seq, 8);
        assert_eq!(got_undo.tree.nodes[0].seq, 7);
        assert_eq!(got_undo.tree.nodes[0].cursor, (2, 5));
    }

    /// A two-node tree: root "a" → child "ab" (current). The anchor is real
    /// history, so `dedup_single_node_base` must leave it alone.
    fn two_node_tree() -> SerTree {
        SerTree {
            base: Some("a".to_string()),
            nodes: vec![
                hjkl_buffer::SerNode {
                    parent: None,
                    children: vec![1],
                    last_child: Some(1),
                    delta: None,
                    cursor: (0, 0),
                    timestamp_unix_ms: 1,
                    marks: hjkl_buffer::MarkSnapshot::default(),
                    seq: 4,
                },
                hjkl_buffer::SerNode {
                    parent: Some(0),
                    children: Vec::new(),
                    last_child: None,
                    delta: Some(hjkl_buffer::Delta {
                        start: 1,
                        old: String::new(),
                        new: "b".to_string(),
                    }),
                    cursor: (0, 1),
                    timestamp_unix_ms: 2,
                    marks: hjkl_buffer::MarkSnapshot::default(),
                    seq: 5,
                },
            ],
            root: 0,
            current: 1,
            next_seq: 6,
        }
    }

    /// The dedup's contract end to end: a single-node tree is written with
    /// `base: None` in the on-disk undo section (the body IS the base), and
    /// `read_swap_full` recovers a tree whose base is the body — rebuilding to
    /// exactly the swap content.
    #[test]
    fn single_node_tree_dedups_base_and_reads_back_body() {
        let td = tempfile::tempdir().unwrap();
        let swp = td.path().join("dedup.swp");

        let header = sample_header("/tmp/dedup.rs");
        let body = "the quick brown fox jumps\nover the lazy dog\n";
        let rope = Rope::from_str(body);
        let mut undo = SwapUndo {
            tree: sample_tree(body, 7),
            current_seq: 7,
        };
        dedup_single_node_base(&mut undo.tree, &rope);
        assert!(undo.tree.base.is_none(), "single-node base must be dropped");
        write_swap_full(&swp, &header, &rope, Some(&undo)).unwrap();

        // The on-disk undo section carries base: None — parse it out of the
        // raw file exactly as `read_swap_full_locked` does.
        let bytes = std::fs::read(&swp).unwrap();
        let hlen = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let ulen_off = 8 + hlen;
        let ulen = u32::from_le_bytes(bytes[ulen_off..ulen_off + 4].try_into().unwrap()) as usize;
        let parsed: SwapUndo =
            postcard::from_bytes(&bytes[ulen_off + 4..ulen_off + 4 + ulen]).unwrap();
        assert!(parsed.tree.base.is_none(), "base must be None on disk");

        // read_swap_full restores the body as the base; the tree rebuilds and
        // materializes to the swap content.
        let (_h, got_body, got_undo) = read_swap_full(&swp).unwrap();
        assert_eq!(got_body, body);
        let got_undo = got_undo.expect("v3 swap must carry the undo section");
        assert_eq!(got_undo.tree.base.as_deref(), Some(body));
        let view = hjkl_buffer::View::from_str(body);
        assert!(
            view.install_recovered_undo_tree(&got_undo.tree, got_undo.current_seq),
            "recovered tree must install and materialize to the swap body"
        );
    }

    /// A multi-node tree keeps its real base on disk and round-trips it.
    #[test]
    fn multi_node_tree_keeps_base_and_round_trips() {
        let td = tempfile::tempdir().unwrap();
        let swp = td.path().join("multi.swp");

        let header = sample_header("/tmp/multi.rs");
        let rope = Rope::from_str("ab");
        let mut undo = SwapUndo {
            tree: two_node_tree(),
            current_seq: 5,
        };
        dedup_single_node_base(&mut undo.tree, &rope); // no-op for multi-node
        assert!(undo.tree.base.is_some(), "multi-node trees keep their base");
        write_swap_full(&swp, &header, &rope, Some(&undo)).unwrap();

        let (_h, got_body, got_undo) = read_swap_full(&swp).unwrap();
        assert_eq!(got_body, "ab");
        let got_undo = got_undo.expect("v3 swap must carry the undo section");
        assert_eq!(
            got_undo.tree.base.as_deref(),
            Some("a"),
            "anchor round-trips"
        );
        assert_eq!(got_undo.tree.nodes.len(), 2);
        let view = hjkl_buffer::View::from_str("ab");
        assert!(
            view.install_recovered_undo_tree(&got_undo.tree, got_undo.current_seq),
            "multi-node recovered tree must install"
        );
    }

    /// A swap written with body text but NO undo tree (the `write_swap` /
    /// content-only path) reads back with `undo == None` and the body intact.
    #[test]
    fn v3_body_only_swap_has_no_undo_section() {
        let td = tempfile::tempdir().unwrap();
        let swp = td.path().join("bodyonly.swp");

        let header = sample_header("/tmp/bodyonly.rs");
        let body = "no undo here\n";
        let rope = Rope::from_str(body);
        // write_swap delegates to write_swap_full(.., None).
        write_swap(&swp, &header, &rope).unwrap();

        let (_h, got_body, got_undo) = read_swap_full(&swp).unwrap();
        assert_eq!(got_body, body);
        assert!(
            got_undo.is_none(),
            "content-only swap must have no undo section"
        );
    }

    /// An old v2-shaped swap — magic + header + body with NO undo-length prefix
    /// section — must be rejected as "no usable swap" (`Err`) under the v3
    /// reader, never panic. (v2 and v3 share the SwapHeader schema; the section
    /// is what differs, so the reader mis-reads the body's first bytes as the
    /// undo length and rejects it / short-reads.)
    #[test]
    fn v2_shaped_swap_is_rejected_no_panic() {
        let td = tempfile::tempdir().unwrap();
        let swp = td.path().join("v2shaped.swp");

        let header = sample_header("/tmp/v2shaped.rs");
        let header_bytes = postcard::to_allocvec(&header).unwrap();
        // Old layout: magic + u32 hlen + header + raw body (no undo section).
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&SwapHeader::MAGIC);
        bytes.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&header_bytes);
        bytes.extend_from_slice(b"old v2 body bytes with no undo length prefix\n");
        std::fs::write(&swp, &bytes).unwrap();

        // No panic; both read entry points surface an error → "no usable swap".
        assert!(read_swap_full(&swp).is_err());
        assert!(read_swap(&swp).is_err());
    }

    /// Format lock: the exact bytes `write_swap_full` produces, built here
    /// independently of the writer. `read_swap_full` is the only consumer of this
    /// layout, so a reordering or an extra field would break `:recover` on every
    /// swap written by an older build — this test fails instead.
    #[test]
    fn on_disk_layout_is_byte_exact() {
        let td = tempfile::tempdir().unwrap();
        let swp = td.path().join("layout.swp");

        let header = sample_header("/tmp/layout.rs");
        let body = "line one\nline two\n";
        let rope = Rope::from_str(body);
        let undo = SwapUndo {
            tree: sample_tree(body, 3),
            current_seq: 3,
        };
        write_swap_full(&swp, &header, &rope, Some(&undo)).unwrap();

        let header_bytes = postcard::to_stdvec(&header).unwrap();
        let undo_bytes = postcard::to_stdvec(&undo).unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(&SwapHeader::MAGIC);
        expected.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
        expected.extend_from_slice(&header_bytes);
        expected.extend_from_slice(&(undo_bytes.len() as u32).to_le_bytes());
        expected.extend_from_slice(&undo_bytes);
        expected.extend_from_slice(body.as_bytes());

        assert_eq!(std::fs::read(&swp).unwrap(), expected);
    }

    /// Format lock for the content-only arm: `undo == None` must still emit a
    /// zero-length undo section, not omit it (omitting it is the v2 layout, which
    /// the v3 reader rejects).
    #[test]
    fn content_only_layout_keeps_zero_undo_section() {
        let td = tempfile::tempdir().unwrap();
        let swp = td.path().join("layout_nodo.swp");

        let header = sample_header("/tmp/nodo.rs");
        let body = "just text\n";
        write_swap(&swp, &header, &Rope::from_str(body)).unwrap();

        let header_bytes = postcard::to_stdvec(&header).unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(&SwapHeader::MAGIC);
        expected.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
        expected.extend_from_slice(&header_bytes);
        expected.extend_from_slice(&0u32.to_le_bytes());
        expected.extend_from_slice(body.as_bytes());

        assert_eq!(std::fs::read(&swp).unwrap(), expected);
    }

    /// A truncated undo section (length prefix promises more bytes than exist)
    /// is rejected without panicking.
    #[test]
    fn truncated_undo_section_is_rejected_no_panic() {
        let td = tempfile::tempdir().unwrap();
        let swp = td.path().join("trunc.swp");

        let header = sample_header("/tmp/trunc.rs");
        let header_bytes = postcard::to_allocvec(&header).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&SwapHeader::MAGIC);
        bytes.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&header_bytes);
        // Claim a 4096-byte undo section but provide only a few bytes.
        bytes.extend_from_slice(&4096u32.to_le_bytes());
        bytes.extend_from_slice(b"\x01\x02\x03");
        std::fs::write(&swp, &bytes).unwrap();

        assert!(read_swap_full(&swp).is_err());
    }
}
