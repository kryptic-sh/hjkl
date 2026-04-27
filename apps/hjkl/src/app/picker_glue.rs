use std::path::PathBuf;

use super::{AnyPicker, App};

impl App {
    /// Open the fuzzy file picker.
    pub(crate) fn open_picker(&mut self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let source = crate::picker::FileSource::new(cwd);
        self.picker = Some(AnyPicker::File(crate::picker::FilePicker::new(source)));
        self.pending_leader = false;
    }

    /// Open the buffer picker over the currently open slots.
    pub(crate) fn open_buffer_picker(&mut self) {
        let source = crate::picker::BufferSource::new(
            &self.slots,
            |s| {
                s.filename
                    .as_ref()
                    .and_then(|p| p.to_str())
                    .unwrap_or("[No Name]")
                    .to_owned()
            },
            |s| s.dirty,
        );
        self.picker = Some(AnyPicker::Buffer(crate::picker::BufferPicker::new(source)));
        self.pending_leader = false;
    }

    pub(crate) fn handle_picker_key(&mut self, key: crossterm::event::KeyEvent) {
        let event = match self.picker.as_mut() {
            Some(AnyPicker::File(p)) => p.handle_key(key),
            Some(AnyPicker::Buffer(p)) => p.handle_key(key),
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

    fn dispatch_picker_action(&mut self, action: crate::picker::PickerAction) {
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
        }
    }
}
