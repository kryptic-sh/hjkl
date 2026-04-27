use std::path::PathBuf;

use hjkl_engine::Host;

use super::App;

impl App {
    /// Open the fuzzy file picker.
    pub(crate) fn open_picker(&mut self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let source = Box::new(crate::picker::FileSource::new(cwd));
        self.picker = Some(crate::picker::Picker::new(source));
        self.pending_leader = false;
    }

    /// Open the buffer picker over the currently open slots.
    pub(crate) fn open_buffer_picker(&mut self) {
        let source = Box::new(crate::picker::BufferSource::new(
            &self.slots,
            |s| {
                s.filename
                    .as_ref()
                    .and_then(|p| p.to_str())
                    .unwrap_or("[No Name]")
                    .to_owned()
            },
            |s| s.dirty,
        ));
        self.picker = Some(crate::picker::Picker::new(source));
        self.pending_leader = false;
    }

    /// Open the ripgrep content-search picker, optionally prepopulating
    /// the query with `pattern`.
    pub(crate) fn open_grep_picker(&mut self, pattern: Option<&str>) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let source = Box::new(crate::picker::RgSource::new(cwd));
        self.picker = Some(match pattern {
            Some(p) if !p.is_empty() => crate::picker::Picker::new_with_query(source, p),
            _ => crate::picker::Picker::new(source),
        });
        self.pending_leader = false;
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
            crate::picker::PickerAction::SwitchBuffer(idx) => {
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
