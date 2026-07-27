//! Window-tree wrapper — adapts [`hjkl_layout`] types to this TUI crate.
//!
//! All layout logic lives in `hjkl-layout`. This module re-exports the
//! renderer-agnostic types and adds the `App`-specific dispatch methods that
//! bridge `LayoutRect` ↔ `ratatui::layout::Rect`.
//!
//! ## Per-window cursor state (#151 Phase D / Stage 2b)
//!
//! [`AppWindow`] (aliased from `hjkl_layout::Window`) is geometry-only: it
//! carries `slot` (which [`crate::app::BufferSlot`] this window shows) and
//! `last_rect` (the pane rect from the most recent render). It does NOT
//! carry cursor or scroll state.
//!
//! Cursor, scroll, and vim-FSM state instead live on a per-window
//! [`hjkl_engine::Editor`] in [`crate::app::App::window_editors`], keyed by
//! [`WindowId`] — one real `Editor` per open window, each an independent
//! [`hjkl_buffer::View`] onto its slot's shared `Buffer`. A window's editor
//! is the single source of truth for its cursor/scroll at all times; no
//! per-keypress or per-focus-change sync is needed because dispatch always
//! operates on the focused window's own editor directly
//! ([`crate::app::App::active_editor_mut`]).
//!
//! `BufferSlot` itself holds no per-window state at all (Stage 2b removed
//! its own `Editor`) — just the document handle (content, shared across
//! every window on the buffer) and a settings template used to seed a
//! freshly (re)targeted window's editor.

pub use hjkl_layout::{Axis, LayoutRect, LayoutTree, SplitDir, WindowId};

/// Per-tab state: the window arrangement, its focused window, and the tab's
/// own docks.
///
/// This shadows [`hjkl_layout::Tab`] (same `layout` / `focused_window` field
/// shape, so every `tabs[i].layout` call site is unchanged) and adds the
/// per-tab dock state that used to live globally on `App` (#63 Phase 3).
/// Docks are ordinary pinned, fixed-size leaves of `layout` now, so the
/// [`Dock`](super::dock::Dock) records here are *bookkeeping* — which leaf is
/// which dock, and what it hosts — not a parallel window list.
///
/// Per-tab is what vim does: `:copen` and netrw open a window in the current
/// tab page, and every tab page has its own. Opening the explorer in tab 2
/// leaves tab 1 alone, and closing a tab disposes of its docks along with its
/// other windows.
#[derive(Debug, Clone)]
pub struct Tab {
    /// Spatial layout tree for this tab. Leaves reference [`WindowId`]s —
    /// including this tab's dock windows.
    pub layout: LayoutTree,
    /// The window that has focus within this tab.
    pub focused_window: WindowId,
    /// This tab's left dock (the file explorer), when open.
    pub(crate) left_dock: Option<super::dock::Dock>,
    /// This tab's bottom dock (quickfix / location list), when open.
    pub(crate) bottom_dock: Option<super::dock::Dock>,
    /// This tab's explorer pane state (tree model, reconcile baseline, undo
    /// stacks). `Some` exactly when `left_dock` is — kept in lockstep by
    /// `explorer::open_explorer` / `close_explorer`.
    pub(crate) explorer: Option<super::explorer::ExplorerPane>,
}

impl Tab {
    /// Create a new tab with the given layout and focused window, and no
    /// docks — a fresh tab starts dock-free, like a fresh vim tab page.
    pub fn new(layout: LayoutTree, focused_window: WindowId) -> Self {
        Self {
            layout,
            focused_window,
            left_dock: None,
            bottom_dock: None,
            explorer: None,
        }
    }
}

impl Default for Tab {
    fn default() -> Self {
        Self::new(LayoutTree::Leaf(0), 0)
    }
}

// Re-export hjkl_layout::Window as AppWindow so callers that imported Window
// continue to compile. This also preserves the public field shape (slot,
// last_rect) used across the app.
pub use hjkl_layout::Window as AppWindow;

// Keep `Window` importable as well for backward compat within this crate.
pub use hjkl_layout::Window;

// ── Rect conversion helpers ───────────────────────────────────────────────────

/// Convert a ratatui `Rect` to the renderer-agnostic [`LayoutRect`].
#[inline]
pub fn rect_to_layout(r: ratatui::layout::Rect) -> LayoutRect {
    LayoutRect::new(r.x, r.y, r.width, r.height)
}

/// Convert a [`LayoutRect`] back to a ratatui `Rect` — the inverse of
/// [`rect_to_layout`], used by the renderer to draw what `hjkl_layout`
/// computed.
#[inline]
pub fn layout_to_rect(r: LayoutRect) -> ratatui::layout::Rect {
    ratatui::layout::Rect {
        x: r.x,
        y: r.y,
        width: r.w,
        height: r.h,
    }
}

/// Why a window was not taken out of its tab's layout.
///
/// A type rather than a `bool` so every refusal has to be matched on: adding
/// a second reason later is a compile error at each call site instead of a
/// silently-wrong branch, which is the failure mode this whole area
/// (#63) exists to stamp out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseRefused {
    /// The focused window is the last REGULAR (non-dock) leaf of its tab.
    /// Removing it would leave a dock as the tab's last window — the user
    /// stranded in a quickfix list or file tree with nothing to edit — or
    /// leave the tab with no windows at all.
    ///
    /// Callers differ on what to do about it: `<C-w>c` reports E444, `<C-w>T`
    /// reports E1, `:q` / `<C-w>q` quit the editor instead.
    LastRegularWindow,
}

// ── App window-action dispatcher ──────────────────────────────────────────────

use super::App;

impl App {
    /// Dispatch a window-management [`crate::keymap_actions::AppAction`].
    ///
    /// Handles variants:
    ///   - FocusLeft / FocusBelow / FocusAbove / FocusRight
    ///   - FocusNext / FocusPrev
    ///   - CloseFocusedWindow / OnlyFocusedWindow
    ///   - SwapWithSibling / MoveWindowToNewTab
    ///   - NewSplit
    ///   - ResizeHeight / ResizeWidth
    ///   - EqualizeLayout / MaximizeHeight / MaximizeWidth
    ///   - TmuxNavigate (focus neighbour or fall through to tmux select-pane)
    pub(crate) fn dispatch_window_action(
        &mut self,
        action: crate::keymap_actions::AppAction,
        count: usize,
    ) {
        use crate::keymap_actions::AppAction;
        match action {
            AppAction::FocusLeft => self.focus_left(),
            AppAction::FocusBelow => self.focus_below(),
            AppAction::FocusAbove => self.focus_above(),
            AppAction::FocusRight => self.focus_right(),
            AppAction::FocusNext => self.focus_next(),
            AppAction::FocusPrev => self.focus_previous(),
            AppAction::CloseFocusedWindow => self.close_focused_window(),
            AppAction::OnlyFocusedWindow => self.only_focused_window(),
            AppAction::SwapWithSibling => self.swap_with_sibling(),
            AppAction::MoveWindowToNewTab => match self.move_window_to_new_tab() {
                Ok(()) => {
                    self.bus.info("moved window to new tab");
                }
                Err(msg) => {
                    self.bus.error(msg.to_string());
                }
            },
            AppAction::NewSplit => self.dispatch_ex("new"),
            AppAction::ResizeHeight(delta) => {
                // `<C-w>+` / `<C-w>-` while the bottom dock is focused resize
                // the dock (config-driven, persisted) instead of a tree
                // split — twin of the `ResizeWidth` / left-dock case below.
                if self.is_bottom_dock(self.focused_window()) {
                    self.resize_dock_height_by(delta * count as i32);
                    self.persist_dock_height();
                } else {
                    self.resize_height(delta * count as i32);
                }
            }
            AppAction::ResizeWidth(delta) => {
                // `<C-w><` / `<C-w>>` while the left dock is focused resize
                // the dock (config-driven, persisted) instead of a tree
                // split. The dock's own enclosing split is `Fixed`, which the
                // tree path refuses to touch (vim's `winfixwidth`) — this is
                // the resize path its config owner exposes instead.
                if self.is_left_dock(self.focused_window()) {
                    self.resize_dock_width_by(delta * count as i32);
                    self.persist_dock_width();
                } else {
                    self.resize_width(delta * count as i32);
                }
            }
            AppAction::EqualizeLayout => self.equalize_layout(),
            AppAction::MaximizeHeight => self.maximize_height(),
            AppAction::MaximizeWidth => self.maximize_width(),
            AppAction::TmuxNavigate(dir) => self.dispatch_tmux_navigate(dir),
            _ => {}
        }
    }

    /// `<C-h/j/k/l>` — focus the neighbour window or fall through to tmux.
    ///
    /// When a window neighbour exists in `dir`, focuses it. When no neighbour
    /// exists and `$TMUX` is set, forwards to `tmux select-pane`.
    pub(crate) fn dispatch_tmux_navigate(&mut self, dir: super::NavDir) {
        use super::NavDir;
        let focused = self.focused_window();
        // Docks are ordinary leaves (#63 Phase 3), so the tree's own
        // adjacency already crosses in and out of them — `<C-h>` at the
        // leftmost regular window with the explorer open finds the explorer
        // here, and only a genuinely edge-most window falls through to tmux.
        let neighbour = match dir {
            NavDir::Left => self.layout().neighbor_left(focused),
            NavDir::Down => self.layout().neighbor_below(focused),
            NavDir::Up => self.layout().neighbor_above(focused),
            NavDir::Right => self.layout().neighbor_right(focused),
        };
        if neighbour.is_some() {
            match dir {
                NavDir::Left => self.focus_left(),
                NavDir::Down => self.focus_below(),
                NavDir::Up => self.focus_above(),
                NavDir::Right => self.focus_right(),
            }
        } else if std::env::var("TMUX").is_ok() {
            let flag = match dir {
                NavDir::Left => "-L",
                NavDir::Down => "-D",
                NavDir::Up => "-U",
                NavDir::Right => "-R",
            };
            let _ = std::process::Command::new("tmux")
                .args(["select-pane", flag])
                .status();
        }
    }

    // ── Window focus navigation ───────────────────────────────────────────

    /// Move focus to the window below the current one (`Ctrl-w j`).
    ///
    /// Docks are ordinary leaves, so this is plain tree navigation — the
    /// bottom dock is found because it genuinely is the leaf below the tree,
    /// and it is NOT found from the explorer because it is nested inside the
    /// explorer's sibling rather than beneath it (#63 Phase 3).
    pub fn focus_below(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_below(fw) {
            self.switch_focus(target);
        }
    }

    /// Move focus to the window above the current one (`Ctrl-w k`).
    pub fn focus_above(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_above(fw) {
            self.switch_focus(target);
        }
    }

    /// Move focus to the window left of the current one (`Ctrl-w h`).
    pub fn focus_left(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_left(fw) {
            self.switch_focus(target);
        }
    }

    /// Move focus to the window right of the current one (`Ctrl-w l`).
    pub fn focus_right(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_right(fw) {
            self.switch_focus(target);
        }
    }

    /// Move focus to the next window, wrapping around (`Ctrl-w w`). Cycle
    /// order includes open docks — vim includes special windows in the
    /// `<C-w>w` cycle.
    pub fn focus_next(&mut self) {
        let fw = self.focused_window();
        let order = self.layout().leaves();
        if let Some(pos) = order.iter().position(|&id| id == fw) {
            self.switch_focus(order[(pos + 1) % order.len()]);
        }
    }

    /// Move focus to the previous window, wrapping around (`Ctrl-w W`).
    pub fn focus_previous(&mut self) {
        let fw = self.focused_window();
        let order = self.layout().leaves();
        if let Some(pos) = order.iter().position(|&id| id == fw) {
            let len = order.len();
            self.switch_focus(order[(pos + len - 1) % len]);
        }
    }

    /// Close all windows except the focused one — and every dock, which is
    /// pinned ([`hjkl_layout::LayoutTree::only`] keeps pinned leaves and their
    /// enclosing fixed splits, so the dock's position and size survive the
    /// collapse).
    ///
    /// When the focused window IS a dock this is a no-op: `:only` from a
    /// special window must not collapse the regular splits behind it (vim's
    /// behaviour), and `only(dock, pinned)` would keep only docks.
    pub fn only_focused_window(&mut self) {
        let focused = self.focused_window();
        if self.is_dock_window(focused) {
            return;
        }
        if !self.layout().contains(focused) {
            return;
        }
        let pinned = self.dock_pins();
        let removed = self.layout_mut().only(focused, &pinned);
        for id in removed {
            self.windows[id] = None;
            self.window_folds.remove(&id);
        }
        self.bus.info("only");
    }

    /// Swap the focused leaf with its sibling in the immediately enclosing
    /// Split. No-op (with no message) when the focused window is the only one,
    /// when it is a dock, or when its sibling subtree holds one — a dock must
    /// not be dragged across the layout, nor another window dragged through it.
    pub fn swap_with_sibling(&mut self) {
        let focused = self.focused_window();
        let pinned = self.dock_pins();
        if self.layout_mut().swap_with_sibling(focused, &pinned) {
            self.bus.info("swap");
        }
    }

    /// Move the focused window to a new tab (`Ctrl-w T`).
    ///
    /// Fails if the current tab has only one window (vim's "E1: at last window").
    /// On success: the window is removed from the current tab's layout (the
    /// previous tab gets focus on its new top leaf), and a new tab is appended
    /// containing only the moved window.
    pub fn move_window_to_new_tab(&mut self) -> Result<(), &'static str> {
        let focused = self.focused_window();
        // A dock belongs to its tab's frame, not to a buffer the user is
        // editing — moving one "to a new tab" would strand the explorer or a
        // quickfix list as a tab's only window. Clean no-op with its own
        // message rather than the "E1" check below.
        if self.is_dock_window(focused) || !self.layout().contains(focused) {
            return Err("dock windows can't move to a new tab");
        }
        // Save cursor/scroll of the focused window before changing focus.
        self.sync_viewport_from_editor();
        // Detach the focused leaf from the current tab's layout. The returned
        // value is the leaf that should receive focus in the current tab.
        // `detach_focused_leaf` is where "would this strand a dock as the last
        // window?" is decided — moving the one regular window out is the same
        // loss as closing it, so it gets the same answer and E1 is just this
        // path's phrasing of the refusal.
        let new_focus_in_old_tab = self
            .detach_focused_leaf()
            .map_err(|CloseRefused::LastRegularWindow| "E1: only one window in this tab")?;
        // Update the old tab's focused window to the surviving sibling.
        self.tabs[self.active_tab].focused_window = new_focus_in_old_tab;

        // Create a new tab containing only the moved window.
        let new_tab = Tab::new(LayoutTree::Leaf(focused), focused);
        self.tabs.push(new_tab);
        self.active_tab = self.tabs.len() - 1;
        // Restore the moved window's cursor/scroll into the editor.
        self.sync_viewport_to_editor();
        Ok(())
    }

    /// Detach the focused window's leaf from the active tab's layout, or
    /// refuse — **the** chokepoint for "a dock must never be the last
    /// window".
    ///
    /// Every path that takes a window out of a tab's tree goes through here:
    /// `:q`, `:close`, `<C-w>c`, `<C-w>q` and `<C-w>T`. The invariant stopped
    /// being structural when docks became real leaves (#63 Phase 3) — docks
    /// are counted by `leaves().len()`, so the check became a `regular_leaf_count`
    /// comparison repeated at four call sites, and a fifth site that forgot it
    /// would fail silently (the user ends up staring at a quickfix list with no
    /// editor). One function owns the count now, and a caller can only get the
    /// decision wrong by not calling it at all.
    ///
    /// Returns the leaf that inherits focus in this tab. Docks are exempt:
    /// closing a dock never strands anything, and dock teardown is routed
    /// through the dock's own path anyway (it owns feature state a bare leaf
    /// removal would leak).
    fn detach_focused_leaf(&mut self) -> Result<WindowId, CloseRefused> {
        let focused = self.focused_window();
        if !self.is_dock_window(focused) && self.regular_leaf_count() <= 1 {
            return Err(CloseRefused::LastRegularWindow);
        }
        // With a dock open the tree still has two leaves after this one goes,
        // so `remove_leaf` succeeds where the count above would have refused —
        // it is not a second opinion, just the mechanics. A failure here means
        // a degenerate single-leaf tree, which is the same refusal.
        self.layout_mut()
            .remove_leaf(focused)
            .map_err(|_| CloseRefused::LastRegularWindow)
    }

    /// Close the focused window.  Reports E444 when it is the last one.  On
    /// success the layout collapses and focus moves to the sibling that took
    /// over.
    ///
    /// Thin wrapper over [`Self::close_focused_window_checked`] that turns a
    /// refusal into vim's message; callers that want to do something else on
    /// refusal (`:q` quits, `<C-w>q` quits) call the checked form directly.
    pub fn close_focused_window(&mut self) {
        if self.close_focused_window_checked().is_err() {
            self.bus.error("E444: Cannot close last window");
        }
    }

    /// Close the focused window, or refuse **without** a status message.
    ///
    /// `Err(CloseRefused::LastRegularWindow)` means nothing was touched: the
    /// focused window is the tab's last regular one, so closing it would leave
    /// the user with only docks. What that means is the caller's decision —
    /// `<C-w>c` reports E444, `:q` and `<C-w>q` quit the editor instead — but
    /// the decision itself is made in exactly one place
    /// ([`Self::detach_focused_leaf`]).
    ///
    /// When the focused window is the command-line window (issue #37) or a
    /// dock, its own teardown path runs instead of the normal one (they own
    /// transient slots / feature state a bare leaf removal would leak), and
    /// the result is always `Ok`.
    pub fn close_focused_window_checked(&mut self) -> Result<(), CloseRefused> {
        // Cmdline window: delegate to its own cleanup.
        if self.is_cmdline_win_focused() {
            self.close_cmdline_window();
            return Ok(());
        }
        // Dock: the leaf removal is the same `remove_leaf` every window gets,
        // but a dock also owns feature state (the explorer's tree model, the
        // dock's scratch slot) that has to come down with it — so route
        // through the dock's own close path, which does both.
        let focused = self.focused_window();
        if self.is_left_dock(focused) {
            self.close_left_dock();
            return Ok(());
        }
        if self.is_bottom_dock(focused) {
            self.close_bottom_dock();
            return Ok(());
        }

        // Capture commit context BEFORE the window is torn down so we can
        // run `git commit` after the close succeeds. `:wq`/`:x` already ran
        // `do_save` before reaching here (ExEffect::Quit in ex_dispatch.rs:441),
        // so the file on disk reflects the user's edits at this point.
        let commit_ctx = self.windows[focused]
            .as_ref()
            .and_then(|w| self.slots.get(w.slot))
            .and_then(|s| s.commit_ctx.clone());

        let new_focus = self.detach_focused_leaf()?;
        self.windows[focused] = None;
        self.window_folds.remove(&focused);
        // switch_focus saves the outgoing window then restores the incoming
        // one — but the outgoing window is being closed, so we set the new
        // focus directly and only restore the incoming.
        self.set_focused_window(new_focus);
        self.sync_viewport_to_editor();
        self.bus.info("window closed");

        // Commit-on-close: if this was a gc commit buffer, run git commit.
        if let Some(ctx) = commit_ctx {
            match hjkl_app::git::commit_with_file(&ctx.root, &ctx.msg_file) {
                Ok(out) => {
                    let first = out.lines().next().unwrap_or("committed").to_string();
                    self.bus.info(first);
                }
                Err(e) => {
                    let first = e.lines().next().unwrap_or("commit failed").to_string();
                    self.bus.warn(first);
                }
            }
            let _ = std::fs::remove_file(&ctx.msg_file);
            self.recompute_explorer_git_base();
            self.refresh_git_signs_force();
            self.explorer_rebuild_buffer();
        }
        Ok(())
    }

    // ── Window size manipulation ───────────────────────────────────────────

    /// Adjust the focused window's height by `delta` lines. Positive grows,
    /// negative shrinks. Clamps so neither sibling drops below 1 line.
    /// No-op when there is no enclosing Horizontal split or last_rect is None.
    pub fn resize_height(&mut self, delta: i32) {
        let fw = self.focused_window();
        if let Some((ratio, Some(rect), in_a)) = self
            .layout_mut()
            .enclosing_split_mut(fw, SplitDir::Horizontal)
        {
            let parent_h = rect.h as i32;
            if parent_h < 2 {
                return;
            }
            let current_focused_height = if in_a {
                (parent_h as f32 * *ratio) as i32
            } else {
                (parent_h as f32 * (1.0 - *ratio)) as i32
            };
            let new_focused = (current_focused_height + delta).clamp(1, parent_h - 1);
            let new_ratio = if in_a {
                new_focused as f32 / parent_h as f32
            } else {
                (parent_h - new_focused) as f32 / parent_h as f32
            };
            *ratio = new_ratio.clamp(0.01, 0.99);
        }
    }

    /// Adjust the focused window's width by `delta` columns. Positive grows,
    /// negative shrinks. Clamps so neither sibling drops below 1 column.
    /// No-op when there is no enclosing Vertical split or last_rect is None.
    pub fn resize_width(&mut self, delta: i32) {
        let fw = self.focused_window();
        if let Some((ratio, Some(rect), in_a)) = self
            .layout_mut()
            .enclosing_split_mut(fw, SplitDir::Vertical)
        {
            let parent_w = rect.w as i32;
            if parent_w < 2 {
                return;
            }
            let current_focused_width = if in_a {
                (parent_w as f32 * *ratio) as i32
            } else {
                (parent_w as f32 * (1.0 - *ratio)) as i32
            };
            let new_focused = (current_focused_width + delta).clamp(1, parent_w - 1);
            let new_ratio = if in_a {
                new_focused as f32 / parent_w as f32
            } else {
                (parent_w - new_focused) as f32 / parent_w as f32
            };
            *ratio = new_ratio.clamp(0.01, 0.99);
        }
    }

    /// Equalize all splits to 0.5 ratio, leaving this tab's docks at their
    /// configured size (they are the pinned set).
    pub fn equalize_layout(&mut self) {
        let pinned = self.dock_pins();
        self.layout_mut().equalize_all(&pinned);
    }

    /// Resize the split whose `last_rect` encompasses `split_origin` and
    /// `split_total` so the boundary sits at `split_pos` cells from the
    /// split origin. `split_pos` is clamped to leave at least
    /// `SPLIT_MIN_SIZE_COLS` / `SPLIT_MIN_SIZE_ROWS` on each side.
    ///
    /// Called by the border-drag handler in the event loop (Phase 9).
    /// `orientation` determines whether we're moving a column (VSplit) or
    /// a row (HSplit) boundary.
    ///
    /// **Dragging the border of a fixed split is refused** (#63 Phase 2): the
    /// split is found, and then deliberately left alone. A `Fixed` allocation
    /// is owned by whoever set it — for a dock, the persisted config
    /// width/height — so a drag must go through that owner's own resize path
    /// (`resize_dock_width_by` / `resize_dock_height_by`, which the event loop
    /// already routes dock borders to), not silently rewrite a `ratio` the
    /// renderer is ignoring. Same rule as
    /// [`hjkl_layout::LayoutTree::enclosing_split_mut`], which keeps
    /// `<C-w>+`/`<C-w><` off fixed splits.
    pub(crate) fn resize_split_to(
        &mut self,
        orientation: super::mouse::SplitOrientation,
        split_origin: u16,
        split_total: u16,
        split_pos: u16,
    ) {
        let min_size = match orientation {
            super::mouse::SplitOrientation::Vertical => super::SPLIT_MIN_SIZE_COLS,
            super::mouse::SplitOrientation::Horizontal => super::SPLIT_MIN_SIZE_ROWS,
        };

        if split_total < min_size * 2 + 1 {
            return; // too small to resize
        }

        // Clamp split_pos so both children stay at least min_size.
        let clamped = split_pos.clamp(min_size, split_total.saturating_sub(min_size + 1));
        let new_ratio = clamped as f32 / split_total as f32;
        let new_ratio = new_ratio.clamp(0.01, 0.99);

        // Find the matching split node by walking the layout tree and looking
        // for a Split whose last_rect matches the origin + total we recorded
        // when the drag started.
        let dir = match orientation {
            super::mouse::SplitOrientation::Vertical => SplitDir::Vertical,
            super::mouse::SplitOrientation::Horizontal => SplitDir::Horizontal,
        };
        fn update_matching(
            node: &mut LayoutTree,
            dir: SplitDir,
            origin: u16,
            total: u16,
            new_ratio: f32,
        ) {
            if let LayoutTree::Split {
                dir: my_dir,
                ratio,
                fixed,
                a,
                b,
                last_rect,
            } = node
            {
                if *my_dir == dir
                    && let Some(r) = last_rect
                {
                    // Match on Axis (exhaustive) so new SplitDir variants
                    // cause a compile error rather than a runtime panic.
                    let (rect_origin, rect_total) = match dir.axis() {
                        Axis::Col => (r.x, r.w),
                        Axis::Row => (r.y, r.h),
                    };
                    if rect_origin == origin && rect_total == total {
                        // Target found. A fixed split keeps its size — see the
                        // doc comment above; drop the drag rather than write a
                        // ratio the renderer ignores.
                        if fixed.is_none() {
                            *ratio = new_ratio;
                        }
                        return;
                    }
                }
                update_matching(a, dir, origin, total, new_ratio);
                update_matching(b, dir, origin, total, new_ratio);
            }
        }
        update_matching(self.layout_mut(), dir, split_origin, split_total, new_ratio);
    }

    /// Equalize all splits (set every ratio to 0.5). Used by double-click on a
    /// border (Phase 9). Delegates to the existing `equalize_layout`.
    pub(crate) fn equalize_split(&mut self) {
        self.equalize_layout();
    }

    // ── nvim-api window accessors ─────────────────────────────────────────

    /// Window ids that belong to tab `tab_idx`, in layout-leaf order.
    /// Returns `None` when `tab_idx` is out of bounds.
    pub(crate) fn nvim_tab_window_ids(&self, tab_idx: usize) -> Option<Vec<u64>> {
        let tab = self.tabs.get(tab_idx)?;
        Some(
            tab.layout
                .leaves()
                .into_iter()
                .map(|id| id as u64)
                .collect(),
        )
    }

    /// All open window ids (index `i` where `windows[i].is_some()`), as `u64`.
    pub(crate) fn nvim_window_ids(&self) -> Vec<u64> {
        self.windows
            .iter()
            .enumerate()
            .filter_map(|(i, w)| w.as_ref().map(|_| i as u64))
            .collect()
    }

    /// The currently focused window id, as `u64`.
    pub(crate) fn nvim_current_window_id(&self) -> u64 {
        self.focused_window() as u64
    }

    /// `true` when `id` refers to an open window.
    pub(crate) fn nvim_window_is_valid(&self, id: u64) -> bool {
        self.windows.get(id as usize).is_some_and(Option::is_some)
    }

    /// View id of the slot backing window `id`, or `None` when `id` is invalid.
    pub(crate) fn nvim_window_buffer_id(&self, id: u64) -> Option<u64> {
        let slot = self.windows.get(id as usize)?.as_ref()?.slot;
        self.slots.get(slot).map(|s| s.buffer_id)
    }

    /// Focus window `id`. Returns `true` on success, `false` when `id` is invalid.
    pub(crate) fn nvim_set_focused_window_checked(&mut self, id: u64) -> bool {
        if !self.nvim_window_is_valid(id) {
            return false;
        }
        self.switch_focus(id as super::window::WindowId);
        true
    }

    /// Point window `win` at the slot whose `buffer_id == buffer_id`.
    /// Returns `true` on success; `false` when the window or buffer is invalid.
    pub(crate) fn nvim_set_window_buffer(&mut self, win: u64, buffer_id: u64) -> bool {
        if !self.nvim_window_is_valid(win) {
            return false;
        }
        let Some(slot) = self.nvim_slot_index_for_buffer(buffer_id) else {
            return false;
        };
        if let Some(Some(w)) = self.windows.get_mut(win as usize) {
            w.slot = slot;
        } else {
            return false;
        }
        self.reconcile_window_editors();
        true
    }

    /// Cursor `(row, col)` for window `id` (0-based char-col), or `None` when invalid.
    pub(crate) fn nvim_window_cursor(&self, id: u64) -> Option<(usize, usize)> {
        let win_id = id as super::window::WindowId;
        if !self.nvim_window_is_valid(id) {
            return None;
        }
        let ed = self.window_editor(win_id);
        Some(ed.cursor())
    }

    /// Set cursor for window `id` to `(row, col)` (0-based char-col).
    /// Returns `true` on success, `false` when `id` is invalid.
    pub(crate) fn nvim_set_window_cursor(&mut self, id: u64, row: usize, col: usize) -> bool {
        let win_id = id as super::window::WindowId;
        if !self.nvim_window_is_valid(id) {
            return false;
        }
        self.window_editors
            .get_mut(&win_id)
            .expect("window_editors must have an entry for every open window")
            .jump_cursor(row, col);
        true
    }

    /// Maximize focused window's height — set every enclosing Horizontal
    /// split so the focused branch gets as much height as possible (siblings
    /// collapse to 1 line each).
    pub fn maximize_height(&mut self) {
        let focused = self.focused_window();
        self.layout_mut()
            .for_each_ancestor(focused, &mut |dir, ratio, in_a, rect| {
                if dir != SplitDir::Horizontal {
                    return;
                }
                if let Some(r) = rect {
                    let h = r.h as f32;
                    if h < 2.0 {
                        return;
                    }
                    let max_branch = (h - 1.0) / h;
                    let min_branch = 1.0 / h;
                    *ratio = if in_a { max_branch } else { min_branch };
                }
            });
    }

    /// Maximize focused window's width — set every enclosing Vertical split
    /// so the focused branch gets as much width as possible (siblings collapse
    /// to 1 column each).
    pub fn maximize_width(&mut self) {
        let focused = self.focused_window();
        self.layout_mut()
            .for_each_ancestor(focused, &mut |dir, ratio, in_a, rect| {
                if dir != SplitDir::Vertical {
                    return;
                }
                if let Some(r) = rect {
                    let w = r.w as f32;
                    if w < 2.0 {
                        return;
                    }
                    let max_branch = (w - 1.0) / w;
                    let min_branch = 1.0 / w;
                    *ratio = if in_a { max_branch } else { min_branch };
                }
            });
    }
}
