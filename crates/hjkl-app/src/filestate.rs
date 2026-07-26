//! Cross-session file-state store — a small shada/viminfo-style index that
//! remembers the last cursor position per file so reopening a file lands you
//! back where you were.
//!
//! This is deliberately **independent** of the swap file and any future
//! undofile: it has a different validity model (best-effort, survives external
//! change by clamping) and a different lifetime (long-lived, capped LRU).
//!
//! Layout: a SINGLE capped index file at
//! `<XDG_STATE_HOME>/hjkl/filestate.bin` (falling back to `~/.local/state`),
//! keyed by a FNV-1a-64 hash of the file's canonicalized path. Kept separate
//! from the swap dir (`<XDG_CACHE_HOME>/hjkl/swap/`).
//!
//! Format:
//! - 4 bytes  magic `b"HSTA"`
//! - then a `u32` LE length prefix
//! - then a postcard-encoded [`StoreData`] `{ version, entries }`
//!
//! Fail-safe: any read/parse error — including a version mismatch, since
//! postcard is not self-describing — is treated as an empty store. Pre-1.0 we
//! do **not** migrate old versions; we simply discard and start fresh.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hjkl_fs::{WriteOptions, ensure_private_dir, with_lock_exclusive, write_atomic};
use serde::{Deserialize, Serialize};

/// Magic prefix for the file-state store.
pub const MAGIC: [u8; 4] = *b"HSTA";
/// Current format version. Bump on any incompatible schema change; old files
/// then deserialize as `Err` / version-mismatch and are discarded.
pub const VERSION: u16 = 1;
/// LRU cap: keep at most this many files (most recent by `last_seen`).
pub const CAP: usize = 500;
/// Reject a length prefix larger than this before allocating.
const MAX_LEN: u64 = 8 * 1024 * 1024;

// ── FNV-1a-64 hash ────────────────────────────────────────────────────────────

/// FNV-1a 64-bit hash — build-stable, used both for the path key and the
/// content hash. Mirrors `swap::fnv1a64` (kept local to avoid a cross-module
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

/// Content hash of a buffer's text, used to decide exact-vs-clamped restore.
/// Hash the buffer's in-memory joined string so save (buffer content) and
/// load (file content, seeded the same way) agree.
pub fn content_hash(text: &str) -> u64 {
    fnv1a64(text.as_bytes())
}

// ── Record + on-disk shape ────────────────────────────────────────────────────

/// One file's remembered state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileState {
    /// Canonicalized path — stored so a hash collision on the key can be
    /// detected (mismatched path ⇒ treat as a miss).
    pub path: String,
    /// Last-moved cursor `(row, col)`, 0-based.
    pub cursor: (u32, u32),
    /// Content hash of the file when the cursor was recorded. Lets the reader
    /// decide exact restore (match) vs clamped restore (mismatch).
    pub content_hash: u64,
    /// Wall-clock time this entry was last written, ms since UNIX epoch. Drives
    /// the LRU cap.
    pub last_seen_unix_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreData {
    version: u16,
    entries: BTreeMap<u64, FileState>,
}

// ── Directory helpers ─────────────────────────────────────────────────────────

/// Return (and auto-create) `<XDG_STATE_HOME>/hjkl/`, owner-only.
pub fn filestate_dir() -> std::io::Result<PathBuf> {
    // Cursor records are low-sensitivity, but `ensure_private_dir` keeps parity
    // with the swap dir's owner-only policy so nothing about a user's open files
    // leaks.
    let dir = hjkl_fs::dirs::state_dir("hjkl").map_err(hjkl_fs::dirs::to_io)?;
    ensure_private_dir(&dir)?;
    Ok(dir)
}

/// Stable path of the single index file: `filestate_dir()/filestate.bin`.
pub fn store_path() -> std::io::Result<PathBuf> {
    Ok(filestate_dir()?.join("filestate.bin"))
}

/// Current time as milliseconds since the UNIX epoch.
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

// ── Store ─────────────────────────────────────────────────────────────────────

/// In-memory view of the file-state index. Load, mutate, save — all fail-safe.
#[derive(Debug, Default)]
pub struct FileStateStore {
    entries: BTreeMap<u64, FileState>,
}

impl FileStateStore {
    /// Load the real store from [`store_path`]. Any error (missing file, bad
    /// magic, short read, parse failure, version mismatch) yields an empty
    /// store — never panics, never blocks.
    ///
    /// Read-only use is fine. Pairing this with [`Self::save`] to mutate the
    /// index is **not**: `filestate.bin` is shared by every hjkl instance, so an
    /// unlocked load → mutate → save loses a concurrent instance's entries. Use
    /// [`update`] (or [`record`]) for that.
    pub fn load() -> Self {
        match store_path() {
            Ok(p) => Self::load_from(&p),
            Err(_) => Self::default(),
        }
    }

    /// Load from an explicit path (test seam / redirect). Fail-safe: any
    /// problem ⇒ empty store.
    pub fn load_from(path: &Path) -> Self {
        Self::try_load_from(path).unwrap_or_default()
    }

    fn try_load_from(path: &Path) -> Option<Self> {
        let mut f = std::fs::File::open(path).ok()?;
        let mut magic = [0u8; 4];
        f.read_exact(&mut magic).ok()?;
        if magic != MAGIC {
            return None;
        }
        let mut len_buf = [0u8; 4];
        f.read_exact(&mut len_buf).ok()?;
        let len = u32::from_le_bytes(len_buf) as u64;
        if len > MAX_LEN {
            return None;
        }
        let mut body = vec![0u8; len as usize];
        f.read_exact(&mut body).ok()?;
        let data: StoreData = postcard::from_bytes(&body).ok()?;
        // Version mismatch ⇒ discard (no migration pre-1.0).
        if data.version != VERSION {
            return None;
        }
        Some(Self {
            entries: data.entries,
        })
    }

    /// Look up the remembered state for a canonicalized path. Returns `None`
    /// on a miss or a stored-path collision mismatch.
    pub fn get(&self, canonical_path: &str) -> Option<&FileState> {
        let key = fnv1a64(canonical_path.as_bytes());
        let st = self.entries.get(&key)?;
        if st.path == canonical_path {
            Some(st)
        } else {
            None
        }
    }

    /// Insert or replace the entry for `canonical_path`, stamping `last_seen`.
    pub fn upsert(
        &mut self,
        canonical_path: &str,
        cursor: (u32, u32),
        content_hash: u64,
        now_ms: u64,
    ) {
        let key = fnv1a64(canonical_path.as_bytes());
        self.entries.insert(
            key,
            FileState {
                path: canonical_path.to_string(),
                cursor,
                content_hash,
                last_seen_unix_ms: now_ms,
            },
        );
    }

    /// Drop the oldest entries (lowest `last_seen`) until at most [`CAP`]
    /// remain. Applied at save time so the on-disk index stays bounded.
    fn cap_lru(&mut self) {
        if self.entries.len() <= CAP {
            return;
        }
        let mut by_recency: Vec<(u64, u64)> = self
            .entries
            .iter()
            .map(|(k, v)| (*k, v.last_seen_unix_ms))
            .collect();
        // Newest first.
        by_recency.sort_by_key(|(_, last_seen)| std::cmp::Reverse(*last_seen));
        let keep: std::collections::HashSet<u64> =
            by_recency.into_iter().take(CAP).map(|(k, _)| k).collect();
        self.entries.retain(|k, _| keep.contains(k));
    }

    /// Save to the real [`store_path`]. Errors are returned but callers treat
    /// persistence as best-effort and ignore them.
    ///
    /// Takes no lock, so it is only safe on a store that was loaded under one —
    /// i.e. from inside [`update`]. See [`Self::load`].
    pub fn save(&mut self) -> std::io::Result<()> {
        let p = store_path()?;
        self.save_to(&p)
    }

    /// Save to an explicit path (test seam / redirect). Applies the LRU cap,
    /// then writes atomically through the [`hjkl_fs`] seam.
    ///
    /// This is the *store* half of a read-modify-write; on its own it does not
    /// make [`record`] safe against a concurrent instance. That is why `record`
    /// holds the store's lock across load → upsert → save.
    pub fn save_to(&mut self, path: &Path) -> std::io::Result<()> {
        self.cap_lru();
        let data = StoreData {
            version: VERSION,
            entries: self.entries.clone(),
        };
        let body = postcard::to_stdvec(&data).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("postcard serialize: {e}"),
            )
        })?;
        // `WriteOptions::state()` has no non-atomic fallback, so the parent must
        // exist for the temp file to be created beside the target.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // magic + u32-LE body length + postcard body — the format is unchanged.
        let mut bytes = Vec::with_capacity(MAGIC.len() + 4 + body.len());
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&body);
        write_atomic(path, &bytes, &WriteOptions::state())
    }

    /// Number of entries — test/introspection helper.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ── Convenience free functions ────────────────────────────────────────────────

/// Look up the remembered [`FileState`] for a canonicalized path in the real
/// store. Loads the whole (small) index; fail-safe to `None`.
pub fn lookup(canonical_path: &str) -> Option<FileState> {
    FileStateStore::load().get(canonical_path).cloned()
}

/// Mutate the real store under its exclusive lock: load → `mutate` → save, all
/// inside one lock.
///
/// This is the ONLY correct way to change the index. `filestate.bin` is a SINGLE
/// file shared by every hjkl instance, so an unlocked read-modify-write loses
/// updates: two instances both load the same snapshot, both mutate their own
/// copy, and the second atomic rename silently discards the first's entries. An
/// atomic write does not fix that — only a lock spanning the whole sequence does,
/// which is why the lock is taken here rather than inside
/// [`FileStateStore::save`].
///
/// [`FileStateStore::load`] deliberately stays lock-free: it keeps a documented
/// "never panics, never blocks" contract, and a stale read is harmless when
/// nothing is written back.
pub fn update<F>(mutate: F) -> std::io::Result<()>
where
    F: FnOnce(&mut FileStateStore),
{
    let path = store_path()?;
    with_lock_exclusive(&path, || {
        let mut store = FileStateStore::load_from(&path);
        mutate(&mut store);
        store.save_to(&path)
    })
}

/// Record a single file's cursor into the real store (load → upsert → save under
/// the store lock). Best-effort: any I/O error is swallowed.
pub fn record(canonical_path: &str, cursor: (u32, u32), content_hash: u64) {
    let _ = update(|store| {
        store.upsert(canonical_path, cursor, content_hash, now_unix_ms());
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn store_file(td: &tempfile::TempDir) -> PathBuf {
        td.path().join("filestate.bin")
    }

    #[test]
    fn roundtrip_write_read_same_cursor() {
        let td = tempfile::tempdir().unwrap();
        let p = store_file(&td);

        let mut store = FileStateStore::default();
        store.upsert("/home/u/a.rs", (50, 20), 0xabcd, 1_000);
        store.save_to(&p).unwrap();

        let loaded = FileStateStore::load_from(&p);
        let st = loaded.get("/home/u/a.rs").expect("entry present");
        assert_eq!(st.cursor, (50, 20));
        assert_eq!(st.content_hash, 0xabcd);
        assert_eq!(st.path, "/home/u/a.rs");
    }

    /// Format lock: magic + `u32`-LE body length + postcard body, built here
    /// independently of `save_to`. `try_load_from` parses exactly this shape, so a
    /// layout change would discard every existing index.
    #[test]
    fn on_disk_layout_is_byte_exact() {
        let td = tempfile::tempdir().unwrap();
        let p = store_file(&td);

        let mut store = FileStateStore::default();
        store.upsert("/home/u/layout.rs", (7, 3), 0x1234, 9_999);
        store.save_to(&p).unwrap();

        let body = postcard::to_stdvec(&StoreData {
            version: VERSION,
            entries: store.entries.clone(),
        })
        .unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(&MAGIC);
        expected.extend_from_slice(&(body.len() as u32).to_le_bytes());
        expected.extend_from_slice(&body);

        assert_eq!(std::fs::read(&p).unwrap(), expected);
    }

    /// The RMW that [`record`] performs, driven concurrently against one shared
    /// index under the same lock `record` takes. Without the lock spanning
    /// load → upsert → save, threads overwrite each other's entries and far fewer
    /// than `N` survive; with it, every entry must be present.
    #[test]
    fn locked_read_modify_write_keeps_every_concurrent_entry() {
        let td = tempfile::tempdir().unwrap();
        let p = std::sync::Arc::new(store_file(&td));

        const N: usize = 16;
        let mut handles = Vec::new();
        for i in 0..N {
            let p = std::sync::Arc::clone(&p);
            handles.push(std::thread::spawn(move || {
                // Mirrors `record`'s body exactly: the lock spans the whole
                // load → upsert → save, not just the save.
                hjkl_fs::with_lock_exclusive(&p, || {
                    let mut store = FileStateStore::load_from(&p);
                    store.upsert(&format!("/f{i}"), (i as u32, 0), i as u64, 1_000 + i as u64);
                    store.save_to(&p)
                })
                .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let loaded = FileStateStore::load_from(&p);
        assert_eq!(loaded.len(), N, "an interleaved write lost entries");
        for i in 0..N {
            assert!(
                loaded.get(&format!("/f{i}")).is_some(),
                "/f{i} was overwritten by a concurrent record()"
            );
        }
    }

    #[test]
    fn miss_returns_none() {
        let store = FileStateStore::default();
        assert!(store.get("/nope").is_none());
    }

    #[test]
    fn upsert_replaces_existing() {
        let mut store = FileStateStore::default();
        store.upsert("/f", (1, 1), 1, 10);
        store.upsert("/f", (2, 2), 2, 20);
        assert_eq!(store.len(), 1);
        let st = store.get("/f").unwrap();
        assert_eq!(st.cursor, (2, 2));
        assert_eq!(st.last_seen_unix_ms, 20);
    }

    #[test]
    fn lru_cap_drops_oldest() {
        let td = tempfile::tempdir().unwrap();
        let p = store_file(&td);

        let mut store = FileStateStore::default();
        // Insert CAP + 10 files with increasing last_seen.
        for i in 0..(CAP + 10) {
            store.upsert(&format!("/f{i}"), (i as u32, 0), 0, i as u64);
        }
        store.save_to(&p).unwrap();

        let loaded = FileStateStore::load_from(&p);
        assert_eq!(loaded.len(), CAP, "must be capped to CAP");
        // The 10 oldest (/f0../f9) must have been dropped.
        assert!(loaded.get("/f0").is_none(), "oldest dropped");
        assert!(loaded.get("/f9").is_none(), "oldest dropped");
        // The newest survive.
        assert!(loaded.get(&format!("/f{}", CAP + 9)).is_some());
    }

    #[test]
    fn bad_magic_is_empty() {
        let td = tempfile::tempdir().unwrap();
        let p = store_file(&td);
        std::fs::write(&p, b"XXXXsomegarbage").unwrap();
        let loaded = FileStateStore::load_from(&p);
        assert!(loaded.is_empty(), "bad magic ⇒ empty");
    }

    #[test]
    fn missing_file_is_empty() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("does-not-exist.bin");
        assert!(FileStateStore::load_from(&p).is_empty());
    }

    #[test]
    fn version_mismatch_is_empty() {
        let td = tempfile::tempdir().unwrap();
        let p = store_file(&td);
        // Hand-craft a store with a bogus version.
        let data = StoreData {
            version: VERSION + 1,
            entries: BTreeMap::new(),
        };
        let body = postcard::to_stdvec(&data).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&body);
        std::fs::write(&p, &bytes).unwrap();

        assert!(
            FileStateStore::load_from(&p).is_empty(),
            "version mismatch ⇒ discarded"
        );
    }

    #[test]
    fn truncated_body_is_empty() {
        let td = tempfile::tempdir().unwrap();
        let p = store_file(&td);
        // Valid magic + length claiming 100 bytes but no body.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&100u32.to_le_bytes());
        std::fs::write(&p, &bytes).unwrap();
        assert!(FileStateStore::load_from(&p).is_empty());
    }

    #[test]
    fn hash_collision_path_mismatch_is_miss() {
        // Two entries can't collide in practice; simulate by inserting one and
        // querying a different path that (deliberately) is not it.
        let mut store = FileStateStore::default();
        store.upsert("/real/path", (3, 4), 0, 1);
        assert!(store.get("/other/path").is_none());
    }

    #[test]
    fn content_hash_is_stable() {
        assert_eq!(content_hash("hello\nworld"), content_hash("hello\nworld"));
        assert_ne!(content_hash("a"), content_hash("b"));
    }
}
