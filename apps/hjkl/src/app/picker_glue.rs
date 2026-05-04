use std::path::PathBuf;

use hjkl_engine::Host;

use super::App;

/// Window radius (in lines) around the cursor when snapshotting a buffer
/// for the picker preview. Bounds the per-frame tree-sitter parse cost
/// so huge buffers don't stall the picker.
const BUFFER_PREVIEW_WINDOW_RADIUS: usize = 250;

/// Snapshot a window of `buf` around the cursor as a `String`, returning
/// the content, the cursor row *within that window* (0-based), and the
/// original-buffer row of the first line in the window (`window_start`).
fn snapshot_buffer_window(buf: &hjkl_buffer::Buffer) -> (String, usize, usize) {
    let cursor_row = buf.cursor().row;
    let total = buf.row_count();
    let start = cursor_row.saturating_sub(BUFFER_PREVIEW_WINDOW_RADIUS);
    let end = (cursor_row + BUFFER_PREVIEW_WINDOW_RADIUS).min(total);
    let mut content = String::with_capacity((end - start).saturating_mul(80));
    for r in start..end {
        if let Some(line) = buf.line(r) {
            content.push_str(line);
            content.push('\n');
        }
    }
    (content, cursor_row - start, start)
}

impl App {
    /// Open the fuzzy file picker.
    pub(crate) fn open_picker(&mut self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let theme =
            self.theme.syntax.clone() as std::sync::Arc<dyn hjkl_bonsai::Theme + Send + Sync>;
        let source = Box::new(crate::picker::HighlightedFileSource::new(
            cwd,
            theme,
            self.directory.clone(),
        ));
        self.picker = Some(crate::picker::Picker::new(source));
        self.pending_leader = false;
    }

    /// Open the buffer picker over the currently open slots.
    pub(crate) fn open_buffer_picker(&mut self) {
        let inner = crate::picker::BufferSource::new(
            &self.slots,
            |s| {
                s.filename
                    .as_ref()
                    .and_then(|p| p.to_str())
                    .unwrap_or("[No Name]")
                    .to_owned()
            },
            |s| s.dirty,
            |s| snapshot_buffer_window(s.editor.buffer()).0,
            |s| s.filename.clone(),
            |s| snapshot_buffer_window(s.editor.buffer()).1,
            |s| snapshot_buffer_window(s.editor.buffer()).2,
        );
        let theme =
            self.theme.syntax.clone() as std::sync::Arc<dyn hjkl_bonsai::Theme + Send + Sync>;
        let source = Box::new(crate::picker::HighlightedBufferSource::new(
            inner,
            theme,
            self.directory.clone(),
        ));
        self.picker = Some(crate::picker::Picker::new(source));
        self.pending_leader = false;
    }

    /// Open the ripgrep content-search picker, optionally prepopulating
    /// the query with `pattern`.
    pub(crate) fn open_grep_picker(&mut self, pattern: Option<&str>) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let theme =
            self.theme.syntax.clone() as std::sync::Arc<dyn hjkl_bonsai::Theme + Send + Sync>;
        let source = Box::new(crate::picker::HighlightedRgSource::new(
            cwd,
            theme,
            self.directory.clone(),
        ));
        self.picker = Some(match pattern {
            Some(p) if !p.is_empty() => crate::picker::Picker::new_with_query(source, p),
            _ => crate::picker::Picker::new(source),
        });
        self.pending_leader = false;
    }

    /// Open the git-status fuzzy picker.
    pub(crate) fn open_git_status_picker(&mut self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let theme =
            self.theme.syntax.clone() as std::sync::Arc<dyn hjkl_bonsai::Theme + Send + Sync>;
        let source = Box::new(crate::picker_git::GitStatusPicker::new(
            cwd,
            theme,
            self.directory.clone(),
        ));
        self.picker = Some(crate::picker::Picker::new(source));
        self.pending_leader = false;
        self.pending_git = false;
    }

    pub(crate) fn handle_picker_key(&mut self, key: crossterm::event::KeyEvent) {
        let event = match self.picker.as_mut() {
            Some(p) => p.handle_key(key),
            None => return,
        };
        match event {
            crate::picker::PickerEvent::None => {}
            crate::picker::PickerEvent::Cancel => {
                self.picker = None;
            }
            crate::picker::PickerEvent::Select(action) => {
                self.picker = None;
                self.dispatch_picker_action(action);
            }
        }
    }

    pub(crate) fn dispatch_picker_action(&mut self, action: crate::picker::PickerAction) {
        match action {
            crate::picker::PickerAction::OpenPath(path) => {
                let s = path.to_string_lossy().to_string();
                self.do_edit(&s, false);
            }
            crate::picker::PickerAction::SwitchSlot(idx) => {
                if idx < self.slots.len() {
                    self.switch_to(idx);
                }
            }
            crate::picker::PickerAction::OpenPathAtLine(path, line) => {
                let s = path.to_string_lossy().to_string();
                self.do_edit(&s, false);
                // goto_line is 1-based and clamps to buffer length.
                if line > 0 {
                    self.active_mut().editor.goto_line(line as usize);
                    // Reset viewport top so the line is visible.
                    let vp = self.active_mut().editor.host_mut().viewport_mut();
                    let top = (line as usize).saturating_sub(5);
                    vp.top_row = top;
                }
            }
            crate::picker::PickerAction::None => {}
        }
    }
}
