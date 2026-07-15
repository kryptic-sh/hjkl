//! Viewport synchronisation helpers — per-window fold swap + blame-box nudge.
//!
//! ## #151 design (per-window Editor)
//!
//! Cursor and scroll are NOT mirrored anywhere: each window owns its own
//! `Editor` (a `View::new_view` of the slot's shared `Buffer`), and that
//! window editor's `View::cursor()` + `host().viewport()` are the *single
//! source of truth*. `layout::Window` is geometry-only (slot + last_rect).
//!
//! What remains here is the residual per-window state that does NOT live on the
//! window editor:
//!  - **Folds** — held in `App.window_folds` (a parallel store keyed by window).
//!    `sync_viewport_to_editor` installs the focused window's folds into the
//!    shared buffer before a dispatch; `sync_viewport_from_editor` snapshots
//!    them back after.
//!  - **Blame box** — `adjust_blame_box_viewport` nudges the focused window
//!    editor's scroll so the cursor stays visible past the virtual border rows.
//!
//! `sync_viewport_to_editor` runs on focus change (via `switch_focus`);
//! `sync_viewport_from_editor` runs after dispatch.

use hjkl_engine::Host;

use super::App;

impl App {
    /// Install the focused window's fold snapshot into the shared buffer so
    /// motions/render/`z`-ops operate on the focused window's folds
    /// (window-level folds). If the window has no snapshot yet (freshly created,
    /// never focused), adopt whatever the buffer currently holds.
    ///
    /// (#151 Phase D, B' step 2) — cursor/scroll are no longer mirrored here;
    /// the per-window editor is the single source of truth for those. This now
    /// only carries the per-window fold swap.
    pub fn sync_viewport_to_editor(&mut self) {
        let fw = self.focused_window();
        match self.window_folds.get(&fw) {
            Some(folds) => {
                let folds = folds.clone();
                self.active_editor_mut().buffer_mut().set_folds(&folds);
            }
            None => {
                let folds = self.active_editor().buffer().folds();
                self.window_folds.insert(fw, folds);
            }
        }
    }

    /// Snapshot the focused window's fold state into `window_folds` (captures
    /// `z`-command toggles + auto-folds), and nudge the blame-box viewport.
    /// Call AFTER input dispatch.
    ///
    /// (#151 Phase D, B' step 2) — cursor/scroll are no longer mirrored back to
    /// the `layout::Window`; the per-window editor owns them.
    pub fn sync_viewport_from_editor(&mut self) {
        // Boxed-blame view inserts virtual border rows that consume screen
        // height the engine doesn't know about; nudge the scroll so the cursor
        // stays visible. Render-level only — the engine still owns cursor row/col.
        self.adjust_blame_box_viewport();
        let folds = self.active_editor().buffer().folds();
        let fw = self.focused_window();
        self.window_folds.insert(fw, folds);
    }

    /// Keep the cursor visible in the boxed-blame view by nudging `top_row` to
    /// compensate for the virtual box-border rows that eat screen height. The
    /// engine scrolls in document rows assuming a 1:1 screen mapping; the box
    /// borders break that, so the engine can leave the cursor's *rendered* row
    /// off-screen. We rebuild the render plan and step `top_row` until the
    /// cursor's plan row sits within scrolloff of the top/bottom. No-op unless
    /// the blame box is active. Bounded so it always terminates.
    pub(crate) fn adjust_blame_box_viewport(&mut self) {
        // Per-window state (is_blame/viewport/cursor/settings) reads from the
        // focused window editor — the single source of truth (#151). Blame data
        // is per-slot Document metadata, read via `self.active()` in the loop.
        let ed = self.active_editor();
        let box_mode =
            ed.is_blame() && matches!(ed.host().viewport().wrap, hjkl_buffer::Wrap::None);
        if !box_mode {
            return;
        }
        let height = ed.host().viewport().height as usize;
        if height < 3 {
            return;
        }
        let cursor_row = ed.buffer().cursor().row;
        let line_count = ed.buffer().row_count();
        let scrolloff = ed.settings().scrolloff.min(height.saturating_sub(1) / 2);

        for _ in 0..(height * 2 + 4) {
            // Rebuild the plan at the current top and locate the cursor's row.
            let (top, idx) = {
                let slot = self.active();
                let ed = self.active_editor();
                let top = ed.host().viewport().top_row;
                let buf = ed.buffer();
                let plan = crate::app::git_hunks::build_blame_box_plan(
                    &slot.blame,
                    line_count,
                    |r| buf.is_row_hidden(r),
                    top,
                    height,
                    0, // title text is irrelevant to border placement
                );
                let idx = plan.iter().position(|r| {
                    matches!(r, hjkl_buffer_tui::render::BlameRow::Buffer(d) if *d == cursor_row)
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
            self.active_editor_mut().host_mut().viewport_mut().top_row = new_top;
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
        // Each window owns its editor (#151 Phase D); ensure the target's view
        // editor exists.
        self.reconcile_window_editors();
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
        self.reconcile_window_editors();
        self.sync_viewport_to_editor();
    }
}
