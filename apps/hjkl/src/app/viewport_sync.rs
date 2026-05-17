//! Viewport synchronisation helpers — copy scroll state between `App` and the focused editor.

use hjkl_engine::Host;

use super::App;

impl App {
    /// Copy the focused window's stored scroll position and cursor into the
    /// active editor's host viewport. Call BEFORE input dispatch so the
    /// engine's scroll math starts from the right offset.
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
        self.active_mut().editor.jump_cursor(cursor_row, cursor_col);
    }

    /// Copy the active editor's host viewport scroll state and cursor back
    /// into the focused window. Call AFTER input dispatch so the engine's
    /// auto-scroll and cursor updates are persisted.
    pub fn sync_viewport_from_editor(&mut self) {
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
}
