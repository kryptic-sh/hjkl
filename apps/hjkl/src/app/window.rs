//! Window-tree wrapper — adapts [`hjkl_layout`] types to this TUI crate.
//!
//! All layout logic lives in `hjkl-layout`. This module re-exports the
//! renderer-agnostic types and adds the `App`-specific dispatch methods that
//! bridge `LayoutRect` ↔ `ratatui::layout::Rect`.

pub use hjkl_layout::{LayoutRect, LayoutTree, SplitDir, Tab, Window, WindowId};

// ── Rect conversion helpers ───────────────────────────────────────────────────

/// Convert a ratatui `Rect` to the renderer-agnostic [`LayoutRect`].
#[inline]
pub fn rect_to_layout(r: ratatui::layout::Rect) -> LayoutRect {
    LayoutRect::new(r.x, r.y, r.width, r.height)
}

/// Convert a [`LayoutRect`] back to a ratatui `Rect`.
#[allow(dead_code)]
#[inline]
pub fn layout_to_rect(r: LayoutRect) -> ratatui::layout::Rect {
    ratatui::layout::Rect {
        x: r.x,
        y: r.y,
        width: r.w,
        height: r.h,
    }
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
            AppAction::ResizeHeight(delta) => self.resize_height(delta * count as i32),
            AppAction::ResizeWidth(delta) => self.resize_width(delta * count as i32),
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
    pub fn focus_below(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_below(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Move focus to the window above the current one (`Ctrl-w k`).
    pub fn focus_above(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_above(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Move focus to the window left of the current one (`Ctrl-w h`).
    pub fn focus_left(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_left(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Move focus to the window right of the current one (`Ctrl-w l`).
    pub fn focus_right(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().neighbor_right(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Move focus to the next window in pre-order traversal, wrapping around (`Ctrl-w w`).
    pub fn focus_next(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().next_leaf(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Move focus to the previous window in pre-order traversal, wrapping around (`Ctrl-w W`).
    pub fn focus_previous(&mut self) {
        let fw = self.focused_window();
        if let Some(target) = self.layout().prev_leaf(fw) {
            self.sync_viewport_from_editor();
            self.set_focused_window(target);
            self.sync_viewport_to_editor();
        }
    }

    /// Close all windows except the focused one. Replaces the layout with a
    /// single leaf and drops the `Option<Window>` entries for all other windows.
    pub fn only_focused_window(&mut self) {
        let focused = self.focused_window();
        let all_leaves = self.layout().leaves();
        for id in all_leaves {
            if id != focused {
                self.windows[id] = None;
            }
        }
        *self.layout_mut() = LayoutTree::Leaf(focused);
        self.bus.info("only");
    }

    /// Swap the focused leaf with its sibling in the immediately enclosing
    /// Split. No-op (with no message) when the focused window is the only one.
    pub fn swap_with_sibling(&mut self) {
        let focused = self.focused_window();
        if self.layout_mut().swap_with_sibling(focused) {
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
        if self.layout().leaves().len() <= 1 {
            return Err("E1: only one window in this tab");
        }
        self.sync_viewport_from_editor();
        // Remove the focused leaf from the current tab's layout. The returned
        // value is the leaf that should receive focus in the current tab.
        let new_focus_in_old_tab = self
            .layout_mut()
            .remove_leaf(focused)
            .map_err(|_| "remove_leaf failed")?;
        // Update the old tab's focused window to the surviving sibling.
        self.tabs[self.active_tab].focused_window = new_focus_in_old_tab;

        // Create a new tab containing only the moved window.
        let new_tab = Tab::new(LayoutTree::Leaf(focused), focused);
        self.tabs.push(new_tab);
        self.active_tab = self.tabs.len() - 1;
        self.sync_viewport_to_editor();
        Ok(())
    }

    /// Close the focused window.  Fails (with status message) when only one
    /// window remains.  On success the layout collapses and focus moves to the
    /// sibling that took over.
    ///
    /// When the focused window is the command-line window (issue #37), the
    /// transient slot is cleaned up via `close_cmdline_window` instead of the
    /// normal path to avoid leaving orphaned slots.
    pub fn close_focused_window(&mut self) {
        // Cmdline window: delegate to its own cleanup.
        if self.is_cmdline_win_focused() {
            self.close_cmdline_window();
            return;
        }
        let focused = self.focused_window();
        match self.layout_mut().remove_leaf(focused) {
            Err(_) => {
                self.bus.error("E444: Cannot close last window");
            }
            Ok(new_focus) => {
                self.windows[focused] = None;
                self.set_focused_window(new_focus);
                self.sync_viewport_to_editor();
                self.bus.info("window closed");
            }
        }
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

    /// Equalize all splits to 0.5 ratio.
    pub fn equalize_layout(&mut self) {
        self.layout_mut().equalize_all();
    }

    /// Resize the split whose `last_rect` encompasses `split_origin` and
    /// `split_total` so the boundary sits at `split_pos` cells from the
    /// split origin. `split_pos` is clamped to leave at least
    /// `SPLIT_MIN_SIZE_COLS` / `SPLIT_MIN_SIZE_ROWS` on each side.
    ///
    /// Called by the border-drag handler in the event loop (Phase 9).
    /// `orientation` determines whether we're moving a column (VSplit) or
    /// a row (HSplit) boundary.
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
                a,
                b,
                last_rect,
            } = node
            {
                if *my_dir == dir
                    && let Some(r) = last_rect
                {
                    let (rect_origin, rect_total) = match dir {
                        SplitDir::Vertical => (r.x, r.w),
                        SplitDir::Horizontal => (r.y, r.h),
                        _ => return,
                    };
                    if rect_origin == origin && rect_total == total {
                        *ratio = new_ratio;
                        return; // found the target; done
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
