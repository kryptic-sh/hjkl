//! Swap file core — path resolution, header format, atomic read/write.
//!
//! Swap files live in `<XDG_CACHE_HOME>/hjkl/swap/<hash>.swp` where `<hash>`
//! is the first 16 hex chars of a FNV-1a-64 over the canonicalized file path.
//!
//! Format:
//! - 4 bytes  magic `b"HSWP"`
//! - then a postcard-encoded `SwapHeader` length-prefixed by a `u32` LE
//! - then the raw UTF-8 body (rope chunks streamed directly)
//!
//! TODOs deferred from issue #185:
//! - TODO(#185): scratch/never-saved-buffer swaps

#[cfg(unix)]
use libc;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

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
    pub const VERSION: u16 = 2;
}

// ── Directory helpers ─────────────────────────────────────────────────────────

/// Return (and auto-create) `<XDG_CACHE_HOME>/hjkl/swap/`.
pub fn swap_dir() -> std::io::Result<PathBuf> {
    let base = hjkl_xdg::cache_dir("hjkl")
        .map_err(|e| std::io::Error::other(format!("xdg cache_dir: {e}")))?;
    let dir = base.join("swap");
    std::fs::create_dir_all(&dir)?;
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

// ── Write ─────────────────────────────────────────────────────────────────────

/// Atomically write a swap file: stream header + rope body to `<path>.tmp`,
/// fsync, then rename to `path`.
///
/// `path` is the final `.swp` path (as returned by [`swap_path_for`]).
/// `rope` body is streamed via `rope.chunks()` — no full-document allocation.
pub fn write_swap(path: &Path, header: &SwapHeader, rope: &Rope) -> std::io::Result<()> {
    let tmp = path.with_extension("swp.tmp");

    // Serialize header with postcard.
    let header_bytes = postcard::to_stdvec(header).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("postcard serialize: {e}"),
        )
    })?;

    // Write: 4-byte magic + u32-LE header length + header bytes + body chunks.
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(&SwapHeader::MAGIC)?;
    let hlen = header_bytes.len() as u32;
    f.write_all(&hlen.to_le_bytes())?;
    f.write_all(&header_bytes)?;
    for chunk in rope.chunks() {
        f.write_all(chunk.as_bytes())?;
    }
    f.sync_all()?;
    drop(f);

    std::fs::rename(&tmp, path)
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

    // Header bytes.
    let mut header_bytes = vec![0u8; hlen];
    f.read_exact(&mut header_bytes)?;
    let header: SwapHeader = postcard::from_bytes(&header_bytes).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("postcard deserialize: {e}"),
        )
    })?;

    // Body: rest of file.
    let mut body = String::new();
    f.read_to_string(&mut body)?;

    Ok((header, body))
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
/// cross-platform.  Unix uses `kill(pid, 0)` (alive on `Ok` or `EPERM`).
/// On non-unix we cannot cheaply check, so return `false` (no lock
/// enforced) — recovery still works; only the multi-instance refusal
/// is unix-only for now.
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
    #[cfg(not(unix))]
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
        let tmp = swp.with_extension("swp.tmp");

        let header = sample_header("/tmp/atomic.rs");
        let rope = Rope::from_str("data");
        write_swap(&swp, &header, &rope).unwrap();

        assert!(swp.exists(), ".swp must exist after write");
        assert!(!tmp.exists(), ".swp.tmp must not exist after rename");
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

    /// pid_is_alive returns true for the current process on unix.
    #[test]
    #[cfg(unix)]
    fn pid_is_alive_true_for_self() {
        assert!(
            pid_is_alive(std::process::id()),
            "current process must report as alive"
        );
    }

    /// A very-high pid that is almost certainly not running returns false.
    #[test]
    #[cfg(unix)]
    fn pid_is_alive_false_for_unused_pid() {
        assert!(
            !pid_is_alive(999_999_999),
            "pid 999_999_999 should not be alive"
        );
    }

    /// On non-unix, pid_is_alive always returns false (no enforcement).
    #[test]
    #[cfg(not(unix))]
    fn pid_is_alive_false_on_non_unix() {
        assert!(!pid_is_alive(std::process::id()));
        assert!(!pid_is_alive(999_999_999));
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
}
