//! Viewport synchronisation helpers — maintain cursor/scroll coherence
//! between per-window snapshots and the slot editor.
//!
//! ## v0.22.0 design
//!
//! Each [`AppWindow`] stores per-window cursor and scroll as plain fields
//! (`cursor_row`, `cursor_col`, `top_row`, `top_col`).  The slot editor is
//! the *active* state while a window is focused and the last `dispatch` path
//! ran through it — its cursor is always up-to-date after a dispatch.
//!
//! **`sync_viewport_from_editor`** — called *after* every dispatch to write
//! the editor's current cursor/scroll back into the focused window's snapshot
//! fields.  Keeps the snapshot current so that:
//!  - Switching to another window can save/restore independently.
//!  - Non-focused rendering of this window sees the correct scroll.
//!
//! **`sync_viewport_to_editor`** — called *only on focus change* (when a
//! different window gains focus).  It restores the newly-focused window's
//! saved cursor/scroll into the slot editor so the next dispatch starts from
//! the right position.  It is NOT called before every keypress — this was
//! the root cause of the sticky-column regression (issue #151) because
//! restoring the cursor also reset `sticky_col` on every frame.
//!
//! Call sites that previously called `sync_viewport_to_editor` before input
//! dispatch have been removed.  The only remaining call is in the focus-change
//! helpers (`focus_below`, `focus_above`, `focus_left`, `focus_right`,
//! `focus_next`, `focus_previous`, `move_window_to_new_tab`,
//! `close_focused_window`) via `switch_focus`.

use hjkl_engine::Host;

use super::App;

impl App {
    /// Copy the focused window's stored scroll position and cursor into the
    /// active editor's host viewport.
    ///
    /// **Call on focus change only** — NOT before every keypress.
    /// Setting the cursor before every keypress used to reset `sticky_col`
    /// (curswant), breaking `j`/`k` column preservation (issue #151).
    pub fn sync_viewport_to_editor(&mut self) {
        let fw = self.focused_window();
        let win = self.windows[fw].as_ref().expect("focused_window open");
        let (top_row, top_col) = (win.top_row, win.top_col);
        let (cursor_row, cursor_col) = (win.cursor_row, win.cursor_col);
        let maybe_rect = win.last_rect;
        if let Some(rect) = maybe_rect {
            let vp = self.active_mut().editor.host_mut().viewport_mut();
            vp.top_row = top_row;
            vp.top_col = top_col;
            vp.width = rect.w;
            vp.height = rect.h;
        }
        self.active_mut()
            .editor
            .set_cursor_quiet(cursor_row, cursor_col);
    }

    /// Copy the active editor's host viewport scroll state and cursor back
    /// into the focused window's snapshot. Call AFTER input dispatch so the
    /// engine's auto-scroll and cursor updates are persisted in the window
    /// snapshot.
    pub fn sync_viewport_from_editor(&mut self) {
        // Boxed-blame view inserts virtual border rows that consume screen
        // height the engine doesn't know about; nudge the scroll so the cursor
        // stays visible. Render-level only — the engine still owns cursor row/col.
        self.adjust_blame_box_viewport();
        let vp = self.active().editor.host().viewport();
        let (top_row, top_col) = (vp.top_row, vp.top_col);
        let (cursor_row, cursor_col) = self.active().editor.cursor();
        let fw = self.focused_window();
        let win = self.windows[fw].as_mut().expect("focused_window open");
        win.top_row = top_row;
        win.top_col = top_col;
        win.cursor_row = cursor_row;
        win.cursor_col = cursor_col;
    }

    /// Keep the cursor visible in the boxed-blame view by nudging `top_row` to
    /// compensate for the virtual box-border rows that eat screen height. The
    /// engine scrolls in document rows assuming a 1:1 screen mapping; the box
    /// borders break that, so the engine can leave the cursor's *rendered* row
    /// off-screen. We rebuild the render plan and step `top_row` until the
    /// cursor's plan row sits within scrolloff of the top/bottom. No-op unless
    /// the blame box is active. Bounded so it always terminates.
    pub(crate) fn adjust_blame_box_viewport(&mut self) {
        let slot = self.active();
        let box_mode = slot.blame_column
            && matches!(slot.editor.host().viewport().wrap, hjkl_buffer::Wrap::None);
        if !box_mode {
            return;
        }
        let height = slot.editor.host().viewport().height as usize;
        if height < 3 {
            return;
        }
        let cursor_row = slot.editor.buffer().cursor().row;
        let line_count = slot.editor.buffer().row_count();
        let scrolloff = slot
            .editor
            .settings()
            .scrolloff
            .min(height.saturating_sub(1) / 2);

        for _ in 0..(height * 2 + 4) {
            // Rebuild the plan at the current top and locate the cursor's row.
            let (top, idx) = {
                let slot = self.active();
                let top = slot.editor.host().viewport().top_row;
                let buf = slot.editor.buffer();
                let plan = crate::app::git_hunks::build_blame_box_plan(
                    &slot.blame,
                    line_count,
                    |r| buf.is_row_hidden(r),
                    top,
                    height,
                    0, // title text is irrelevant to border placement
                );
                let idx = plan.iter().position(|r| {
                    matches!(r, hjkl_buffer_tui::render::BlameRow::Content(d) if *d == cursor_row)
                });
                (top, idx)
            };
            let new_top = match idx {
                // Cursor row isn't in the visible plan — scroll toward it.
                None => {
                    if cursor_row >= top {
                        top + 1
                    } else {
                        top.saturating_sub(1)
                    }
                }
                Some(i) => {
                    if i < scrolloff && top > 0 {
                        top - 1
                    } else if i + 1 + scrolloff > height && top + 1 < line_count {
                        top + 1
                    } else {
                        break;
                    }
                }
            };
            if new_top == top {
                break;
            }
            self.active_mut().editor.host_mut().viewport_mut().top_row = new_top;
        }
    }

    /// Switch focus from the current window to `target_id`, saving the current
    /// window's cursor/scroll into its snapshot and restoring the new window's
    /// snapshot into the editor.
    ///
    /// Use this in all focus-change helpers instead of the old pattern of
    /// `sync_from` + `set_focused` + `sync_to`.
    pub(crate) fn switch_focus(&mut self, target_id: super::window::WindowId) {
        self.sync_viewport_from_editor();
        self.set_focused_window(target_id);
        self.sync_viewport_to_editor();
    }

    /// Switch to `tab_idx`, saving the current window's cursor/scroll and
    /// restoring the new tab's focused window snapshot into the editor.
    ///
    /// Use this for all tab-switch operations instead of the old pattern of
    /// `sync_from` + `active_tab = idx` + `sync_to`.
    pub(crate) fn switch_tab(&mut self, tab_idx: usize) {
        self.sync_viewport_from_editor();
        self.active_tab = tab_idx;
        self.sync_viewport_to_editor();
    }
}
