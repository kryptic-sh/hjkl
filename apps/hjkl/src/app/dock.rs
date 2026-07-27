//! Per-tab dock windows (window-management refactor, kryptic-sh/hjkl#63).
//!
//! A [`Dock`] owns a real [`WindowId`] + slot index — same as any window —
//! so all existing `window_editors` / focus / dispatch / render machinery
//! applies to it unmodified. Since Phase 3 a dock is also an **ordinary leaf
//! of its tab's [`LayoutTree`](hjkl_layout::LayoutTree)**, pinned and
//! fixed-size:
//!
//! - the explorer weaves in at the root as
//!   `Fixed::First(config.explorer.width)` on a `Vertical` split, so it is
//!   the full-height leftmost column;
//! - the quickfix / location-list dock weaves in as
//!   `Fixed::Second(config.panel.height)` on a `Horizontal` split *inside the
//!   non-explorer side*, so it sits below the tree but to the RIGHT of the
//!   explorer and never spans beneath it.
//!
//! Everything adjacency-shaped therefore comes from the tree itself:
//! `neighbor_direction` crosses in and out of docks, `<C-w>w` cycling walks
//! them in pre-order, and `window_rects` gives them their geometry. What the
//! app still has to say about a dock is only what vim keeps in window/buffer
//! attributes: it is *pinned* (`:only` keeps it, `equalize_all` skips it,
//! `swap_with_sibling` refuses to drag it) and its fixed size is owned by
//! config rather than by a split ratio.
//!
//! Docks are **per-tab** ([`Tab::left_dock`](super::window::Tab) /
//! [`Tab::bottom_dock`](super::window::Tab)), matching vim: `:copen` and the
//! file explorer open a window in the current tab page, every tab page has
//! its own, and closing a tab disposes of its docks with the rest of its
//! windows.

use hjkl_layout::{Fixed, LayoutTree, SplitDir};

use super::window::{self, WindowId};

/// Which feature a dock hosts. Only [`DockKind::Explorer`] is wired up in
/// Phase A; the other two are reserved for the bottom dock landing in a
/// later phase (kryptic-sh/hjkl#63 Phase B) so the type is stable now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockKind {
    /// Left dock: the file-explorer tree.
    Explorer,
    /// Bottom dock: `:copen` quickfix list (#63 Phase B).
    Quickfix,
    /// Bottom dock: `:lopen` location list (#63 Phase B).
    Loclist,
}

/// A pinned, fixed-size leaf of its tab's `LayoutTree`.
#[derive(Debug, Clone)]
pub struct Dock {
    /// The dock's real `WindowId` — has a normal `windows[..]` entry, a
    /// normal `window_editors` entry, and a normal `LayoutTree::Leaf`,
    /// exactly like any other window.
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
pub const DOCK_MIN_WIDTH: u16 = 12;

/// Clamp a candidate left-dock width against both the static sane bounds and
/// the live terminal width, per the approved design ("12..=terminal_width/2").
pub fn clamp_dock_width(width: u16, terminal_width: u16) -> u16 {
    let max = (terminal_width / 2).max(DOCK_MIN_WIDTH);
    width.clamp(DOCK_MIN_WIDTH, max)
}

/// Minimum sane height for the bottom dock — mirrors `panel.height`'s static
/// validation bound (3..=200, see `hjkl_app::config::Config::validate`).
pub const DOCK_MIN_HEIGHT: u16 = 3;

/// Clamp a candidate bottom-dock height against both the static sane bounds
/// and the live terminal height, mirroring [`clamp_dock_width`]'s
/// "12..=terminal_width/2" shape ("3..=terminal_height/2" here).
pub fn clamp_dock_height(height: u16, terminal_height: u16) -> u16 {
    let max = (terminal_height / 2).max(DOCK_MIN_HEIGHT);
    height.clamp(DOCK_MIN_HEIGHT, max)
}

/// Rewrite the `fixed` allocation of the split that has `id` as one of its
/// immediate children, so a dock leaf renders exactly `cells` cells along its
/// parent split's axis.
///
/// `Fixed::First` / `Fixed::Second` is chosen from which side the leaf is on,
/// so the same call works for the left dock (child `a`) and the bottom dock
/// (child `b`). No-op when `id` is not an immediate child of any split — which
/// is the degenerate "dock is the only window" case, where there is no split to
/// size in the first place.
fn resize_dock_leaf(node: &mut LayoutTree, id: WindowId, cells: u16) {
    let LayoutTree::Split { fixed, a, b, .. } = node else {
        return;
    };
    if matches!(a.as_ref(), LayoutTree::Leaf(leaf) if *leaf == id) {
        *fixed = Some(Fixed::First(cells));
        return;
    }
    if matches!(b.as_ref(), LayoutTree::Leaf(leaf) if *leaf == id) {
        *fixed = Some(Fixed::Second(cells));
        return;
    }
    resize_dock_leaf(a, id, cells);
    resize_dock_leaf(b, id, cells);
}

impl super::App {
    // ── Lifecycle ────────────────────────────────────────────────────────

    /// Allocate a fresh window over `slot_idx` and weave it into the ACTIVE
    /// tab's tree as the left dock: a `Vertical` split at the root with the
    /// dock as `a` at `Fixed::First(explorer.width)`, so it is the full-height
    /// leftmost column and the whole previous tree becomes its right sibling.
    ///
    /// Does NOT touch focus — callers (currently only
    /// `explorer::open_explorer`) handle sequencing (window_editors reconcile,
    /// focus, initial cursor) themselves. Returns the new dock's `WindowId`.
    pub(crate) fn install_left_dock(&mut self, slot_idx: usize, kind: DockKind) -> WindowId {
        let win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(window::Window::new(slot_idx)));

        let cells = self.left_dock_cells();
        let tree = self.layout_mut();
        let rest = std::mem::replace(tree, LayoutTree::Leaf(win_id));
        *tree = LayoutTree::split_fixed(
            SplitDir::Vertical,
            0.5,
            Fixed::First(cells),
            LayoutTree::Leaf(win_id),
            rest,
        );

        self.tabs[self.active_tab].left_dock = Some(Dock {
            win_id,
            slot_idx,
            kind,
        });
        win_id
    }

    /// Tear down the active tab's left dock: remove its leaf from the tab's
    /// tree, drop its window entry + folds + editor, and remove its slot
    /// (reindexing every other window's slot reference). Returns the removed
    /// [`Dock`] (its `slot_idx` is the pre-removal value, informational only).
    ///
    /// Does not touch focus beyond repairing a `focused_window` that pointed
    /// at the dock — call [`App::set_focused_window`] afterward if the caller
    /// needs the tab to land somewhere specific (see
    /// `explorer::close_explorer`).
    pub(crate) fn teardown_left_dock(&mut self) -> Option<Dock> {
        let dock = self.tabs[self.active_tab].left_dock.take()?;
        self.dispose_dock_window(&dock);
        Some(dock)
    }

    /// Allocate a fresh window over `slot_idx` and weave it into the ACTIVE
    /// tab's tree as the bottom dock (twin of [`Self::install_left_dock`]): a
    /// `Horizontal` split with the dock as `b` at
    /// `Fixed::Second(panel.height)`.
    ///
    /// The split is inserted **inside the non-explorer side** of the tree, not
    /// at the root, so the dock sits below the tree but to the RIGHT of the
    /// explorer — it must never span beneath the explorer (`<C-w>j` from the
    /// explorer must not reach it). Wrapping the root instead would give
    /// `H{ V{explorer, tree}, dock }`, which is exactly the wrong shape.
    ///
    /// `kind` is always [`DockKind::Quickfix`] or [`DockKind::Loclist`];
    /// callers (`quickfix::open_bottom_dock_for`) handle sequencing (focus,
    /// window-editor reconcile, buffer content) themselves.
    pub(crate) fn install_bottom_dock(&mut self, slot_idx: usize, kind: DockKind) -> WindowId {
        let win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.push(Some(window::Window::new(slot_idx)));

        let cells = self.bottom_dock_cells();
        let explorer_win = self.tabs[self.active_tab]
            .left_dock
            .as_ref()
            .map(|d| d.win_id);
        let root = self.layout_mut();
        // The main area is the whole tree, unless the explorer already owns
        // the root split's `a` — then it is the explorer's sibling.
        let main = match root {
            LayoutTree::Split { a, b, .. }
                if explorer_win
                    .is_some_and(|e| matches!(a.as_ref(), LayoutTree::Leaf(l) if *l == e)) =>
            {
                b.as_mut()
            }
            other => other,
        };
        let rest = std::mem::replace(main, LayoutTree::Leaf(win_id));
        *main = LayoutTree::split_fixed(
            SplitDir::Horizontal,
            0.5,
            Fixed::Second(cells),
            rest,
            LayoutTree::Leaf(win_id),
        );

        self.tabs[self.active_tab].bottom_dock = Some(Dock {
            win_id,
            slot_idx,
            kind,
        });
        win_id
    }

    /// Tear down the active tab's bottom dock — twin of
    /// [`Self::teardown_left_dock`].
    pub(crate) fn teardown_bottom_dock(&mut self) -> Option<Dock> {
        let dock = self.tabs[self.active_tab].bottom_dock.take()?;
        self.dispose_dock_window(&dock);
        Some(dock)
    }

    /// Shared teardown for both docks: unweave the leaf from the tab that owns
    /// it, drop the window's state, and remove its scratch slot.
    ///
    /// The tab is located by searching for the leaf rather than assuming the
    /// active one, so a dock disposed as part of closing some OTHER tab
    /// (`:tabclose`, `:tabonly`) is unwoven from the right tree.
    fn dispose_dock_window(&mut self, dock: &Dock) {
        let slot_idx = self
            .windows
            .get(dock.win_id)
            .and_then(|w| w.as_ref())
            .map_or(dock.slot_idx, |w| w.slot);

        if let Some(i) = (0..self.tabs.len()).find(|&i| self.tabs[i].layout.contains(dock.win_id)) {
            // A dock is never the last leaf (see `regular_leaf_count`'s
            // callers), so `remove_leaf` only fails in a degenerate test
            // layout — leave the tree alone there rather than panicking.
            if let Ok(new_focus) = self.tabs[i].layout.remove_leaf(dock.win_id) {
                if self.tabs[i].focused_window == dock.win_id {
                    self.tabs[i].focused_window = new_focus;
                }
            } else if self.tabs[i].focused_window == dock.win_id {
                let fallback = self.tabs[i].layout.leaves().into_iter().next().unwrap_or(0);
                self.tabs[i].focused_window = fallback;
            }
        }

        self.windows[dock.win_id] = None;
        self.window_folds.remove(&dock.win_id);
        self.window_editors.remove(&dock.win_id);

        if slot_idx < self.slots.len() {
            self.slots.remove(slot_idx);
            self.reindex_after_slot_removal(slot_idx);
        }
    }

    /// Dispose of tab `tab_idx`'s docks before the tab itself is dropped.
    ///
    /// Tab-close paths (`:tabclose`, `:tabonly`, close-tabs-left/right) null
    /// out every window the tab owned, but a dock also owns a scratch SLOT
    /// that would otherwise outlive it — and a leaked quickfix slot stops
    /// being recognised as special the moment its dock record is gone, so it
    /// would resurface as a fake user buffer in `:ls` / `:bn`.
    pub(crate) fn dispose_tab_docks(&mut self, tab_idx: usize) {
        let left = self.tabs[tab_idx].left_dock.take();
        let bottom = self.tabs[tab_idx].bottom_dock.take();
        self.tabs[tab_idx].explorer = None;
        if let Some(d) = left {
            self.dispose_dock_window(&d);
        }
        if let Some(d) = bottom {
            self.dispose_dock_window(&d);
        }
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
            .bottom_dock()
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

    /// `<C-w>c` / `:close` / `:q` on the left dock — routes to the dock's own
    /// toggle-off path so the feature state behind the window (the explorer's
    /// tree model, its scratch slot) comes down with the leaf. Dispatches on
    /// [`DockKind`] so a future left-dock kind gets its own toggle-off path
    /// without touching this call site again. No-op if the dock is already
    /// closed.
    pub(crate) fn close_left_dock(&mut self) {
        let Some(kind) = self.left_dock().map(|d| d.kind) else {
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

    /// The active tab's left dock, when open.
    pub(crate) fn left_dock(&self) -> Option<&Dock> {
        self.tabs[self.active_tab].left_dock.as_ref()
    }

    /// The active tab's bottom dock, when open.
    pub(crate) fn bottom_dock(&self) -> Option<&Dock> {
        self.tabs[self.active_tab].bottom_dock.as_ref()
    }

    /// The active tab's dock windows — the *pinned* set for every tree op that
    /// takes one (`:only`, `equalize_all`, `swap_with_sibling`).
    pub(crate) fn dock_pins(&self) -> Vec<WindowId> {
        let tab = &self.tabs[self.active_tab];
        tab.left_dock
            .iter()
            .chain(tab.bottom_dock.iter())
            .map(|d| d.win_id)
            .collect()
    }

    /// How many leaves of the active tab's tree are NOT docks.
    ///
    /// This is the count every "is this the last window?" decision must use.
    /// Docks are real leaves now, so a bare `leaves().len() > 1` would report
    /// two windows when the user sees one editor plus a quickfix list — and
    /// `:q` / `<C-w>q` would close that editor instead of quitting, leaving a
    /// dock as the last window. A dock must never become the last window.
    pub(crate) fn regular_leaf_count(&self) -> usize {
        self.layout()
            .leaves()
            .into_iter()
            .filter(|&id| !self.is_dock_window(id))
            .count()
    }

    /// `true` when `id` is the active tab's left-dock window.
    pub(crate) fn is_left_dock(&self, id: WindowId) -> bool {
        self.left_dock().is_some_and(|d| d.win_id == id)
    }

    /// `true` when `id` is the active tab's bottom-dock window.
    pub(crate) fn is_bottom_dock(&self, id: WindowId) -> bool {
        self.bottom_dock().is_some_and(|d| d.win_id == id)
    }

    /// `true` when `id` is any dock's window.
    pub(crate) fn is_dock_window(&self, id: WindowId) -> bool {
        self.is_left_dock(id) || self.is_bottom_dock(id)
    }

    /// `true` when slot `idx` backs ANY tab's bottom dock.
    ///
    /// Docks are per-tab, so the active tab's dock is not the only quickfix
    /// scratch slot that can exist — a `:copen` left open in tab 1 must stay
    /// out of `:ls` while the user is looking at tab 2.
    fn is_qf_dock_slot(&self, idx: usize) -> bool {
        self.tabs.iter().any(|t| {
            t.bottom_dock.as_ref().is_some_and(|d| {
                self.windows
                    .get(d.win_id)
                    .and_then(|w| w.as_ref())
                    .is_some_and(|w| w.slot == idx)
            })
        })
    }

    /// `true` when slot `idx` is a "special" pane slot that must never appear
    /// as a normal user buffer: the explorer OR the bottom quickfix/
    /// location-list dock. Used everywhere `is_explorer` used to be the sole
    /// exclusion check for buffer cycling (`:bn`/`:bp`), `:ls`, the buffer
    /// line, the nvim buffer list, and the top-bar multi-buffer visibility
    /// count — the qf dock slot needs the exact same treatment the explorer
    /// slot already gets, or it would show up as a fake "real" buffer the
    /// moment `:copen` creates it.
    /// The command-line window (`q:` / `q/`) is included too: it is a
    /// `LayoutTree` leaf rather than a dock, but it appends a scratch slot the
    /// same way, and that slot is no more a user buffer than the explorer's.
    pub(crate) fn slot_is_special(&self, idx: usize) -> bool {
        self.slots.get(idx).is_some_and(|s| s.is_explorer)
            || self.is_qf_dock_slot(idx)
            || self
                .cmdline_win
                .as_ref()
                .is_some_and(|cw| cw.slot_idx == idx)
    }

    /// How many slots are real user buffers, i.e. what the user would call
    /// "open buffers".
    ///
    /// Dock slots live in `slots` like any other buffer, so a bare
    /// `self.slots.len() > 1` reads "multiple buffers open" the moment the
    /// explorer or a `:copen` dock exists. Anything deciding *user-facing*
    /// multiplicity — `:q` choosing between quitting and `:bdelete`, `H`/`L`
    /// choosing between buffer cycling and viewport motion — must count this
    /// instead, or opening the explorer silently changes what those commands
    /// do.
    pub(crate) fn real_slot_count(&self) -> usize {
        (0..self.slots.len())
            .filter(|&i| !self.slot_is_special(i))
            .count()
    }

    // ── Fixed-size sync ─────────────────────────────────────────────────
    //
    // A dock's size lives in config, not in a split ratio, so the `Fixed`
    // allocation on its parent split is a *projection* of config that has to
    // be refreshed whenever either side moves: config on `<C-w><` / a border
    // drag, and the terminal on a resize (the width is clamped against the
    // live frame so a dock can never crowd out the main area).
    //
    // The value passed is `config.explorer.width` itself, NOT `width - 1`:
    // `Fixed(n)` means "renders exactly n cells" and finds the separator's
    // cell for you, so the config value means the width it says.

    /// Cells the left dock should render, clamped against the live frame.
    fn left_dock_cells(&self) -> u16 {
        let terminal_w = self.last_frame_rect.map_or(80, |r| r.width);
        clamp_dock_width(self.config.explorer.width, terminal_w)
    }

    /// Rows the bottom dock should render, clamped against the live frame.
    fn bottom_dock_cells(&self) -> u16 {
        let terminal_h = self.last_frame_rect.map_or(24, |r| r.height);
        clamp_dock_height(self.config.panel.height, terminal_h)
    }

    /// Re-project the configured dock sizes onto every tab's tree.
    ///
    /// Called once per frame from `render::frame` (after `last_frame_rect` is
    /// known) so the clamp tracks terminal resizes, and directly from the
    /// resize paths so a `<C-w>>` shows up before the next draw. Every tab is
    /// swept, not just the active one, so a background tab's dock is already
    /// the right size the moment it is switched to.
    pub(crate) fn sync_dock_fixed_sizes(&mut self) {
        let left_cells = self.left_dock_cells();
        let bottom_cells = self.bottom_dock_cells();
        for tab in &mut self.tabs {
            if let Some(d) = tab.left_dock.as_ref() {
                resize_dock_leaf(&mut tab.layout, d.win_id, left_cells);
            }
            if let Some(d) = tab.bottom_dock.as_ref() {
                resize_dock_leaf(&mut tab.layout, d.win_id, bottom_cells);
            }
        }
    }

    // ── Resize + persistence ────────────────────────────────────────────

    /// Adjust the left dock's configured width by `delta` columns in
    /// memory only (clamped). Does not touch disk — callers decide when to
    /// persist (`<C-w><`/`<C-w>>` persists immediately after this; a mouse
    /// drag persists once on release via [`App::persist_dock_width`]).
    ///
    /// Deliberately NOT routed through the tree's own resize path: a `Fixed`
    /// split is not resizable there (vim's `winfixwidth`), because its size
    /// belongs to config. This is that owner's resize path.
    pub(crate) fn resize_dock_width_by(&mut self, delta: i32) {
        let terminal_w = self.last_frame_rect.map_or(80, |r| r.width);
        let current = self.config.explorer.width as i32;
        let candidate = (current + delta).clamp(0, u16::MAX as i32) as u16;
        self.config.explorer.width = clamp_dock_width(candidate, terminal_w);
        self.sync_dock_fixed_sizes();
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
        let terminal_h = self.last_frame_rect.map_or(24, |r| r.height);
        let current = self.config.panel.height as i32;
        let candidate = (current + delta).clamp(0, u16::MAX as i32) as u16;
        self.config.panel.height = clamp_dock_height(candidate, terminal_h);
        self.sync_dock_fixed_sizes();
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
        let open = self.tabs[self.active_tab].explorer.is_some();
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
        if self.config.explorer.open && self.tabs[self.active_tab].explorer.is_none() {
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
