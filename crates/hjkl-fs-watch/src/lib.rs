//! Debounced filesystem watcher for hjkl editors and pickers.
//!
//! # Why tokio-free?
//!
//! `notify`'s [`RecommendedWatcher`] already runs on a background OS thread
//! (inotify/kqueue/FSEvents). Adding a second async runtime purely for the
//! fan-out / debounce logic would be heavy ceremony with no throughput benefit
//! for the access patterns this crate targets (a handful of events per user
//! keystroke, not thousands per second). A single `std::thread` +
//! `crossbeam-channel` gives the same semantics with zero additional deps on
//! Tokio and composes cleanly with both TUI (main-thread event loop) and GUI
//! (floem reactive) consumers that manage their own runtimes.
//!
//! # Debounce window
//!
//! The debounce timer uses a **sliding window**: each new raw event for a
//! path resets that path's timer to `now + debounce_duration`.  A rapid
//! burst of writes to the same file does **not** emit intermediate events —
//! only one [`FsEvent`] is emitted after the last raw event in the burst,
//! once the sliding window has expired with no further activity on that path.
//! The flush ticker runs at half the debounce interval so events are
//! delivered promptly once the burst settles.
//!
//! # Overview
//!
//! ```no_run
//! use std::time::Duration;
//! use hjkl_fs_watch::{WatcherBuilder, FsEvent};
//!
//! # fn main() -> Result<(), hjkl_fs_watch::WatchError> {
//! let mut watcher = WatcherBuilder::new()
//!     .root("/tmp".into())
//!     .debounce(Duration::from_millis(50))
//!     .recursive(true)
//!     .build()?;
//!
//! for event in watcher.events() {
//!     match event {
//!         FsEvent::Created(p) => println!("created: {}", p.display()),
//!         FsEvent::Modified(p) => println!("modified: {}", p.display()),
//!         FsEvent::Removed(p) => println!("removed: {}", p.display()),
//!         FsEvent::Renamed { from, to } => {
//!             println!("renamed: {} -> {}", from.display(), to.display())
//!         }
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use std::{
    collections::HashMap,
    fmt, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, Sender, bounded, select, tick};
use notify::{
    Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher,
    event::{ModifyKind, RenameMode},
};

/// Type alias for the path filter closure to keep signatures readable.
type FilterFn = Box<dyn Fn(&Path) -> bool + Send + 'static>;

// ──────────────────────────────────────────────────────────────────────────────
// Public error type
// ──────────────────────────────────────────────────────────────────────────────

/// Errors produced by [`WatcherBuilder::build`] and internal worker threads.
#[non_exhaustive]
#[derive(Debug)]
pub enum WatchError {
    /// An error from the underlying `notify` backend.
    Notify(notify::Error),
    /// An I/O error (e.g. the root directory does not exist).
    Io(io::Error),
    /// The root directory was not configured before calling
    /// [`WatcherBuilder::build`].
    MissingRoot,
}

impl fmt::Display for WatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WatchError::Notify(e) => write!(f, "notify error: {e}"),
            WatchError::Io(e) => write!(f, "io error: {e}"),
            WatchError::MissingRoot => write!(f, "no root directory configured"),
        }
    }
}

impl std::error::Error for WatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WatchError::Notify(e) => Some(e),
            WatchError::Io(e) => Some(e),
            WatchError::MissingRoot => None,
        }
    }
}

impl From<notify::Error> for WatchError {
    fn from(e: notify::Error) -> Self {
        WatchError::Notify(e)
    }
}

impl From<io::Error> for WatchError {
    fn from(e: io::Error) -> Self {
        WatchError::Io(e)
    }
}

/// Convenience alias.
pub type Result<T, E = WatchError> = std::result::Result<T, E>;

// ──────────────────────────────────────────────────────────────────────────────
// FsEvent
// ──────────────────────────────────────────────────────────────────────────────

/// A filesystem event delivered by [`Watcher`].
///
/// The enum is `#[non_exhaustive]`; match arms should include a wildcard
/// catch-all.
///
/// # Rename semantics
///
/// `notify` delivers renames differently across platforms:
///
/// - **Linux/inotify** emits paired `MOVED_FROM` + `MOVED_TO` events with a
///   kernel cookie when the rename is within the watched tree, or a single
///   `Modify(Name(Both))` event when both paths are provided in one batch.
/// - **macOS/FSEvents** and **Windows/ReadDirectoryChanges** sometimes split
///   the pair across separate batches.
///
/// The debounce worker merges rename events within a debounce window using
/// `notify`'s `RenameMode` hints:
///
/// - `RenameMode::Both` — paths\[0\] = from, paths\[1\] = to → `Renamed`
///   directly.
/// - `RenameMode::From` — stored as a pending "from" entry.
/// - `RenameMode::To` — merged with any pending "from" entry, or emitted as
///   `Created` if no matching "from" is found.
///
/// When the platform does not provide enough information to correlate the pair,
/// two separate events (`Removed` + `Created`) are emitted. Do not rely on
/// `Renamed` being delivered on all platforms.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsEvent {
    /// A new file or directory was created at the given path.
    Created(PathBuf),
    /// An existing file or directory was modified.
    Modified(PathBuf),
    /// A file or directory was deleted.
    Removed(PathBuf),
    /// A file or directory was renamed / moved.
    Renamed {
        /// The original path.
        from: PathBuf,
        /// The new path.
        to: PathBuf,
    },
}

// ──────────────────────────────────────────────────────────────────────────────
// Internal debounce state
// ──────────────────────────────────────────────────────────────────────────────

/// The "kind" we track per-path during debouncing.
#[derive(Debug, Clone)]
enum PendingKind {
    Created,
    Modified,
    Removed,
    /// Pending rename-from: we saw `RenameMode::From`; a matching `To` may
    /// arrive within the debounce window and be merged into `Renamed`.
    RenameFrom,
    /// Merged rename: `to` is the destination path.
    Renamed {
        to: PathBuf,
    },
}

#[derive(Debug, Clone)]
struct Pending {
    kind: PendingKind,
    at: Instant,
}

// ──────────────────────────────────────────────────────────────────────────────
// WatcherBuilder
// ──────────────────────────────────────────────────────────────────────────────

/// Builder for [`Watcher`].
///
/// The only required field is [`root`](WatcherBuilder::root).
///
/// ```no_run
/// use std::time::Duration;
/// use hjkl_fs_watch::WatcherBuilder;
///
/// # fn main() -> Result<(), hjkl_fs_watch::WatchError> {
/// let watcher = WatcherBuilder::new()
///     .root("/tmp".into())
///     .debounce(Duration::from_millis(50))
///     .filter(|p| p.extension().map(|e| e == "sql").unwrap_or(false))
///     .recursive(true)
///     .build()?;
/// # Ok(())
/// # }
/// ```
#[non_exhaustive]
pub struct WatcherBuilder {
    root: Option<PathBuf>,
    filter: Option<FilterFn>,
    debounce: Duration,
    recursive: bool,
}

impl Default for WatcherBuilder {
    /// Creates a builder with defaults: no filter, 100 ms debounce, recursive.
    /// Note: [`root`](WatcherBuilder::root) must be set before calling
    /// [`build`](WatcherBuilder::build).
    fn default() -> Self {
        Self::new()
    }
}

impl WatcherBuilder {
    /// Create a new builder with defaults: no filter, 100 ms debounce,
    /// recursive.
    pub fn new() -> Self {
        Self {
            root: None,
            filter: None,
            debounce: Duration::from_millis(100),
            recursive: true,
        }
    }

    /// Set the directory to watch. **Required.**
    pub fn root(mut self, p: PathBuf) -> Self {
        self.root = Some(p);
        self
    }

    /// Set a path filter. Only events whose path satisfies `f` are forwarded.
    /// The filter runs *before* the debounce window.
    pub fn filter(mut self, f: impl Fn(&Path) -> bool + Send + 'static) -> Self {
        self.filter = Some(Box::new(f));
        self
    }

    /// Set the debounce window. Rapid events for the same path are coalesced;
    /// only the last kind within the window is emitted. Default: 100 ms.
    pub fn debounce(mut self, d: Duration) -> Self {
        self.debounce = d;
        self
    }

    /// Enable or disable recursive watching. Default: `true`.
    pub fn recursive(mut self, r: bool) -> Self {
        self.recursive = r;
        self
    }

    /// Build the watcher and start the background worker thread.
    ///
    /// Returns [`WatchError::MissingRoot`] if [`root`](Self::root) was not set.
    ///
    /// ```no_run
    /// use hjkl_fs_watch::WatcherBuilder;
    /// # fn main() -> Result<(), hjkl_fs_watch::WatchError> {
    /// let w = WatcherBuilder::new().root("/tmp".into()).build()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn build(self) -> Result<Watcher> {
        // `root` is optional: when set, it's watched immediately (the
        // directory-watching use case — explorer, oil). When absent, the
        // watcher starts empty and the caller adds individual paths via
        // [`Watcher::watch_path`] (the per-file autoreload use case, where
        // recursively watching a whole tree would be too expensive).
        let root = self.root;
        let debounce = self.debounce;
        let recursive = self.recursive;
        let filter = self.filter;

        // Channel from notify → worker thread.
        let (raw_tx, raw_rx) = bounded::<notify::Result<Event>>(512);

        // Channel from worker thread → consumer (Watcher::try_recv etc.).
        let (ev_tx, ev_rx) = bounded::<FsEvent>(512);

        // Pause flag shared between Watcher and the worker.
        let paused = Arc::new(AtomicBool::new(false));
        let paused_worker = Arc::clone(&paused);

        // Build the notify watcher.
        let mut notify_watcher = RecommendedWatcher::new(
            move |res| {
                let _ = raw_tx.try_send(res);
            },
            notify::Config::default(),
        )?;

        let mode = if recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        if let Some(root) = &root {
            notify_watcher.watch(root, mode)?;
        }

        // Spawn the debounce worker.
        std::thread::Builder::new()
            .name("hjkl-fs-watch-worker".into())
            .spawn(move || {
                worker(raw_rx, ev_tx, filter, debounce, paused_worker);
            })
            .map_err(WatchError::Io)?;

        Ok(Watcher {
            rx: ev_rx,
            paused,
            notify: notify_watcher,
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Background worker
// ──────────────────────────────────────────────────────────────────────────────

/// Background thread: receives raw notify events, applies filter, debounces,
/// and forwards coalesced [`FsEvent`]s to the consumer channel.
fn worker(
    raw_rx: Receiver<notify::Result<Event>>,
    ev_tx: Sender<FsEvent>,
    filter: Option<FilterFn>,
    debounce: Duration,
    paused: Arc<AtomicBool>,
) {
    // Tick at half the debounce window to flush pending events promptly.
    let flush_interval = if debounce.is_zero() {
        Duration::from_millis(1)
    } else {
        debounce / 2
    };
    let ticker = tick(flush_interval);

    // path → latest pending kind + arrival time.
    let mut pending: HashMap<PathBuf, Pending> = HashMap::new();

    loop {
        select! {
            recv(raw_rx) -> msg => {
                match msg {
                    Err(_) => break, // Watcher dropped, channel closed.
                    Ok(Err(_)) => continue, // notify error, ignore.
                    Ok(Ok(event)) => {
                        if paused.load(Ordering::SeqCst) {
                            continue;
                        }
                        handle_event(event, &mut pending, filter.as_ref());
                    }
                }
            }
            recv(ticker) -> _ => {
                flush_pending(&mut pending, &ev_tx, debounce);
            }
        }
    }

    // Flush remaining events before exit.
    for (path, p) in pending {
        let _ = ev_tx.try_send(pending_to_event(path, p));
    }
}

/// Incorporate one raw notify [`Event`] into the pending map.
fn handle_event(event: Event, pending: &mut HashMap<PathBuf, Pending>, filter: Option<&FilterFn>) {
    match event.kind {
        // ── Rename: both paths in one event (most reliable) ──────────────────
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
            // paths = [from, to]
            let mut iter = event.paths.into_iter();
            let (Some(from), Some(to)) = (iter.next(), iter.next()) else {
                return;
            };
            if !passes_filter(filter, &from) && !passes_filter(filter, &to) {
                return;
            }
            // Remove any stale pending entries for both paths.
            pending.remove(&from);
            pending.remove(&to);
            pending.insert(
                from.clone(),
                Pending {
                    kind: PendingKind::Renamed { to },
                    at: Instant::now(),
                },
            );
        }

        // ── Rename: "from" side ───────────────────────────────────────────────
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
            for path in event.paths {
                if !passes_filter(filter, &path) {
                    continue;
                }
                upsert(pending, path, PendingKind::RenameFrom);
            }
        }

        // ── Rename: "to" side — merge with any pending RenameFrom ─────────────
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
            for to_path in event.paths {
                // Look for the most recent RenameFrom.
                let from = pending
                    .iter()
                    .filter(|(_, v)| matches!(v.kind, PendingKind::RenameFrom))
                    .max_by_key(|(_, v)| v.at)
                    .map(|(k, _)| k.clone());

                if let Some(from_path) = from {
                    pending.remove(&from_path);
                    pending.insert(
                        from_path.clone(),
                        Pending {
                            kind: PendingKind::Renamed { to: to_path },
                            at: Instant::now(),
                        },
                    );
                } else {
                    // No matching From — treat as Create.
                    if passes_filter(filter, &to_path) {
                        upsert(pending, to_path, PendingKind::Created);
                    }
                }
            }
        }

        // ── Rename: Any / Other ───────────────────────────────────────────────
        EventKind::Modify(ModifyKind::Name(_)) => {
            // Fallback: treat as a modify for each path.
            for path in event.paths {
                if !passes_filter(filter, &path) {
                    continue;
                }
                upsert(pending, path, PendingKind::Modified);
            }
        }

        // ── Create ───────────────────────────────────────────────────────────
        EventKind::Create(_) => {
            for path in event.paths {
                if !passes_filter(filter, &path) {
                    continue;
                }
                // If there's a pending RenameFrom, merge to Renamed.
                let from = pending
                    .iter()
                    .filter(|(_, v)| matches!(v.kind, PendingKind::RenameFrom))
                    .max_by_key(|(_, v)| v.at)
                    .map(|(k, _)| k.clone());
                if let Some(from_path) = from {
                    pending.remove(&from_path);
                    pending.insert(
                        from_path,
                        Pending {
                            kind: PendingKind::Renamed { to: path },
                            at: Instant::now(),
                        },
                    );
                } else {
                    upsert(pending, path, PendingKind::Created);
                }
            }
        }

        // ── Modify ───────────────────────────────────────────────────────────
        EventKind::Modify(_) => {
            for path in event.paths {
                if !passes_filter(filter, &path) {
                    continue;
                }
                upsert(pending, path, PendingKind::Modified);
            }
        }

        // ── Remove ───────────────────────────────────────────────────────────
        EventKind::Remove(_) => {
            for path in event.paths {
                if !passes_filter(filter, &path) {
                    continue;
                }
                upsert(pending, path, PendingKind::Removed);
            }
        }

        // ── Access / Other / Any ─────────────────────────────────────────────
        EventKind::Access(_) | EventKind::Other | EventKind::Any => {
            // Access events are ignored. Any/Other are no-ops.
        }
    }
}

/// Flush entries whose debounce window has elapsed.
fn flush_pending(
    pending: &mut HashMap<PathBuf, Pending>,
    ev_tx: &Sender<FsEvent>,
    debounce: Duration,
) {
    let now = Instant::now();
    let done: Vec<PathBuf> = pending
        .iter()
        .filter(|(_, p)| now.duration_since(p.at) >= debounce)
        .map(|(path, _)| path.clone())
        .collect();
    for path in done {
        if let Some(p) = pending.remove(&path) {
            let _ = ev_tx.try_send(pending_to_event(path, p));
        }
    }
}

fn pending_to_event(path: PathBuf, p: Pending) -> FsEvent {
    match p.kind {
        PendingKind::Created => FsEvent::Created(path),
        PendingKind::Modified => FsEvent::Modified(path),
        PendingKind::Removed => FsEvent::Removed(path),
        PendingKind::RenameFrom => FsEvent::Removed(path),
        PendingKind::Renamed { to } => FsEvent::Renamed { from: path, to },
    }
}

#[inline]
fn passes_filter(filter: Option<&FilterFn>, path: &Path) -> bool {
    filter.map(|f| f(path)).unwrap_or(true)
}

#[inline]
fn upsert(pending: &mut HashMap<PathBuf, Pending>, path: PathBuf, kind: PendingKind) {
    let entry = pending.entry(path).or_insert_with(|| Pending {
        kind: kind.clone(),
        at: Instant::now(),
    });
    entry.kind = kind;
    entry.at = Instant::now();
}

// ──────────────────────────────────────────────────────────────────────────────
// Watcher (public)
// ──────────────────────────────────────────────────────────────────────────────

/// Owns a background filesystem watcher and delivers debounced [`FsEvent`]s.
///
/// Create via [`WatcherBuilder`].
///
/// ```no_run
/// use std::time::Duration;
/// use hjkl_fs_watch::{WatcherBuilder, FsEvent};
///
/// # fn main() -> Result<(), hjkl_fs_watch::WatchError> {
/// let mut watcher = WatcherBuilder::new()
///     .root("/tmp".into())
///     .debounce(Duration::from_millis(50))
///     .build()?;
///
/// // Non-blocking drain.
/// for event in watcher.events() {
///     println!("{event:?}");
/// }
/// # Ok(())
/// # }
/// ```
#[non_exhaustive]
pub struct Watcher {
    rx: Receiver<FsEvent>,
    paused: Arc<AtomicBool>,
    /// The live notify watcher. Kept alive for the watcher's lifetime and used
    /// to add / remove individual watched paths after construction.
    notify: RecommendedWatcher,
}

impl fmt::Debug for Watcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watcher")
            .field("paused", &self.paused.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Watcher {
    /// Pause event delivery. Events that arrive while paused are silently
    /// dropped by the worker thread.
    ///
    /// Useful to suppress self-triggered events when the editor writes the
    /// file it is watching.
    ///
    /// ```no_run
    /// use hjkl_fs_watch::WatcherBuilder;
    /// # fn main() -> Result<(), hjkl_fs_watch::WatchError> {
    /// let mut watcher = WatcherBuilder::new().root("/tmp".into()).build()?;
    /// watcher.pause();
    /// // ... write files ...
    /// watcher.resume();
    /// # Ok(())
    /// # }
    /// ```
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    /// Resume event delivery after a [`pause`](Self::pause).
    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    /// Return `true` if the watcher is currently paused.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Try to receive a single event without blocking.
    ///
    /// Returns `None` if the queue is empty.
    pub fn try_recv(&mut self) -> Option<FsEvent> {
        self.rx.try_recv().ok()
    }

    /// Block until an event arrives or `timeout` elapses.
    ///
    /// Returns `None` on timeout.
    pub fn recv_timeout(&mut self, timeout: Duration) -> Option<FsEvent> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Drain all currently queued events.
    ///
    /// This is a non-blocking iterator that stops as soon as the queue is
    /// empty.
    ///
    /// ```no_run
    /// use hjkl_fs_watch::WatcherBuilder;
    /// # fn main() -> Result<(), hjkl_fs_watch::WatchError> {
    /// let mut watcher = WatcherBuilder::new().root("/tmp".into()).build()?;
    /// for event in watcher.events() {
    ///     println!("{event:?}");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn events(&mut self) -> impl Iterator<Item = FsEvent> + '_ {
        std::iter::from_fn(move || self.rx.try_recv().ok())
    }

    /// Start watching `path` after construction. Use `recursive = false` to
    /// watch a single file or one directory level (the cheap per-file
    /// autoreload case); `recursive = true` to watch a whole subtree.
    ///
    /// Idempotent at the notify layer — re-watching an already-watched path is
    /// harmless. The builder's `filter` still applies to the resulting events.
    pub fn watch_path(&mut self, path: &Path, recursive: bool) -> Result<()> {
        let mode = if recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        self.notify.watch(path, mode)?;
        Ok(())
    }

    /// Stop watching `path` (added earlier via [`watch_path`](Self::watch_path)
    /// or the builder root). Returns the underlying error if `path` was not
    /// watched.
    pub fn unwatch_path(&mut self, path: &Path) -> Result<()> {
        self.notify.unwatch(path)?;
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    /// Wait up to `budget` polling every `interval` until `predicate` is true.
    fn wait_for<F: FnMut() -> bool>(
        mut predicate: F,
        budget: Duration,
        interval: Duration,
    ) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            if predicate() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(interval);
        }
    }

    fn poll_event(watcher: &mut Watcher, budget: Duration) -> Option<FsEvent> {
        let deadline = Instant::now() + budget;
        loop {
            if let Some(ev) = watcher.try_recv() {
                return Some(ev);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn collect_events(watcher: &mut Watcher, budget: Duration) -> Vec<FsEvent> {
        let mut events = Vec::new();
        let deadline = Instant::now() + budget;
        loop {
            while let Some(ev) = watcher.try_recv() {
                events.push(ev);
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        events
    }

    fn default_watcher(dir: &Path) -> Watcher {
        WatcherBuilder::new()
            .root(dir.to_path_buf())
            .debounce(Duration::from_millis(50))
            .build()
            .expect("build watcher")
    }

    /// Let FSEvents / inotify settle after watcher creation, then drain any
    /// init-time events (e.g. macOS FSEvents fires a Created for the watched
    /// root directory itself).
    fn settle_watcher(watcher: &mut Watcher) {
        std::thread::sleep(Duration::from_millis(300));
        while watcher.try_recv().is_some() {}
    }

    // ── builder / error tests ────────────────────────────────────────────────

    #[test]
    fn build_without_root_succeeds_empty() {
        // A rootless watcher is valid: it starts empty and watches nothing
        // until the caller adds paths via `watch_path` (the per-file autoreload
        // use case, where recursively watching a tree would be too expensive).
        let mut watcher = WatcherBuilder::new()
            .debounce(Duration::from_millis(50))
            .build()
            .expect("rootless build must succeed");
        settle_watcher(&mut watcher);
        // No watches → no events even when an unrelated file changes.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("untracked.txt"), b"x").unwrap();
        assert!(
            poll_event(&mut watcher, Duration::from_millis(400)).is_none(),
            "rootless watcher must not emit events"
        );
    }

    #[test]
    fn watch_path_after_build_emits_events() {
        // Build empty, then add a directory watch — events for files in it flow.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut watcher = WatcherBuilder::new()
            .debounce(Duration::from_millis(50))
            .build()
            .expect("build");
        watcher.watch_path(&root, false).expect("watch_path");
        settle_watcher(&mut watcher);

        let file = root.join("added.txt");
        fs::write(&file, b"hi").unwrap();
        let ev = poll_event(&mut watcher, Duration::from_secs(3))
            .expect("expected event for a path added via watch_path");
        assert!(
            matches!(&ev, FsEvent::Created(p) | FsEvent::Modified(p) if p == &file),
            "unexpected event: {ev:?}"
        );
    }

    #[test]
    fn unwatch_path_stops_events() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut watcher = WatcherBuilder::new()
            .debounce(Duration::from_millis(50))
            .build()
            .expect("build");
        watcher.watch_path(&root, false).expect("watch_path");
        settle_watcher(&mut watcher);

        watcher.unwatch_path(&root).expect("unwatch_path");
        settle_watcher(&mut watcher);

        fs::write(root.join("after_unwatch.txt"), b"x").unwrap();
        assert!(
            poll_event(&mut watcher, Duration::from_millis(500)).is_none(),
            "no events should arrive after unwatch_path"
        );
    }

    #[test]
    fn watch_error_display_missing_root() {
        let e = WatchError::MissingRoot;
        assert!(e.to_string().contains("root"));
    }

    #[test]
    fn watch_error_display_io() {
        let e = WatchError::Io(io::Error::other("bang"));
        assert!(e.to_string().contains("io error"));
    }

    #[test]
    fn watch_error_display_notify() {
        let e = WatchError::Notify(notify::Error::generic("x"));
        assert!(e.to_string().contains("notify error"));
    }

    #[test]
    fn builder_defaults() {
        let b = WatcherBuilder::new();
        assert!(b.root.is_none());
        assert!(b.filter.is_none());
        assert_eq!(b.debounce, Duration::from_millis(100));
        assert!(b.recursive);
    }

    // ── Created event ────────────────────────────────────────────────────────

    #[test]
    fn create_file_emits_created_event() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut watcher = default_watcher(&root);
        settle_watcher(&mut watcher);

        let file = root.join("hello.txt");
        fs::write(&file, b"hi").unwrap();

        let ev = poll_event(&mut watcher, Duration::from_secs(3)).expect("expected Created event");
        // Platforms may emit Created or Modified on a new file.
        assert!(
            matches!(&ev, FsEvent::Created(p) | FsEvent::Modified(p) if p == &file),
            "unexpected event: {ev:?}"
        );
    }

    // ── Modified event ───────────────────────────────────────────────────────

    #[test]
    fn modify_file_emits_modified_event() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("data.txt");
        fs::write(&file, b"initial").unwrap();

        let mut watcher = default_watcher(&root);
        settle_watcher(&mut watcher);

        fs::write(&file, b"updated").unwrap();

        let ev = poll_event(&mut watcher, Duration::from_secs(3)).expect("expected Modified event");
        assert!(
            matches!(&ev, FsEvent::Modified(p) | FsEvent::Created(p) if p == &file),
            "unexpected event: {ev:?}"
        );
    }

    // ── Removed event ────────────────────────────────────────────────────────

    #[test]
    fn delete_file_emits_removed_event() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("gone.txt");
        fs::write(&file, b"bye").unwrap();

        let mut watcher = default_watcher(&root);
        settle_watcher(&mut watcher);

        fs::remove_file(&file).unwrap();

        let ev = poll_event(&mut watcher, Duration::from_secs(3)).expect("expected Removed event");
        // macOS FSEvents may report unlink as Modified rather than Removed.
        assert!(
            matches!(&ev, FsEvent::Removed(p) | FsEvent::Modified(p) if p == &file),
            "unexpected event: {ev:?}"
        );
    }

    // ── Renamed event (best-effort) ──────────────────────────────────────────

    #[test]
    fn rename_file_emits_renamed_or_remove_create() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let src = root.join("old.txt");
        let dst = root.join("new.txt");
        fs::write(&src, b"data").unwrap();

        let mut watcher = default_watcher(&root);
        settle_watcher(&mut watcher);

        fs::rename(&src, &dst).unwrap();

        let events = collect_events(&mut watcher, Duration::from_secs(10));
        assert!(
            !events.is_empty(),
            "expected at least one event after rename"
        );

        // Accept any of:
        //   (a) a single Renamed event with the correct from/to pair, or
        //   (b) a Removed(src) AND a Created/Modified(dst) pair, or
        //   (c) a Modified(src) AND a Modified(dst) pair
        //       (macOS FSEvents conflates rename as two modify events).
        //
        // Requiring both sides of the pair prevents a lone spurious Create or
        // Remove from making the test pass vacuously.
        let has_rename = events
            .iter()
            .any(|e| matches!(e, FsEvent::Renamed { from, to } if from == &src && to == &dst));
        let has_remove = events
            .iter()
            .any(|e| matches!(e, FsEvent::Removed(p) if p == &src));
        let has_create = events
            .iter()
            .any(|e| matches!(e, FsEvent::Created(p) | FsEvent::Modified(p) if p == &dst));
        let has_modify_src = events
            .iter()
            .any(|e| matches!(e, FsEvent::Modified(p) if p == &src));
        let has_modify_dst = events
            .iter()
            .any(|e| matches!(e, FsEvent::Modified(p) if p == &dst));
        assert!(
            has_rename || (has_remove && has_create) || (has_modify_src && has_modify_dst),
            "expected Renamed(src→dst), Removed(src)+Created/Modified(dst), or Modified(src)+Modified(dst); got {events:?}"
        );
    }

    // ── Debounce ─────────────────────────────────────────────────────────────

    #[test]
    fn rapid_modifies_coalesced() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("busy.txt");
        fs::write(&file, b"0").unwrap();

        let mut watcher = WatcherBuilder::new()
            .root(root.clone())
            .debounce(Duration::from_millis(200))
            .build()
            .unwrap();
        settle_watcher(&mut watcher);

        // Write 5 times rapidly.
        for i in 1u8..=5 {
            fs::write(&file, [i]).unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }

        // Wait for debounce window to clear (200 ms + generous headroom).
        let events = collect_events(&mut watcher, Duration::from_millis(800));

        // Filter to events for our specific file.
        let file_events: Vec<_> = events
            .iter()
            .filter(|e| match e {
                FsEvent::Created(p) | FsEvent::Modified(p) | FsEvent::Removed(p) => p == &file,
                FsEvent::Renamed { from, to } => from == &file || to == &file,
            })
            .collect();

        // Should be coalesced: 1 or at most 2 events (timing edge).
        assert!(
            file_events.len() <= 2,
            "expected debounce to coalesce; got {file_events:?}"
        );
    }

    // ── Filter ───────────────────────────────────────────────────────────────

    #[test]
    fn filter_blocks_non_matching_files() {
        let dir = tempfile::tempdir().unwrap();

        let mut watcher = WatcherBuilder::new()
            .root(dir.path().to_path_buf())
            .debounce(Duration::from_millis(50))
            .filter(|p| p.extension().map(|e| e == "rs").unwrap_or(false))
            .build()
            .unwrap();
        settle_watcher(&mut watcher);

        // Write a .txt file — should be filtered out.
        fs::write(dir.path().join("ignored.txt"), b"x").unwrap();
        std::thread::sleep(Duration::from_millis(200));

        let events = collect_events(&mut watcher, Duration::from_millis(100));
        assert!(
            events.is_empty(),
            "expected no events for filtered file, got {events:?}"
        );
    }

    #[test]
    fn filter_allows_matching_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();

        let mut watcher = WatcherBuilder::new()
            .root(root.clone())
            .debounce(Duration::from_millis(50))
            .filter(|p| p.extension().map(|e| e == "rs").unwrap_or(false))
            .build()
            .unwrap();
        settle_watcher(&mut watcher);

        let rs_file = root.join("main.rs");
        fs::write(&rs_file, b"fn main() {}").unwrap();

        let ev =
            poll_event(&mut watcher, Duration::from_secs(3)).expect("expected event for .rs file");
        assert!(
            matches!(&ev, FsEvent::Created(p) | FsEvent::Modified(p) if p == &rs_file),
            "unexpected event: {ev:?}"
        );
    }

    // ── Pause / resume ───────────────────────────────────────────────────────

    #[test]
    fn pause_suppresses_events() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("paused.txt");

        let mut watcher = default_watcher(dir.path());
        settle_watcher(&mut watcher);

        watcher.pause();
        assert!(watcher.is_paused());
        // Settle so the worker sees the SeqCst store, then drain stale events.
        std::thread::sleep(Duration::from_millis(300));
        while watcher.try_recv().is_some() {}

        fs::write(&file, b"silent").unwrap();
        std::thread::sleep(Duration::from_millis(200));

        let events = collect_events(&mut watcher, Duration::from_millis(100));
        assert!(
            events.is_empty(),
            "expected no events while paused, got {events:?}"
        );
    }

    #[test]
    fn resume_restores_event_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file_before = root.join("before.txt");
        let file_after = root.join("after.txt");

        let mut watcher = default_watcher(&root);
        settle_watcher(&mut watcher);

        watcher.pause();
        fs::write(&file_before, b"silent").unwrap();
        std::thread::sleep(Duration::from_millis(150));

        watcher.resume();
        assert!(!watcher.is_paused());

        fs::write(&file_after, b"audible").unwrap();

        let events = collect_events(&mut watcher, Duration::from_secs(3));
        let has_after = events
            .iter()
            .any(|e| matches!(e, FsEvent::Created(p) | FsEvent::Modified(p) if p == &file_after));
        let has_before = events
            .iter()
            .any(|e| matches!(e, FsEvent::Created(p) | FsEvent::Modified(p) if p == &file_before));
        assert!(
            has_after,
            "expected event for file written after resume; got {events:?}"
        );
        assert!(
            !has_before,
            "expected no event for file written while paused; got {events:?}"
        );
    }

    // ── try_recv / recv_timeout ──────────────────────────────────────────────

    #[test]
    fn try_recv_returns_none_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut watcher = default_watcher(dir.path());
        assert!(watcher.try_recv().is_none());
    }

    #[test]
    fn recv_timeout_returns_none_on_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let mut watcher = default_watcher(dir.path());
        // Let any init-time events settle before asserting silence.
        std::thread::sleep(std::time::Duration::from_millis(300));
        while watcher.try_recv().is_some() {}
        let result = watcher.recv_timeout(Duration::from_millis(50));
        assert!(result.is_none());
    }

    // ── non-recursive ────────────────────────────────────────────────────────

    #[test]
    fn non_recursive_ignores_subdirectory_files() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();

        let mut watcher = WatcherBuilder::new()
            .root(dir.path().to_path_buf())
            .debounce(Duration::from_millis(50))
            .recursive(false)
            .build()
            .unwrap();
        settle_watcher(&mut watcher);

        let deep = sub.join("deep.txt");
        fs::write(&deep, b"deep").unwrap();
        std::thread::sleep(Duration::from_millis(200));

        let events = collect_events(&mut watcher, Duration::from_millis(100));
        let deep_events: Vec<_> = events
            .iter()
            .filter(|e| match e {
                FsEvent::Created(p) | FsEvent::Modified(p) | FsEvent::Removed(p) => p == &deep,
                FsEvent::Renamed { from, to } => from == &deep || to == &deep,
            })
            .collect();
        assert!(
            deep_events.is_empty(),
            "non-recursive should not see deep.txt; got {events:?}"
        );
    }

    // ── events() iterator ────────────────────────────────────────────────────

    #[test]
    fn events_drains_queue() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("drain.txt");
        fs::write(&file, b"a").unwrap();

        let mut watcher = default_watcher(&root);
        settle_watcher(&mut watcher);

        fs::write(&file, b"b").unwrap();

        let found = wait_for(
            || watcher.try_recv().is_some(),
            Duration::from_secs(3),
            Duration::from_millis(20),
        );
        assert!(found, "expected at least one event");
        // Drain the rest — should not panic.
        let _rest: Vec<_> = watcher.events().collect();
    }

    // ── FsEvent equality ─────────────────────────────────────────────────────

    #[test]
    fn fs_event_equality() {
        let a = FsEvent::Created(PathBuf::from("/tmp/a"));
        let b = FsEvent::Created(PathBuf::from("/tmp/a"));
        let c = FsEvent::Modified(PathBuf::from("/tmp/a"));
        assert_eq!(a, b);
        assert_ne!(a, c);

        let r1 = FsEvent::Renamed {
            from: PathBuf::from("/tmp/old"),
            to: PathBuf::from("/tmp/new"),
        };
        let r2 = FsEvent::Renamed {
            from: PathBuf::from("/tmp/old"),
            to: PathBuf::from("/tmp/new"),
        };
        assert_eq!(r1, r2);
    }

    // ── pause/resume is_paused ───────────────────────────────────────────────

    #[test]
    fn is_paused_reflects_state() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = default_watcher(dir.path());
        assert!(!watcher.is_paused());
        watcher.pause();
        assert!(watcher.is_paused());
        watcher.resume();
        assert!(!watcher.is_paused());
    }
}
