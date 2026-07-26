//! Persistent undo store — the `undofile`.
//!
//! On `:w` the whole delta-encoded undo tree (from `hjkl-buffer`) is serialized
//! next to the file's identity so that reopening the same, unchanged file
//! restores the exact node it was saved on, with `<C-r>` still able to walk the
//! retained forward branch. Because nodes hold **deltas** (not full ropes) the
//! file is compact.
//!
//! Mirrors the two sibling on-disk stores:
//! - [`crate::swap`] (`HSWP`) — atomic temp+rename, `fnv1a64` path hash,
//!   postcard body, 0700 dir / 0600 file, fail-safe read.
//! - [`crate::filestate`] (`HSTA`) — magic + length-prefixed postcard, version
//!   gate, XDG **state** dir.
//!
//! Layout: one file per document at `<undodir>/<fnv1a64(path):016x>.und`, where
//! `<undodir>` defaults to `<XDG_STATE_HOME>/hjkl/undo/` (kept separate from the
//! swap `.swp` files and the single `filestate.bin` index). An `undodir`
//! setting overrides the base directory.
//!
//! Format:
//! - 4 bytes  magic `b"HUND"`
//! - `u16` LE `format_version`
//! - `u64` LE `content_hash`      — FNV-1a-64 of the on-disk file == current node
//! - `u64` LE `file_size`         — cheap pre-check
//! - `u64` LE `file_mtime_unix_ms`— cheap pre-check; not authoritative
//! - `u64` LE `current_seq`       — the node the buffer was on at save
//! - `u32` LE body length
//! - postcard-encoded [`hjkl_buffer::SerTree`] body
//!
//! Integrity: like swap, we rely on postcard being non-self-describing — a
//! version/schema drift or truncation surfaces as a parse `Err` on read, which
//! (with the fixed-header length caps) is treated as "no usable undofile". Every
//! read/parse failure — bad magic, version mismatch, short read, corrupt body,
//! hash mismatch handled by the caller — degrades safely to a fresh tree; the
//! worst case is "no cross-session undo", never a corrupted buffer. Pre-1.0 we
//! do **not** migrate old versions: a `format_version` bump makes old files
//! parse as `Err` and be discarded.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use hjkl_buffer::SerTree;

/// Magic prefix for the undofile.
pub const MAGIC: [u8; 4] = *b"HUND";
/// Current format version. Bump on any incompatible schema change; old files
/// then fail the version gate (or parse as `Err`) and are discarded.
pub const VERSION: u16 = 1;
/// Fixed-header byte length: magic(4) + version(2) + 4×u64 + body-len(4).
const HEADER_LEN: usize = 4 + 2 + 8 * 4 + 4;
/// Reject a body length prefix larger than this before allocating (a 1 MB
/// buffer's full delta tree is comfortably under this).
const MAX_BODY_LEN: u64 = 256 * 1024 * 1024;

// ── FNV-1a-64 hash ────────────────────────────────────────────────────────────

/// FNV-1a 64-bit hash — build-stable, used for the path key. Mirrors
/// `swap::fnv1a64` / `filestate::fnv1a64` (kept local to avoid a cross-module
/// dependency for a 6-line function).
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

// ── Directory helpers ─────────────────────────────────────────────────────────

/// Return (and auto-create) the undo directory, owner-only. Defaults to
/// `<XDG_STATE_HOME>/hjkl/undo/`; `override_dir` (the `undodir` setting) replaces
/// the whole path when set.
pub fn undofile_dir(override_dir: Option<&Path>) -> std::io::Result<PathBuf> {
    let dir = match override_dir {
        Some(d) => d.to_path_buf(),
        None => hjkl_fs::dirs::state_dir("hjkl")
            .map_err(hjkl_fs::dirs::to_io)?
            .join("undo"),
    };
    // Undofiles embed the full buffer history (potentially credentials, private
    // keys, etc.). `ensure_private_dir` keeps the directory owner-only, matching
    // swap — including when an `undodir` override points at a looser directory.
    hjkl_fs::ensure_private_dir(&dir)?;
    Ok(dir)
}

/// Stable undofile path for a file: `undofile_dir()/<hash16>.und`.
///
/// `canonical_path` should be an already-canonicalized absolute path. The hash
/// is FNV-1a-64 over the path string — build-stable, cross-platform, and kept
/// distinct from the swap `.swp` name space.
pub fn undofile_path_for(
    canonical_path: &Path,
    override_dir: Option<&Path>,
) -> std::io::Result<PathBuf> {
    let hash = fnv1a64(canonical_path.to_string_lossy().as_bytes());
    Ok(undofile_dir(override_dir)?.join(format!("{hash:016x}.und")))
}

// ── Loaded record ─────────────────────────────────────────────────────────────

/// A successfully-read undofile: the persisted tree plus the identity fields the
/// caller gates on (content hash / size / mtime) before installing it.
#[derive(Debug, Clone)]
pub struct LoadedUndo {
    /// FNV-1a-64 of the file contents when the undofile was written (== the
    /// saved current node). The caller compares this to `hash(disk)`.
    pub content_hash: u64,
    /// File size at save time (cheap pre-check).
    pub file_size: u64,
    /// File mtime at save time, ms since UNIX epoch (cheap pre-check).
    pub file_mtime_unix_ms: u64,
    /// `seq` of the node the buffer was on at save.
    pub current_seq: u64,
    /// The deserialized undo tree projection.
    pub tree: SerTree,
}

// ── Write ─────────────────────────────────────────────────────────────────────

/// Serialize `tree` and write the undofile for `canonical_path` atomically
/// (temp file + fsync + rename, via [`hjkl_fs`]). `content_hash` / `file_size` /
/// `file_mtime_unix_ms` describe the on-disk file at save time (== the current
/// node); `current_seq` is read from the tree's current node.
///
/// Errors are returned but callers treat persistence as best-effort and ignore
/// them (a failed undofile write must never fail a `:w`).
pub fn write(
    canonical_path: &Path,
    tree: &SerTree,
    content_hash: u64,
    file_size: u64,
    file_mtime_unix_ms: u64,
    override_dir: Option<&Path>,
) -> std::io::Result<()> {
    let path = undofile_path_for(canonical_path, override_dir)?;
    let current_seq = tree.nodes.get(tree.current as usize).map_or(0, |n| n.seq);
    let body = postcard::to_stdvec(tree).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("postcard serialize: {e}"),
        )
    })?;

    // Fixed header + u32-LE body length + postcard body — the format is
    // unchanged. The closure is `Fn` (it may be re-run on a temp-name collision)
    // and borrows everything it writes, so each call emits identical bytes.
    //
    // Exclusive lock for the whole write: two instances saving the same file
    // would otherwise interleave, and a reader in a third could parse a
    // half-replaced file. Re-entrant, so a caller holding this path's lock
    // across a wider sequence passes through.
    hjkl_fs::with_lock_exclusive(&path, || {
        hjkl_fs::write_atomic_with(&path, &hjkl_fs::WriteOptions::state(), |f| {
            f.write_all(&MAGIC)?;
            f.write_all(&VERSION.to_le_bytes())?;
            f.write_all(&content_hash.to_le_bytes())?;
            f.write_all(&file_size.to_le_bytes())?;
            f.write_all(&file_mtime_unix_ms.to_le_bytes())?;
            f.write_all(&current_seq.to_le_bytes())?;
            f.write_all(&(body.len() as u32).to_le_bytes())?;
            f.write_all(&body)?;
            Ok(())
        })
    })
}

// ── Read ──────────────────────────────────────────────────────────────────────

/// Read the undofile for `canonical_path`. Fail-safe: any problem — missing
/// file, bad magic, version mismatch, short read, oversized/corrupt body —
/// yields `None`. Never panics, never blocks. The caller still gates on
/// `content_hash` before installing the tree.
pub fn read(canonical_path: &Path, override_dir: Option<&Path>) -> Option<LoadedUndo> {
    let path = undofile_path_for(canonical_path, override_dir).ok()?;
    // Lock the *resolved* path — the sidecar guards the `.und` file, not the
    // document. The nested lock inside `read_from` is re-entrant.
    hjkl_fs::with_lock_shared(&path, || Ok(read_from(&path)))
        .ok()
        .flatten()
}

/// Read from an explicit undofile path (test seam / redirect). Fail-safe.
///
/// Takes the path's shared lock so a concurrent instance's `write` (which holds
/// the exclusive lock across its temp+rename) cannot be observed half-applied.
/// A lock failure degrades to `None`, like every other read failure here.
pub fn read_from(path: &Path) -> Option<LoadedUndo> {
    hjkl_fs::with_lock_shared(path, || Ok(read_from_locked(path)))
        .ok()
        .flatten()
}

/// The parse half of [`read_from`], run with `path`'s lock already held.
fn read_from_locked(path: &Path) -> Option<LoadedUndo> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut hdr = [0u8; HEADER_LEN];
    f.read_exact(&mut hdr).ok()?;
    if hdr[0..4] != MAGIC {
        return None;
    }
    let version = u16::from_le_bytes(hdr[4..6].try_into().ok()?);
    if version != VERSION {
        return None; // no migration pre-1.0
    }
    let content_hash = u64::from_le_bytes(hdr[6..14].try_into().ok()?);
    let file_size = u64::from_le_bytes(hdr[14..22].try_into().ok()?);
    let file_mtime_unix_ms = u64::from_le_bytes(hdr[22..30].try_into().ok()?);
    let current_seq = u64::from_le_bytes(hdr[30..38].try_into().ok()?);
    let body_len = u32::from_le_bytes(hdr[38..42].try_into().ok()?) as u64;
    if body_len > MAX_BODY_LEN {
        return None;
    }
    let mut body = vec![0u8; body_len as usize];
    f.read_exact(&mut body).ok()?;
    let tree: SerTree = postcard::from_bytes(&body).ok()?;
    Some(LoadedUndo {
        content_hash,
        file_size,
        file_mtime_unix_ms,
        current_seq,
        tree,
    })
}

// ── Remove ────────────────────────────────────────────────────────────────────

/// Delete an undofile. Silently succeeds when the file is absent.
pub fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_buffer::{Delta, SerNode, SerTree};

    /// Build a tiny 3-node projection: root "a" → n1 "ab" → n2 "abc", current
    /// on n1. Content-agnostic: the round-trip only needs a valid `SerTree`.
    fn sample_tree(current: u32) -> SerTree {
        SerTree {
            base: "a".to_string(),
            nodes: vec![
                SerNode {
                    parent: None,
                    children: vec![1],
                    last_child: Some(1),
                    delta: None,
                    cursor: (0, 0),
                    timestamp_unix_ms: 1000,
                    marks: Default::default(),
                    seq: 0,
                },
                SerNode {
                    parent: Some(0),
                    children: vec![2],
                    last_child: Some(2),
                    delta: Some(Delta {
                        start: 1,
                        old: String::new(),
                        new: "b".to_string(),
                    }),
                    cursor: (0, 1),
                    timestamp_unix_ms: 2000,
                    marks: Default::default(),
                    seq: 1,
                },
                SerNode {
                    parent: Some(1),
                    children: vec![],
                    last_child: None,
                    delta: Some(Delta {
                        start: 2,
                        old: String::new(),
                        new: "c".to_string(),
                    }),
                    cursor: (0, 2),
                    timestamp_unix_ms: 3000,
                    marks: Default::default(),
                    seq: 2,
                },
            ],
            root: 0,
            current,
            next_seq: 3,
        }
    }

    fn undofile_at(td: &tempfile::TempDir) -> PathBuf {
        td.path().join("test.und")
    }

    #[test]
    fn write_read_round_trip() {
        let td = tempfile::tempdir().unwrap();
        let p = undofile_at(&td);
        let tree = sample_tree(1);
        // Write via the atomic writer into an explicit dir, keyed by a path.
        let canon = Path::new("/home/u/a.rs");
        write(canon, &tree, 0xDEAD_BEEF, 3, 42_000, Some(td.path())).unwrap();
        // Re-read from the derived path and compare identity + structure.
        let loaded = read(canon, Some(td.path())).expect("round-trips");
        assert_eq!(loaded.content_hash, 0xDEAD_BEEF);
        assert_eq!(loaded.file_size, 3);
        assert_eq!(loaded.file_mtime_unix_ms, 42_000);
        assert_eq!(loaded.current_seq, 1, "current node seq in header");
        assert_eq!(loaded.tree.nodes.len(), 3);
        assert_eq!(loaded.tree.base, "a");
        assert_eq!(loaded.tree.current, 1);
        assert_eq!(loaded.tree.nodes[2].delta.as_ref().unwrap().new, "c");
        // `p` is unused as a real target here but proves the helper compiles.
        let _ = p;
    }

    #[test]
    fn write_is_atomic_no_tmp_left() {
        let td = tempfile::tempdir().unwrap();
        let canon = Path::new("/home/u/atomic.rs");
        write(canon, &sample_tree(2), 1, 1, 1, Some(td.path())).unwrap();
        let has_tmp = std::fs::read_dir(td.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().is_some_and(|x| x == "tmp"));
        assert!(!has_tmp, "no .tmp files should remain after write");
        let has_und = std::fs::read_dir(td.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().is_some_and(|x| x == "und"));
        assert!(has_und, "the .und file must exist after write");
    }

    #[test]
    fn header_content_hash_gate_values_survive() {
        let td = tempfile::tempdir().unwrap();
        let canon = Path::new("/home/u/gate.rs");
        write(
            canon,
            &sample_tree(0),
            0x1234_5678_9ABC_DEF0,
            99,
            7,
            Some(td.path()),
        )
        .unwrap();
        let loaded = read(canon, Some(td.path())).unwrap();
        // The caller compares loaded.content_hash to hash(disk); prove the exact
        // value round-trips so an accept/reject decision is trustworthy.
        assert_eq!(loaded.content_hash, 0x1234_5678_9ABC_DEF0);
        assert_ne!(loaded.content_hash, 0x1234_5678_9ABC_DEF1);
    }

    /// Format lock: the exact bytes [`write`] produces, built here independently
    /// of the writer. [`read_from`] parses this layout by fixed offsets, so a
    /// reordered or resized field would silently discard every undofile written
    /// by an older build — this test fails instead.
    #[test]
    fn on_disk_layout_is_byte_exact() {
        let td = tempfile::tempdir().unwrap();
        let canon = Path::new("/home/u/layout.rs");
        let tree = sample_tree(1);
        write(
            canon,
            &tree,
            0x0102_0304_0506_0708,
            77,
            1_234_567,
            Some(td.path()),
        )
        .unwrap();

        let body = postcard::to_stdvec(&tree).unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(&MAGIC);
        expected.extend_from_slice(&VERSION.to_le_bytes());
        expected.extend_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
        expected.extend_from_slice(&77u64.to_le_bytes());
        expected.extend_from_slice(&1_234_567u64.to_le_bytes());
        // current_seq: tree.nodes[current == 1].seq == 1.
        expected.extend_from_slice(&1u64.to_le_bytes());
        expected.extend_from_slice(&(body.len() as u32).to_le_bytes());
        expected.extend_from_slice(&body);
        assert_eq!(expected.len(), HEADER_LEN + body.len());

        let path = undofile_path_for(canon, Some(td.path())).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), expected);
    }

    #[test]
    fn missing_file_is_none() {
        let td = tempfile::tempdir().unwrap();
        assert!(read_from(&td.path().join("nope.und")).is_none());
    }

    #[test]
    fn bad_magic_is_none() {
        let td = tempfile::tempdir().unwrap();
        let p = undofile_at(&td);
        std::fs::write(&p, b"XXXX and some more garbage bytes here padding").unwrap();
        assert!(read_from(&p).is_none());
    }

    #[test]
    fn version_bump_is_none() {
        let td = tempfile::tempdir().unwrap();
        let p = undofile_at(&td);
        // Hand-craft a header with a bogus version but otherwise plausible.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&(VERSION + 1).to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes()); // content_hash
        bytes.extend_from_slice(&0u64.to_le_bytes()); // file_size
        bytes.extend_from_slice(&0u64.to_le_bytes()); // mtime
        bytes.extend_from_slice(&0u64.to_le_bytes()); // current_seq
        bytes.extend_from_slice(&0u32.to_le_bytes()); // body len
        std::fs::write(&p, &bytes).unwrap();
        assert!(read_from(&p).is_none(), "version mismatch ⇒ discarded");
    }

    #[test]
    fn truncated_body_is_none() {
        let td = tempfile::tempdir().unwrap();
        let p = undofile_at(&td);
        // Valid header claiming 500 body bytes, but none follow.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&500u32.to_le_bytes());
        std::fs::write(&p, &bytes).unwrap();
        assert!(read_from(&p).is_none(), "short body ⇒ discarded");
    }

    #[test]
    fn corrupt_body_bytes_is_none() {
        let td = tempfile::tempdir().unwrap();
        let p = undofile_at(&td);
        // Valid header + a body length matching the payload, but the payload is
        // not a valid postcard SerTree ⇒ parse Err ⇒ None.
        let junk = b"not a valid postcard SerTree payload!!";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&(junk.len() as u32).to_le_bytes());
        bytes.extend_from_slice(junk);
        std::fs::write(&p, &bytes).unwrap();
        assert!(read_from(&p).is_none(), "corrupt body ⇒ discarded");
    }

    #[test]
    fn oversized_body_len_is_none() {
        let td = tempfile::tempdir().unwrap();
        let p = undofile_at(&td);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // 4 GiB body claim
        std::fs::write(&p, &bytes).unwrap();
        assert!(read_from(&p).is_none(), "oversized body-len rejected");
    }

    #[test]
    fn path_hash_is_stable_and_distinct() {
        let td = tempfile::tempdir().unwrap();
        let a1 = undofile_path_for(Path::new("/x/a.rs"), Some(td.path())).unwrap();
        let a2 = undofile_path_for(Path::new("/x/a.rs"), Some(td.path())).unwrap();
        let b = undofile_path_for(Path::new("/x/b.rs"), Some(td.path())).unwrap();
        assert_eq!(a1, a2, "same path ⇒ same undofile");
        assert_ne!(a1, b, "different path ⇒ different undofile");
        assert!(a1.extension().is_some_and(|e| e == "und"));
    }

    #[test]
    fn remove_ignores_missing() {
        let td = tempfile::tempdir().unwrap();
        assert!(remove(&td.path().join("gone.und")).is_ok());
    }
}
