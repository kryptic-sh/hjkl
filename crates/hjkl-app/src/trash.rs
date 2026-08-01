//! Trash-directory helper — safe "delete" for the oil-style explorer.
//!
//! Deleted filesystem entries are moved to `<XDG_CACHE_HOME>/hjkl/trash/`
//! rather than permanently removed, matching the swap-dir pattern from
//! [`crate::swap`].
//!
//! Where the trash lives is a parameter, not a lookup: [`TrashRoot`] is either
//! `Xdg` (the production default, resolved on every use) or an explicit
//! directory. Every helper has an `_in` variant taking one. Tests use the
//! explicit variant rather than overriding `XDG_CACHE_HOME` — that variable is
//! process-global, so an override is visible to every thread in the binary and
//! not just to the test that set it.
//!
//! # Growth
//!
//! Nothing here ever reclaims the trash directory. There is no reaper, no
//! age-based expiry and no size cap: every explorer deletion adds an entry
//! under a fresh monotonic `.N` suffix and leaves it there forever. Clearing
//! `<XDG_CACHE_HOME>/hjkl/trash/` is the user's to do — deliberately, since the
//! whole point of trashing rather than deleting is that the editor does not get
//! to decide when the data is gone.
//!
//! The one consequence to be aware of: [`trash_path`] probes at most
//! `MAX_RETRIES` (1000) counter slots. Slots are never reused, so the 1001st
//! deletion of a same-named file fails with an error rather than recycling an
//! older entry. That is the intended failure mode — a full trash surfaces as a
//! failed delete, not as a silent overwrite of something the user still has.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

// ── Where the trash lives ─────────────────────────────────────────────────────

/// Which directory trashed entries are moved into.
///
/// Production uses [`TrashRoot::Xdg`], which resolves `<XDG_CACHE_HOME>/hjkl/
/// trash/` on every use exactly as before. [`TrashRoot::At`] names the
/// directory outright, which is what lets a test point the trash at its own
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
/// Same shape as [`hjkl_anvil::store::AnvilPaths`], for the same reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TrashRoot {
    /// `<XDG_CACHE_HOME>/hjkl/trash/`, resolved on each use — the production
    /// default.
    #[default]
    Xdg,
    /// An explicit directory, created on first use with the same owner-only
    /// mode as the XDG one.
    At(PathBuf),
}

impl TrashRoot {
    /// Resolve to a concrete directory, creating it (owner-only) if needed.
    ///
    /// Both variants go through [`hjkl_fs::ensure_private_dir`], so the
    /// permissions of an injected root match the XDG one — a trashed file holds
    /// whatever the user was deleting, and that is not for other local users to
    /// read in either case.
    pub fn dir(&self) -> std::io::Result<PathBuf> {
        match self {
            Self::Xdg => hjkl_fs::private_cache_subdir("hjkl", "trash"),
            Self::At(dir) => {
                hjkl_fs::ensure_private_dir(dir)?;
                Ok(dir.clone())
            }
        }
    }
}

// ── Directory helper ──────────────────────────────────────────────────────────

/// Return (and auto-create) `<XDG_CACHE_HOME>/hjkl/trash/`.
///
/// Resolution mirrors [`crate::swap::swap_dir`] — the same
/// `hjkl_fs::private_cache_subdir` call, so `$XDG_CACHE_HOME` resolution,
/// creation and the owner-only mode are decided in one place.
///
/// Shorthand for `TrashRoot::Xdg.dir()`.
pub fn trash_dir() -> std::io::Result<PathBuf> {
    TrashRoot::Xdg.dir()
}

// ── Unique destination path ───────────────────────────────────────────────────

/// [`trash_path`] against an explicit [`TrashRoot`].
///
/// The `_in` half of the pair, mirroring `hjkl_anvil::store`'s `*_in` helpers:
/// every behaviour below is identical, only the directory is named by the
/// caller instead of read off the process environment.
pub fn trash_path_in(root: &TrashRoot, original: &Path, is_dir: bool) -> std::io::Result<PathBuf> {
    let dir = root.dir()?;

    // Build a base name from the original file name (fall back to "unnamed").
    let base_name = original.file_name().map_or_else(
        || "unnamed".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );

    // Atomically reserve the first free counter slot.
    const MAX_RETRIES: u64 = 1000;
    let mut counter: u64 = 0;
    loop {
        let candidate_name = format!("{base_name}.{counter}");
        let candidate = dir.join(&candidate_name);

        let result = if is_dir {
            std::fs::create_dir(&candidate).map(|_| ())
        } else {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
                .map(|_| ())
        };

        match result {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                counter += 1;
                if counter > MAX_RETRIES {
                    return Err(std::io::Error::other(format!(
                        "trash_path: exhausted {MAX_RETRIES} retries for {candidate:?}"
                    )));
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// Return a unique path inside [`trash_dir`] for `original`, atomically
/// reserved so that two concurrent callers never select the same destination.
///
/// The returned path has the form `<trash>/<stem>.<ext>.<n>` where `<n>` is
/// the lowest non-negative integer for which the reservation succeeds.
///
/// For **files** (`is_dir == false`), a 0-byte placeholder file is created
/// with `create_new` (O_EXCL) and left in place.  The caller's `rename`
/// atomically replaces it — no TOCTOU window.
///
/// For **directories** (`is_dir == true`), an empty directory is created with
/// `create_dir` (atomic mkdir).  The caller's `rename` of a source directory
/// onto an empty target directory succeeds on Linux and leaves no window.
///
/// **Does NOT move anything** — the caller is responsible for the actual
/// filesystem operation.
///
/// Shorthand for `trash_path_in(&TrashRoot::Xdg, …)`.
pub fn trash_path(original: &Path, is_dir: bool) -> std::io::Result<PathBuf> {
    trash_path_in(&TrashRoot::Xdg, original, is_dir)
}

// ── Moving the entry in ───────────────────────────────────────────────────────

/// Move `original` into the `dest` slot [`trash_path`] reserved for it.
///
/// The trash lives under `$XDG_CACHE_HOME`; the entry being deleted can be
/// anywhere. So this move regularly crosses a filesystem boundary — a separate
/// `/home`, a mounted volume, an editor started in `/tmp` — where `rename` fails
/// outright with `EXDEV` and the obvious fallback (copy into the trash slot, then
/// delete the original) is precisely the shape that loses the user's data if it
/// dies halfway through.
///
/// [`hjkl_fs::move_atomic`] is the seam's answer: `rename` when it can, and
/// otherwise a copy staged beside the destination that is swapped in only once it
/// is complete, with the original unlinked only after that. The point of trashing
/// something rather than deleting it is not losing it, and that ordering is the
/// whole guarantee.
///
/// It also handles all three shapes the explorer can hand it — file, directory
/// tree, symlink — so the call site no longer has to pick a mover by type. A
/// symlink is moved as a symlink; the data it points at is neither copied nor
/// deleted.
///
/// The reservation made by [`trash_path`] is left intact: the placeholder (a
/// 0-byte file, or an empty directory) is replaced by a single `rename` on both
/// the same-device and the cross-device path, so the slot is never briefly free
/// for a concurrent caller to claim.
pub fn move_to_trash(original: &Path, dest: &Path) -> std::io::Result<()> {
    // Durable by default: a trashed file is the one thing the user might want
    // back after a crash.
    hjkl_fs::move_atomic(original, dest, &hjkl_fs::WriteOptions::default())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the one remaining env-mutating test. `XDG_CACHE_HOME` is
    /// process-global; under `cargo test`'s thread pool (as opposed to
    /// nextest's process-per-test) two such tests otherwise race on it and fail
    /// nondeterministically. Hold the returned guard for the whole test.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A private trash root under a fresh `TempDir`, with **no** environment
    /// mutation. Keep the `TempDir` in scope so it is not deleted while in use.
    ///
    /// This is the shape every test below one wants: `XDG_CACHE_HOME` is
    /// process-global, so overriding it leaks into every other thread in this
    /// binary for the duration, and a `Mutex` only holds back the tests that
    /// agreed to take it. Naming the directory outright cannot collide.
    fn isolated_trash_root() -> (tempfile::TempDir, TrashRoot) {
        let td = tempfile::tempdir().unwrap();
        let root = TrashRoot::At(td.path().join("hjkl").join("trash"));
        (td, root)
    }

    /// The one place `XDG_CACHE_HOME` still has to be set: it is the thing
    /// under test. [`TrashRoot::Xdg`] must resolve to `<XDG_CACHE_HOME>/hjkl/
    /// trash` and create it — every other test below uses an explicit root and
    /// never touches the environment.
    #[test]
    fn trash_dir_creates_directory() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let td = tempfile::tempdir().unwrap();
        // SAFETY: the ENV_LOCK guard serializes this against the only other
        // env-mutating test in this module; it is restored below.
        let prev = std::env::var_os("XDG_CACHE_HOME");
        unsafe {
            std::env::set_var("XDG_CACHE_HOME", td.path());
        }
        let got = trash_dir().expect("trash_dir must succeed");
        // SAFETY: same guard still held.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
                None => std::env::remove_var("XDG_CACHE_HOME"),
            }
        }
        assert_eq!(
            got,
            td.path().join("hjkl").join("trash"),
            "trash_dir must match XDG_CACHE_HOME/hjkl/trash"
        );
        assert!(got.is_dir(), "trash_dir must create the directory");
    }

    /// An explicit root is created on first use, owner-only, exactly where it
    /// was named — and nowhere near `XDG_CACHE_HOME`.
    #[test]
    fn explicit_trash_root_creates_the_named_directory() {
        let (td, root) = isolated_trash_root();
        let got = root.dir().expect("explicit root must resolve");
        assert_eq!(
            got,
            td.path().join("hjkl").join("trash"),
            "TrashRoot::At must resolve to exactly the directory it names"
        );
        assert!(got.is_dir(), "TrashRoot::At must create the directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&got).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o700,
                "an injected trash root must be owner-only like the XDG one"
            );
        }
    }

    #[test]
    fn trash_path_is_unique_across_two_calls_for_same_name() {
        let (_td, root) = isolated_trash_root();
        let fake_original = Path::new("/some/project/foo.rs");

        // First call — nothing in trash yet, should get counter 0.
        let first =
            trash_path_in(&root, fake_original, false).expect("first trash_path must succeed");
        assert!(
            first.to_string_lossy().ends_with(".0"),
            "first trash_path must end with .0, got {first:?}"
        );

        // Simulate the file being moved there: create a dummy entry.
        std::fs::write(&first, b"dummy content").expect("creating dummy trash entry must succeed");

        // Second call — counter 0 is taken, must get counter 1.
        let second =
            trash_path_in(&root, fake_original, false).expect("second trash_path must succeed");
        assert!(
            second.to_string_lossy().ends_with(".1"),
            "second trash_path must end with .1, got {second:?}"
        );
        assert_ne!(first, second, "trash_path must return distinct paths");
    }

    #[test]
    fn trash_path_does_not_collide_with_gaps() {
        let (_td, root) = isolated_trash_root();
        let fake_original = Path::new("/home/user/gap_test.txt");

        // Pre-populate counters 0 and 2 (leaving 1 absent).
        let dir = root.dir().unwrap();
        std::fs::write(dir.join("gap_test.txt.0"), b"x").unwrap();
        std::fs::write(dir.join("gap_test.txt.2"), b"x").unwrap();

        // First free slot is 1 (lexicographic probe, not a sorted scan —
        // the implementation probes 0, 1, 2 in order; 0 is taken, 1 is free).
        let got = trash_path_in(&root, fake_original, false).unwrap();
        assert!(
            got.to_string_lossy().ends_with(".1"),
            "expected counter 1 (first gap), got {got:?}"
        );
    }

    #[test]
    fn move_to_trash_moves_a_file_into_its_reserved_slot() {
        let (_td, root) = isolated_trash_root();
        let work = tempfile::tempdir().unwrap();
        let victim = work.path().join("notes.txt");
        std::fs::write(&victim, b"contents").unwrap();

        let dest = trash_path_in(&root, &victim, false).unwrap();
        move_to_trash(&victim, &dest).unwrap();

        assert!(!victim.exists(), "original must be gone");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"contents",
            "the reserved 0-byte placeholder must have been replaced by the file"
        );
    }

    #[test]
    fn move_to_trash_moves_a_directory_tree_into_its_reserved_slot() {
        let (_td, root) = isolated_trash_root();
        let work = tempfile::tempdir().unwrap();
        let victim = work.path().join("project");
        std::fs::create_dir_all(victim.join("sub")).unwrap();
        std::fs::write(victim.join("sub/file.txt"), b"deep").unwrap();

        let dest = trash_path_in(&root, &victim, true).unwrap();
        move_to_trash(&victim, &dest).unwrap();

        assert!(!victim.exists(), "original tree must be gone");
        assert_eq!(std::fs::read(dest.join("sub/file.txt")).unwrap(), b"deep");
    }

    /// A failed move must not consume the reservation *or* the original — the
    /// caller reports the error and the user still has the file.
    #[test]
    fn move_to_trash_failure_keeps_the_original() {
        let (_td, root) = isolated_trash_root();
        let work = tempfile::tempdir().unwrap();
        let missing = work.path().join("never-existed");
        let dest = trash_path_in(&root, &missing, false).unwrap();

        assert!(move_to_trash(&missing, &dest).is_err());
        assert!(dest.is_file(), "the reserved slot must survive");
    }

    #[test]
    fn trash_dir_is_idempotent() {
        let (_td, root) = isolated_trash_root();
        // Calling twice must not error even though the dir already exists.
        let a = root.dir().unwrap();
        let b = root.dir().unwrap();
        assert_eq!(a, b);
    }

    /// Two roots that name different directories cannot collide, even for the
    /// same original file name — the property the env override could never
    /// give, since there is only one `XDG_CACHE_HOME` per process.
    #[test]
    fn distinct_roots_do_not_share_slots() {
        let (_a_td, a) = isolated_trash_root();
        let (_b_td, b) = isolated_trash_root();
        let original = Path::new("/some/project/same-name.rs");

        let first = trash_path_in(&a, original, false).unwrap();
        let second = trash_path_in(&b, original, false).unwrap();

        assert!(
            first.to_string_lossy().ends_with(".0") && second.to_string_lossy().ends_with(".0"),
            "each root reserves its own counter 0: {first:?} / {second:?}"
        );
        assert_ne!(first, second, "distinct roots must yield distinct paths");
    }
}
