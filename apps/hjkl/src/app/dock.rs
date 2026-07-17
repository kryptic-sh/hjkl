//! Global dock windows (window-management refactor Phase A, kryptic-sh/hjkl#63).
//!
//! A [`Dock`] owns a real [`WindowId`] + slot index — same as any window —
//! so all existing `window_editors` / focus / dispatch / render machinery
//! applies to it unmodified. What makes it a *dock* rather than an ordinary
//! window is simply that **no `LayoutTree` leaf ever references it**: it is
//! never woven into any tab's tree, so tree operations (`remove_leaf`,
//! `equalize_all`, `swap_with_sibling`, `:only`, `:split`, move-to-tab) can't
//! touch it by construction — there's no code path that could find it there.
//!
//! Docks are **global** (`App`-level, not per-tab): one `left_dock` /
//! `bottom_dock` pair shared across every tab, visible whichever tab is
//! active. Dock geometry comes from config (`App.config.explorer.width` /
//! `App.config.panel.height`), not a split ratio — see `render::frame`,
//! which carves the dock rect off the frame before handing the remainder to
//! the tree renderer.
//!
//! This module owns the dock lifecycle (install/teardown) and the
//! frame-level navigation/adjacency helpers that let `<C-w>` commands cross
//! between the tree and the dock. `hjkl-layout`'s tree API itself is
//! untouched — docks are invisible to it.

use super::window::{self, WindowId};

/// Which feature a dock hosts. Only [`DockKind::Explorer`] is wired up in
/// Phase A; the other two are reserved for the bottom dock landing in a
/// later phase (kryptic-sh/hjkl#63 Phase B) so the type is stable now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DockKind {
    /// Left dock: the file-explorer tree.
    Explorer,
    /// Bottom dock: `:copen` quickfix list (#63 Phase B).
    Quickfix,
    /// Bottom dock: `:lopen` location list (#63 Phase B).
    Loclist,
}

/// A window pinned outside the per-tab `LayoutTree`.
#[derive(Debug, Clone)]
pub(crate) struct Dock {
    /// The dock's real `WindowId` — has a normal `windows[..]` entry and a
    /// normal `window_editors` entry, exactly like a tree window.
    pub win_id: WindowId,
    /// Slot index at the time the dock was created. NOT authoritative after
    /// other slots are inserted/removed — always prefer
    /// `app.windows[dock.win_id].slot` for the current value; this is kept
    /// only as a creation-time record (matches the design doc's field
    /// shape) and as a fallback if the window entry is ever already gone.
    pub slot_idx: usize,
    pub kind: DockKind,
}

/// Minimum/maximum sane width for the left dock, independent of terminal
/// size — mirrors the static bounds in `hjkl_app::config::Config::validate`
/// (`explorer.width` ∈ 12..=400). The dynamic per-frame clamp additionally
/// caps at half the terminal width so the dock can never crowd out the main
/// area entirely.
pub(crate) const DOCK_MIN_WIDTH: u16 = 12;

/// Clamp a candidate left-dock width against both the static sane bounds and
/// the live terminal width, per the approved design ("12..=terminal_width/2").
pub(crate) fn clamp_dock_width(width: u16, terminal_width: u16) -> u16 {
    let max = (terminal_width / 2).max(DOCK_MIN_WIDTH);
    width.clamp(DOCK_MIN_WIDTH, max)
}

/// Minimum sane height for the bottom dock — mirrors `panel.height`'s static
/// validation bound (3..=200, see `hjkl_app::config::Config::validate`).
pub(crate) const DOCK_MIN_HEIGHT: u16 = 3;

/// Clamp a candidate bottom-dock height against both the static sane bounds
/// and the live terminal height, mirroring [`clamp_dock_width`]'s
/// "12..=terminal_width/2" shape ("3..=terminal_height/2" here).
pub(crate) fn clamp_dock_height(height: u16, terminal_height: u16) -> u16 {
    let max = (terminal_height / 2).max(DOCK_MIN_HEIGHT);
    height.clamp(DOCK_MIN_HEIGHT, max)
}

impl super::App {
    // ── Lifecycle ────────────────────────────────────────────────────────

    /// Allocate a fresh window over `slot_idx` and install it as the left
    /// dock. Does NOT touch focus or any `LayoutTree` — callers (currently
    /// only `explorer::open_explorer`) handle sequencing (window_editors
    /// reconcile, focus, initial cursor) themselves. Returns the new dock's
    /// `WindowId`.
    pub(crate) fn install_left_dock(&mut self, slot_idx: usize, kind: DockKind) -> WindowId {
        let win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(window::Window::new(slot_idx)));
        self.left_dock = Some(Dock {
            win_id,
            slot_idx,
            kind,
        });
        win_id
    }

    /// Tear down the left dock: drop its window entry + folds, remove its
    /// slot (reindexing every other window's slot reference), and fix up
    /// any tab whose remembered `focused_window` pointed at it. Docks are
    /// global, so a tab OTHER than the currently active one may also have
    /// been focused on this dock the last time it was active — every tab is
    /// swept, not just the active one. Returns the removed [`Dock`] (its
    /// `slot_idx` is the pre-removal value, informational only).
    ///
    /// Does not touch focus for the *active* tab beyond this sweep — call
    /// [`App::set_focused_window`] afterward if the caller needs the active
    /// tab to land somewhere specific (see `explorer::close_explorer`).
    pub(crate) fn teardown_left_dock(&mut self) -> Option<Dock> {
        let dock = self.left_dock.take()?;
        let slot_idx = self
            .windows
            .get(dock.win_id)
            .and_then(|w| w.as_ref())
            .map(|w| w.slot)
            .unwrap_or(dock.slot_idx);

        self.windows[dock.win_id] = None;
        self.window_folds.remove(&dock.win_id);
        self.window_editors.remove(&dock.win_id);

        if slot_idx < self.slots.len() {
            self.slots.remove(slot_idx);
            self.reindex_after_slot_removal(slot_idx);
        }

        for i in 0..self.tabs.len() {
            if self.tabs[i].focused_window == dock.win_id {
                let fallback = self.tabs[i].layout.leaves().into_iter().next().unwrap_or(0);
                self.tabs[i].focused_window = fallback;
            }
        }

        Some(dock)
    }

    /// Allocate a fresh window over `slot_idx` and install it as the bottom
    /// dock (#63 Phase B — twin of [`Self::install_left_dock`]). `kind` is
    /// always [`DockKind::Quickfix`] or [`DockKind::Loclist`]; callers
    /// (`quickfix::open_bottom_dock_for`) handle sequencing (focus,
    /// window-editor reconcile, buffer content) themselves.
    pub(crate) fn install_bottom_dock(&mut self, slot_idx: usize, kind: DockKind) -> WindowId {
        let win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(window::Window::new(slot_idx)));
        self.bottom_dock = Some(Dock {
            win_id,
            slot_idx,
            kind,
        });
        win_id
    }

    /// Tear down the bottom dock: drop its window entry + folds/editor, remove
    /// its slot (reindexing every other window's slot reference, same as
    /// [`Self::teardown_left_dock`]), and fix up any tab whose remembered
    /// `focused_window` pointed at it. Returns the removed [`Dock`].
    pub(crate) fn teardown_bottom_dock(&mut self) -> Option<Dock> {
        let dock = self.bottom_dock.take()?;
        let slot_idx = self
            .windows
            .get(dock.win_id)
            .and_then(|w| w.as_ref())
            .map(|w| w.slot)
            .unwrap_or(dock.slot_idx);

        self.windows[dock.win_id] = None;
        self.window_folds.remove(&dock.win_id);
        self.window_editors.remove(&dock.win_id);

        if slot_idx < self.slots.len() {
            self.slots.remove(slot_idx);
            self.reindex_after_slot_removal(slot_idx);
        }

        for i in 0..self.tabs.len() {
            if self.tabs[i].focused_window == dock.win_id {
                let fallback = self.tabs[i].layout.leaves().into_iter().next().unwrap_or(0);
                self.tabs[i].focused_window = fallback;
            }
        }

        Some(dock)
    }

    /// `<C-w>c` / `:cclose` / `:lclose` on the bottom dock — twin of
    /// [`Self::close_left_dock`]. Unlike the left dock, the bottom dock has no
    /// separate `Option<ExplorerPane>`-style state to dispatch on: quickfix
    /// and location lists both live directly on `App` (`quickfix` /
    /// `loclist`) and close identically regardless of which one currently
    /// owns the dock, so there's no per-`DockKind` branch to write here.
    /// No-op if the dock is already closed. Fixes focus onto a regular window
    /// only when the dock itself was focused at close time (mirrors
    /// `explorer::close_explorer`).
    pub(crate) fn close_bottom_dock(&mut self) {
        let was_focused = self
            .bottom_dock
            .as_ref()
            .is_some_and(|d| d.win_id == self.focused_window());
        if self.teardown_bottom_dock().is_none() {
            return;
        }
        if was_focused {
            let target = self
                .editor_target_window()
                .or_else(|| self.layout().leaves().into_iter().next())
                .unwrap_or_else(|| self.focused_window());
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Fix up window `slot` pointers AND [`super::App::prev_active`] after
    /// `self.slots[removed_idx]` has been removed. Shared by
    /// [`Self::teardown_left_dock`] and [`Self::teardown_bottom_dock`].
    ///
    /// `prev_active` is a raw slot index (`<C-^>` / `:b#`'s alternate-buffer
    /// pointer) that Phase A's `teardown_left_dock` never fixed up — the
    /// explorer opens/closes rarely enough per session that a stale
    /// `prev_active` was unlikely to bite. The bottom dock opens/closes far
    /// more often (every `:copen`/`:cclose`, every `:grep`/`:make`), which
    /// makes it much more likely for `prev_active` to point at a slot that
    /// used to sit one-past a removed dock slot — silently landing `<C-^>` on
    /// the WRONG buffer post-removal instead of erroring or panicking
    /// (`buffer_alt`'s `i < self.slots.len()` bounds check tolerates a
    /// stale-but-in-range index without ever detecting the drift). Fixed here
    /// for both dock kinds rather than left as a Phase-B-only patch.
    fn reindex_after_slot_removal(&mut self, removed_idx: usize) {
        let slot_count = self.slots.len();
        for win in self.windows.iter_mut().flatten() {
            if win.slot == removed_idx {
                win.slot = 0;
            } else if win.slot > removed_idx {
                win.slot -= 1;
            }
            win.slot = win.slot.min(slot_count.saturating_sub(1));
        }
        self.prev_active = match self.prev_active {
            Some(p) if p == removed_idx => None,
            Some(p) if p > removed_idx => Some(p - 1),
            other => other,
        };
    }

    /// `<C-w>c` / `:close` / `:q` on the left dock — closes the dock itself
    /// rather than touching the tree (#63 Phase A). Dispatches on
    /// [`DockKind`] so a future left-dock kind gets its own toggle-off path
    /// without touching this call site again. No-op if the dock is already
    /// closed.
    pub(crate) fn close_left_dock(&mut self) {
        let Some(kind) = self.left_dock.as_ref().map(|d| d.kind) else {
            return;
        };
        match kind {
            DockKind::Explorer => self.toggle_explorer(),
            // No bottom-dock kind (Quickfix/Loclist) is ever installed as
            // the LEFT dock — `install_left_dock`'s only caller passes
            // `DockKind::Explorer` — so this arm is unreachable in practice;
            // kept exhaustive so a future left-dock kind can't silently fall
            // through without a close path.
            DockKind::Quickfix | DockKind::Loclist => {}
        }
    }

    // ── Membership / lookup ─────────────────────────────────────────────

    /// `true` when `id` is the left dock's window.
    pub(crate) fn is_left_dock(&self, id: WindowId) -> bool {
        self.left_dock.as_ref().is_some_and(|d| d.win_id == id)
    }

    /// `true` when `id` is the bottom dock's window.
    pub(crate) fn is_bottom_dock(&self, id: WindowId) -> bool {
        self.bottom_dock.as_ref().is_some_and(|d| d.win_id == id)
    }

    /// `true` when `id` is any dock's window.
    pub(crate) fn is_dock_window(&self, id: WindowId) -> bool {
        self.is_left_dock(id) || self.is_bottom_dock(id)
    }

    /// Slot index of the bottom dock's scratch buffer, or `None` when no
    /// bottom dock is open. Twin of `explorer::explorer_slot_idx`, but
    /// derived from `bottom_dock.win_id` rather than an `is_explorer`-style
    /// flag on the slot itself — the dock is the ONLY thing that can point a
    /// window at this slot, so its window's `slot` field is already the
    /// single source of truth (#63 Phase B).
    pub(crate) fn qf_dock_slot_idx(&self) -> Option<usize> {
        let win_id = self.bottom_dock.as_ref()?.win_id;
        self.windows.get(win_id)?.as_ref().map(|w| w.slot)
    }

    /// `true` when slot `idx` is a "special" pane slot that must never appear
    /// as a normal user buffer: the explorer OR the bottom quickfix/
    /// location-list dock. Used everywhere `is_explorer` used to be the sole
    /// exclusion check for buffer cycling (`:bn`/`:bp`), `:ls`, the buffer
    /// line, the nvim buffer list, and the top-bar multi-buffer visibility
    /// count — the qf dock slot needs the exact same treatment the explorer
    /// slot already gets, or it would show up as a fake "real" buffer the
    /// moment `:copen` creates it.
    pub(crate) fn slot_is_special(&self, idx: usize) -> bool {
        self.slots.get(idx).is_some_and(|s| s.is_explorer) || self.qf_dock_slot_idx() == Some(idx)
    }

    // ── Frame-level focus navigation ────────────────────────────────────
    //
    // The tree itself stays dock-blind (hjkl-layout is untouched); this is
    // the one layer that knows both the tree AND the docks, so `<C-w>`
    // commands can cross the boundary. `hjkl_layout::neighbor_left` already
    // returns `None` exactly for leaves that are the tree's leftmost column
    // (it accounts for nested vertical splits at every ancestor level), so
    // "tree said no left neighbour" is precisely the condition under which
    // the left dock — which spans the full frame height to the left of the
    // whole tree — is the correct next target.

    /// Left-dock target when tree navigation found no left neighbour for
    /// `fw`. `None` when there's no left dock, or `fw` already IS the left
    /// dock (nothing further left).
    pub(crate) fn dock_neighbor_left(&self, fw: WindowId) -> Option<WindowId> {
        let dock = self.left_dock.as_ref()?;
        if fw == dock.win_id || !self.layout().contains(fw) {
            return None;
        }
        Some(dock.win_id)
    }

    /// Main-area re-entry target when leaving the left dock to the right.
    /// Prefers the last regular window the user focused (if it still lives
    /// in the ACTIVE tab's tree), else the tree's first (top-left-most) leaf.
    pub(crate) fn dock_neighbor_right(&self, fw: WindowId) -> Option<WindowId> {
        if !self.is_left_dock(fw) {
            return None;
        }
        if let Some(last) = self.last_regular_window
            && self.layout().contains(last)
        {
            return Some(last);
        }
        self.layout().leaves().into_iter().next()
    }

    /// Bottom-dock target when tree navigation found no neighbour BELOW `fw`
    /// (#63 Phase B — twin of [`Self::dock_neighbor_left`]). `None` when
    /// there's no bottom dock, `fw` already IS the bottom dock, or `fw` isn't
    /// even in the active tab's tree — which is exactly the left dock's case:
    /// the bottom dock spans only the MAIN AREA's width (below the tree, to
    /// the right of the left dock), so `<C-w>j` from the explorer must NOT
    /// reach it (`layout().contains(explorer_win)` is always `false`, same
    /// guard `dock_neighbor_left` relies on).
    pub(crate) fn dock_neighbor_down(&self, fw: WindowId) -> Option<WindowId> {
        let dock = self.bottom_dock.as_ref()?;
        if fw == dock.win_id || !self.layout().contains(fw) {
            return None;
        }
        Some(dock.win_id)
    }

    /// Main-area re-entry target when leaving the bottom dock upward (`<C-w>k`
    /// from the dock) — twin of [`Self::dock_neighbor_right`].
    pub(crate) fn dock_neighbor_up(&self, fw: WindowId) -> Option<WindowId> {
        if !self.is_bottom_dock(fw) {
            return None;
        }
        if let Some(last) = self.last_regular_window
            && self.layout().contains(last)
        {
            return Some(last);
        }
        self.layout().leaves().into_iter().next()
    }

    /// Focus order for `<C-w>w` / `<C-w>W`: left dock (if open), then the
    /// active tab's tree leaves in pre-order, then the bottom dock (if
    /// open). Vim includes special windows in the cycle, so docks aren't
    /// skipped.
    pub(crate) fn focus_cycle_order(&self) -> Vec<WindowId> {
        let mut order = Vec::new();
        if let Some(d) = &self.left_dock {
            order.push(d.win_id);
        }
        order.extend(self.layout().leaves());
        if let Some(d) = &self.bottom_dock {
            order.push(d.win_id);
        }
        order
    }

    // ── Resize + persistence ────────────────────────────────────────────

    /// Adjust the left dock's configured width by `delta` columns in
    /// memory only (clamped). Does not touch disk — callers decide when to
    /// persist (`<C-w><`/`<C-w>>` persists immediately after this; a mouse
    /// drag persists once on release via [`App::persist_dock_width`]).
    pub(crate) fn resize_dock_width_by(&mut self, delta: i32) {
        let terminal_w = self.last_frame_rect.map(|r| r.width).unwrap_or(80);
        let current = self.config.explorer.width as i32;
        let candidate = (current + delta).clamp(0, u16::MAX as i32) as u16;
        self.config.explorer.width = clamp_dock_width(candidate, terminal_w);
    }

    /// Write the left dock's current configured width back to the user's
    /// config file via a surgical `toml_edit` patch (comments/formatting of
    /// every other key are preserved). No-op — silently, not an error — when
    /// no config path is known (e.g. a test that never called
    /// `with_config_path`, or the platform has no resolvable home dir).
    pub(crate) fn persist_dock_width(&mut self) {
        let Some(path) = self.config_path.clone() else {
            return;
        };
        let width = self.config.explorer.width;
        if let Err(e) = hjkl_config::write_key_at(&path, "explorer.width", width as i64) {
            self.bus.warn(format!("couldn't save explorer width: {e}"));
        }
    }

    /// Adjust the bottom dock's configured height by `delta` rows in memory
    /// only (clamped) — twin of [`Self::resize_dock_width_by`].
    pub(crate) fn resize_dock_height_by(&mut self, delta: i32) {
        let terminal_h = self.last_frame_rect.map(|r| r.height).unwrap_or(24);
        let current = self.config.panel.height as i32;
        let candidate = (current + delta).clamp(0, u16::MAX as i32) as u16;
        self.config.panel.height = clamp_dock_height(candidate, terminal_h);
    }

    /// Write the bottom dock's current configured height back to the user's
    /// config file — twin of [`Self::persist_dock_width`].
    pub(crate) fn persist_dock_height(&mut self) {
        let Some(path) = self.config_path.clone() else {
            return;
        };
        let height = self.config.panel.height;
        if let Err(e) = hjkl_config::write_key_at(&path, "panel.height", height as i64) {
            self.bus.warn(format!("couldn't save panel height: {e}"));
        }
    }

    // ── Session-state persistence (#63 Phase C) ─────────────────────────
    //
    // Unlike `persist_dock_width`/`persist_dock_height` (interactive resize
    // only), open/closed state is written back on every toggle so the dock
    // reopens automatically on the next launch. There is no separate
    // "session file" mechanism anywhere in this codebase to hook into —
    // `hjkl_config::write_key_at`'s surgical TOML patch (the same one Phase
    // A/B already use for dock geometry) IS the closest existing precedent
    // for "runtime state that survives a restart", so this reuses it rather
    // than inventing a new persistence format.

    /// Write the left dock's current open/closed state back to the user's
    /// config file. Called from [`super::App::toggle_explorer`] after every
    /// toggle (both open and close), so `explorer.open` always reflects
    /// reality — including when [`Self::restore_dock_state_from_config`]
    /// itself calls `toggle_explorer` at startup (a harmless rewrite of the
    /// same value already on disk). No-op — silently, matching
    /// [`Self::persist_dock_width`] — when no config path is known.
    pub(crate) fn persist_explorer_open(&mut self) {
        let Some(path) = self.config_path.clone() else {
            return;
        };
        let open = self.explorer.is_some();
        if let Err(e) = hjkl_config::write_key_at(&path, "explorer.open", open) {
            self.bus
                .warn(format!("couldn't save explorer open state: {e}"));
        }
    }

    /// Startup dock-state restore (#63 Phase C). Reopens the left dock iff
    /// `explorer.open` is `true` in the loaded config — going through
    /// [`super::App::toggle_explorer`], the SAME path an interactive
    /// `<leader>e` takes, so every invariant that path maintains (slot
    /// creation, dock installation, initial cursor/reveal, focus) holds for
    /// the restored dock exactly as it would for a fresh open. Must run
    /// after `App::new` + `with_config`/`with_config_path` (needs both the
    /// initial window/slot machinery AND the loaded config); see
    /// `main.rs`'s call site.
    ///
    /// The bottom dock's open/which-list state is deliberately NOT
    /// restored: unlike dock geometry, the quickfix/location-list ENTRIES
    /// are never persisted (`QfList` is plain in-memory state, rebuilt
    /// fresh by `:grep`/`:make`/`:cexpr`/etc. every run) — reopening an
    /// empty `:copen` window on startup would show a blank read-only
    /// buffer with nothing to look at, so there is nothing worth restoring.
    /// Only the left dock, whose content (the file tree) is cheap to
    /// rebuild from disk on every open, gets this treatment.
    pub(crate) fn restore_dock_state_from_config(&mut self) {
        if self.config.explorer.open && self.explorer.is_none() {
            self.toggle_explorer();
            // An interactive open focuses the explorer; a startup RESTORE
            // must not — the user launched `hjkl <file>` to edit the file,
            // so focus belongs in the main area with the tree merely
            // visible (IDE/vim-session convention).
            if let Some(target) = self.editor_target_window() {
                self.switch_focus(target);
            }
        }
    }
}
