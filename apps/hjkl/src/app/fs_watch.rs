//! Event-driven autoreload (#242) — wires `hjkl-fs-watch` into the buffer
//! reload path so external changes are picked up immediately, without waiting
//! for a `:checktime` / focus-regain poll.
//!
//! Rather than recursively watching the whole cwd subtree (which would register
//! tens of thousands of inotify watches in a big repo and stall startup), the
//! watcher watches the **parent directory of each open file, non-recursively** —
//! one cheap watch per distinct directory. A filter then forwards only events
//! whose path is an actual open buffer, so sibling-file churn never wakes the UI
//! loop. Each surviving event is reconciled through [`App::checktime_slot`] —
//! the exact reload logic the poll path uses (autoreload-gated, cursor-
//! preserving, dirty buffers warn instead of clobber). Self-writes (`:w`) are
//! absorbed by the mtime/len baseline `checktime_slot` updates on save, so they
//! reconcile to a no-op rather than looping.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use std::sync::{Arc, Mutex};

use hjkl_fs_watch::{FsEvent, Watcher, WatcherBuilder};

use super::{App, canon_for_match};

/// Event-driven file-watch state held by [`App`].
pub(crate) struct FsWatch {
    /// Background notify watcher + debounce worker. Drained each tick.
    watcher: Watcher,
    /// Canonicalized paths of open file buffers. Shared with the watcher's
    /// filter closure so non-open sibling churn is dropped before it reaches the
    /// consumer channel. Kept in lockstep with `dirs` by `fs_watch_sync`.
    watched: Arc<Mutex<HashSet<PathBuf>>>,
    /// Directories currently registered with notify (parents of open files).
    /// Diffed against the desired set each sync so we only add/remove the delta.
    dirs: HashSet<PathBuf>,
}

impl App {
    /// Start event-driven autoreload: build a rootless fs-watch and register a
    /// non-recursive watch on each open file's parent directory.
    ///
    /// Idempotent and best-effort — a watcher build error leaves `fs_watch`
    /// `None`, in which case the poll-based `:checktime` path (focus-regain)
    /// still provides autoreload. Call once after config is applied, before the
    /// event loop starts.
    pub fn enable_fs_watch(&mut self) {
        if self.fs_watch.is_some() {
            return;
        }
        let watched: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
        let filter_set = Arc::clone(&watched);
        let build = WatcherBuilder::new()
            .debounce(Duration::from_millis(100))
            .filter(move |p| {
                let c = canon_for_match(p);
                filter_set.lock().map(|s| s.contains(&c)).unwrap_or(false)
            })
            .build();
        match build {
            Ok(watcher) => {
                self.fs_watch = Some(FsWatch {
                    watcher,
                    watched,
                    dirs: HashSet::new(),
                });
                // Register watches for the files already open at startup.
                self.fs_watch_sync();
            }
            Err(e) => {
                self.bus
                    .warn(format!("fs-watch disabled (autoreload polls only): {e}"));
            }
        }
    }

    /// Reconcile the notify watches and the filter set with the currently-open
    /// file slots: watch any new parent directory, unwatch directories with no
    /// open file left, and refresh the open-file filter set. Cheap (a handful of
    /// `canonicalize` calls + a set diff) and idempotent. Call after any buffer
    /// open/close, and once per tick from the drain as a catch-all for the
    /// less-common open paths (`:e`, picker, explorer, splits). No-op when
    /// fs-watch isn't enabled.
    pub(crate) fn fs_watch_sync(&mut self) {
        if self.fs_watch.is_none() {
            return;
        }
        // Desired state derived from open file slots.
        let mut files: HashSet<PathBuf> = HashSet::new();
        let mut dirs: HashSet<PathBuf> = HashSet::new();
        for s in &self.slots {
            if let Some(p) = s.filename.as_deref() {
                let cf = canon_for_match(p);
                if let Some(parent) = cf.parent() {
                    dirs.insert(parent.to_path_buf());
                }
                files.insert(cf);
            }
        }
        let fw = self.fs_watch.as_mut().expect("checked is_some above");
        // Refresh the filter set (events for non-open files get dropped).
        if let Ok(mut set) = fw.watched.lock() {
            *set = files;
        }
        // Watch newly-needed directories; unwatch ones no file references now.
        // Both are best-effort: a transient watch/unwatch error degrades to the
        // poll path for that file, never a crash.
        for d in dirs.difference(&fw.dirs) {
            let _ = fw.watcher.watch_path(d, false);
        }
        for d in fw.dirs.difference(&dirs) {
            let _ = fw.watcher.unwatch_path(d);
        }
        fw.dirs = dirs;
    }

    /// Drain queued fs-watch events and reconcile each against open slots.
    /// Returns `true` when any buffer was reloaded (so the caller can request a
    /// repaint). Called once per event-loop tick from `drain_async_polls`.
    pub(crate) fn drain_fs_watch_events(&mut self) -> bool {
        if self.fs_watch.is_none() {
            return false;
        }
        // Catch-all: keep the watches + filter set current with the open slots.
        // The hot open/close paths sync eagerly, but less-common opens (`:e`,
        // picker, explorer, splits) don't — a cheap per-tick resync covers them.
        self.fs_watch_sync();
        // Collect first so the &mut borrow on the watcher ends before we touch
        // slots in `checktime_slot`.
        let events: Vec<FsEvent> = match &mut self.fs_watch {
            Some(fw) => fw.watcher.events().collect(),
            None => return false,
        };
        self.apply_fs_events(events)
    }

    /// Reconcile a batch of [`FsEvent`]s against open slots. Split out from
    /// [`drain_fs_watch_events`] so tests can inject events without a live
    /// watcher. Returns `true` when any buffer was reloaded.
    pub(crate) fn apply_fs_events(&mut self, events: Vec<FsEvent>) -> bool {
        if events.is_empty() {
            return false;
        }
        // Map events → candidate paths (a rename touches both ends).
        let mut paths: Vec<PathBuf> = Vec::new();
        for ev in events {
            match ev {
                FsEvent::Created(p) | FsEvent::Modified(p) | FsEvent::Removed(p) => paths.push(p),
                FsEvent::Renamed { from, to } => {
                    paths.push(from);
                    paths.push(to);
                }
                _ => {}
            }
        }
        // Canonicalize + dedup so two events for one file trigger one reload.
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut messages: Vec<String> = Vec::new();
        let mut reloaded = false;
        for p in paths {
            let canon = canon_for_match(&p);
            if !seen.insert(canon.clone()) {
                continue;
            }
            // Reconcile every slot whose file canonicalizes to this path.
            for idx in 0..self.slots.len() {
                let matches =
                    self.slots[idx].filename.as_deref().map(canon_for_match) == Some(canon.clone());
                if matches {
                    reloaded |= self.checktime_slot(idx, &mut messages);
                }
            }
        }
        if !messages.is_empty() {
            self.bus.info(messages.join(" | "));
        }
        reloaded
    }
}
