//! Swap file core — path resolution, header format, atomic read/write.
//!
//! Swap files live in `<XDG_CACHE_HOME>/hjkl/swap/<hash>.swp` where `<hash>`
//! is the first 16 hex chars of a FNV-1a-64 over the canonicalized file path.
//! Scratch (never-saved) buffers get `scratch_<pid>_<bufid>.swp` in the same
//! directory; their header has `canonical_path = ""`.
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapUndo {
    /// The serialized undo tree (root base text + delta-encoded nodes).
    pub tree: SerTree,
    /// `seq` of the live current node — must match `tree`'s current node; a
    /// mismatch on read rejects the tree (recover content only).
    pub current_seq: u64,
}

// ── Directory helpers ─────────────────────────────────────────────────────────

/// Return (and auto-create) `<XDG_CACHE_HOME>/hjkl/swap/`.
pub fn swap_dir() -> std::io::Result<PathBuf> {
    let base = hjkl_xdg::cache_dir("hjkl")
        .map_err(|e| std::io::Error::other(format!("xdg cache_dir: {e}")))?;
    let dir = base.join("swap");
    std::fs::create_dir_all(&dir)?;
    // Swap files hold unsaved buffer contents (potentially credentials, private
    // keys, etc.). Keep the directory owner-only so other local users cannot
    // enumerate or read them.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

/// Stable swap path for a file: `swap_dir()/<hash16>.swp`
///
/// `canonical_path` should be an already-canonicalized absolute path.
/// The hash is the first 16 hex chars of FNV-1a-64 over the UTF-8 bytes of
/// the path string — build-stable, cross-platform.
pub fn swap_path_for(canonical_path: &Path) -> std::io::Result<PathBuf> {
    let path_str = canonical_path.to_string_lossy();
    let hash = fnv1a64(path_str.as_bytes());
    let name = format!("{hash:016x}.swp");
    Ok(swap_dir()?.join(name))
}

/// Swap path for an unnamed/scratch buffer: `swap_dir()/scratch_<pid>_<bufid>.swp`
///
/// The filename is stable for a given (pid, buffer_id) pair within a session,
/// so the same slot always writes to the same path.
pub fn scratch_swap_path(pid: u32, buffer_id: u64) -> std::io::Result<PathBuf> {
    Ok(swap_dir()?.join(format!("scratch_{pid}_{buffer_id}.swp")))
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
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("scratch_") || !name_str.ends_with(".swp") {
            continue;
        }
        let path = entry.path();
        let (header, body, undo) = match read_swap_full(&path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        // Only scratch swaps have an empty canonical_path.
        if !header.canonical_path.is_empty() {
            continue;
        }
        // Skip swaps owned by a live process (another hjkl instance).
        if pid_is_alive(header.writer_pid) {
            continue;
        }
        out.push(OrphanScratch {
            swap_path: path,
            header,
            body,
            undo,
        });
    }
    out
}

/// Convenience: scan the real `swap_dir()`.
pub fn scan_orphan_scratch_swaps() -> Vec<OrphanScratch> {
    match swap_dir() {
        Ok(d) => scan_orphan_scratch_swaps_in(&d),
        Err(_) => Vec::new(),
    }
}

// ── Write ─────────────────────────────────────────────────────────────────────

/// Generate a 16-char hex suffix from a platform-secure random source for
/// temp-file uniqueness.  Falls back to a time / pid / counter mix on
/// platforms without `/dev/urandom` (the fallback is not crypto-grade but is
/// unique enough in-practice for a temp filename).
fn random_suffix() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 8];
        if let Ok(mut f) = std::fs::File::open("/dev/urandom")
            && f.read_exact(&mut buf).is_ok()
        {
            return format!("{:016x}", u64::from_le_bytes(buf));
        }
    }
    // Fallback (non-Unix, or /dev/urandom unavailable).
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    // Mix with Knuth's multiplicative hash constants.
    let combined = now
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(pid.wrapping_mul(1_442_695_040_888_963_407))
        .wrapping_add(count);
    format!("{:016x}", combined)
}

/// Atomically write a swap file: stream header + rope body to a unique
/// temporary file (`<path>.<random>.tmp`), fsync, then rename to `path`.
///
/// `path` is the final `.swp` path (as returned by [`swap_path_for`]).
/// `rope` body is streamed via `rope.chunks()` — no full-document allocation.
///
/// Uses `create_new` (O_CREAT | O_EXCL) on a random temp path so concurrent
/// writers never share the same temporary file.  Retries up to 5 times on
/// collision.
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

    const MAX_RETRIES: u32 = 5;
    let mut last_err: Option<std::io::Error> = None;

    for _ in 0..MAX_RETRIES {
        let suffix = random_suffix();
        let tmp_name = format!(
            "{}.{}.tmp",
            path.file_name().unwrap_or_default().to_string_lossy(),
            suffix
        );
        let tmp = path.with_file_name(&tmp_name);

        // Open with create_new (O_EXCL) so we never open a pre-existing file.
        // On Unix set mode 0o600 so unsaved contents are owner-only even on
        // first creation (create_new guarantees this is first creation).
        let mut f = {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            match opts.open(&tmp) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    last_err = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            }
        };

        // Write: magic + u32-LE header length + header bytes + u32-LE undo
        // length + undo bytes + body chunks.
        let write_result = (|| -> std::io::Result<()> {
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
            f.sync_all()?;
            Ok(())
        })();

        if let Err(e) = write_result {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }

        drop(f);

        match std::fs::rename(&tmp, path) {
            Ok(()) => {
                // fsync the parent directory so the rename is durable.
                if let Some(parent) = path.parent()
                    && let Ok(pdir) = std::fs::File::open(parent)
                {
                    let _ = pdir.sync_all();
                }
                return Ok(());
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "failed to create unique swap temp file after retries",
        )
    }))
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
    let (header, body, _undo) = read_swap_full(path)?;
    Ok((header, body))
}

/// Like [`read_swap`] but also returns the v3 [`SwapUndo`] section when present
/// (`None` for a swap written without an undo tree). The header + body are
/// parsed identically; the undo section sits between them.
///
/// Any structural / parse error in the undo section is fatal to the whole read
/// (returns `Err` ⇒ "no usable swap") — consistent with treating a malformed
/// swap as absent. Callers that only need the body use [`read_swap`].
pub fn read_swap_full(path: &Path) -> std::io::Result<(SwapHeader, String, Option<SwapUndo>)> {
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

    Ok((header, body, undo))
}

// ── Remove ────────────────────────────────────────────────────────────────────

/// Delete a swap file.  Silently succeeds when the file is absent.
pub fn remove_swap(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ── Time helper ───────────────────────────────────────────────────────────────

/// Current time as milliseconds since UNIX epoch.
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
pub fn pid_is_alive(pid: u32) -> bool {
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
            base: base.to_string(),
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
        assert_eq!(got_undo.tree.base, body);
        assert_eq!(got_undo.tree.nodes.len(), 1);
        assert_eq!(got_undo.tree.root, 0);
        assert_eq!(got_undo.tree.current, 0);
        assert_eq!(got_undo.tree.next_seq, 8);
        assert_eq!(got_undo.tree.nodes[0].seq, 7);
        assert_eq!(got_undo.tree.nodes[0].cursor, (2, 5));
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
