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
    /// Bottom dock (Phase B, unused): `:copen` quickfix list.
    #[allow(dead_code)]
    Quickfix,
    /// Bottom dock (Phase B, unused): `:lopen` location list.
    #[allow(dead_code)]
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
            let slot_count = self.slots.len();
            for win in self.windows.iter_mut().flatten() {
                if win.slot == slot_idx {
                    win.slot = 0;
                } else if win.slot > slot_idx {
                    win.slot -= 1;
                }
                win.slot = win.slot.min(slot_count.saturating_sub(1));
            }
        }

        for i in 0..self.tabs.len() {
            if self.tabs[i].focused_window == dock.win_id {
                let fallback = self.tabs[i].layout.leaves().into_iter().next().unwrap_or(0);
                self.tabs[i].focused_window = fallback;
            }
        }

        Some(dock)
    }

    /// `<C-w>c` / `:close` / `:q` on the left dock — closes the dock itself
    /// rather than touching the tree (#63 Phase A). Dispatches on
    /// [`DockKind`] so a future bottom-dock kind gets its own toggle-off
    /// path without touching this call site again. No-op if the dock is
    /// already closed.
    pub(crate) fn close_left_dock(&mut self) {
        let Some(kind) = self.left_dock.as_ref().map(|d| d.kind) else {
            return;
        };
        match kind {
            DockKind::Explorer => self.toggle_explorer(),
            // Reserved for Phase B — no bottom-dock kind is ever installed
            // as the LEFT dock today, so this arm is unreachable in
            // practice; kept exhaustive so a future left-dock kind can't
            // silently fall through without a close path.
            DockKind::Quickfix | DockKind::Loclist => {}
        }
    }

    // ── Membership / lookup ─────────────────────────────────────────────

    /// `true` when `id` is the left dock's window.
    pub(crate) fn is_left_dock(&self, id: WindowId) -> bool {
        self.left_dock.as_ref().is_some_and(|d| d.win_id == id)
    }

    /// `true` when `id` is the bottom dock's window (reserved; always
    /// `false` in Phase A since `bottom_dock` is never populated).
    pub(crate) fn is_bottom_dock(&self, id: WindowId) -> bool {
        self.bottom_dock.as_ref().is_some_and(|d| d.win_id == id)
    }

    /// `true` when `id` is any dock's window.
    pub(crate) fn is_dock_window(&self, id: WindowId) -> bool {
        self.is_left_dock(id) || self.is_bottom_dock(id)
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
}
