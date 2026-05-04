use std::path::PathBuf;
use std::time::{Duration, Instant};

use hjkl_buffer::Buffer;
use hjkl_engine::{BufferEdit, Editor, Host, Options};

use super::{App, BufferSlot, STATUS_LINE_HEIGHT};
use crate::host::TuiHost;
use crate::syntax::BufferId;

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

    /// Open the git-log commit picker.
    pub(crate) fn open_git_log_picker(&mut self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let theme =
            self.theme.syntax.clone() as std::sync::Arc<dyn hjkl_bonsai::Theme + Send + Sync>;
        let source = Box::new(crate::picker_git::GitLogPicker::new(
            cwd,
            theme,
            self.directory.clone(),
        ));
        self.picker = Some(crate::picker::Picker::new(source));
        self.pending_leader = false;
        self.pending_git = false;
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
            crate::picker::PickerAction::ShowCommit(sha) => self.do_show_commit(&sha),
            crate::picker::PickerAction::None => {}
        }
    }

    pub(crate) fn do_show_commit(&mut self, sha: &str) {
        let repo = match git2::Repository::discover(
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        ) {
            Ok(r) => r,
            Err(_) => {
                self.status_message = Some("git: not in a repo".into());
                return;
            }
        };
        let oid = match git2::Oid::from_str(sha) {
            Ok(o) => o,
            Err(e) => {
                self.status_message = Some(format!("git: bad sha: {e}"));
                return;
            }
        };
        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(e) => {
                self.status_message = Some(format!("git: {e}"));
                return;
            }
        };
        let content = crate::picker_git::render_commit(&repo, &commit);
        let short_sha = &sha[..7.min(sha.len())];
        match build_scratch_slot(
            &mut self.syntax,
            self.next_buffer_id,
            &content,
            &self.config,
        ) {
            Ok(slot) => {
                self.next_buffer_id += 1;
                self.slots.push(slot);
                let new_idx = self.slots.len() - 1;
                self.switch_to(new_idx);
                self.status_message = Some(format!("showing commit {short_sha}"));
            }
            Err(e) => {
                self.status_message = Some(e);
            }
        }
    }
}

/// Build a scratch [`BufferSlot`] pre-loaded with `content`. Mirrors
/// `build_slot`'s file-read path but injects content directly instead of
/// reading from disk, avoiding a file round-trip for ephemeral commit views.
fn build_scratch_slot(
    syntax: &mut crate::syntax::SyntaxLayer,
    buffer_id: BufferId,
    content: &str,
    config: &crate::config::Config,
) -> Result<BufferSlot, String> {
    let mut buffer = Buffer::new();
    let content = content.strip_suffix('\n').unwrap_or(content);
    BufferEdit::replace_all(&mut buffer, content);

    let host = TuiHost::new();
    let opts = Options {
        expandtab: config.editor.expandtab,
        tabstop: config.editor.tab_width as u32,
        shiftwidth: config.editor.tab_width as u32,
        softtabstop: config.editor.tab_width as u32,
        readonly: true,
        ..Options::default()
    };
    let mut editor = Editor::new(buffer, host, opts);
    if let Ok(size) = crossterm::terminal::size() {
        let vp = editor.host_mut().viewport_mut();
        vp.width = size.0;
        vp.height = size.1.saturating_sub(STATUS_LINE_HEIGHT);
    }
    let _ = editor.take_content_edits();
    let _ = editor.take_content_reset();

    let (vp_top, vp_height) = {
        let vp = editor.host().viewport();
        (vp.top_row, vp.height as usize)
    };
    if let Some(out) = syntax.preview_render(buffer_id, editor.buffer(), vp_top, vp_height) {
        editor.install_ratatui_syntax_spans(out.spans);
    }
    let initial_dg = editor.buffer().dirty_gen();
    let (key, signs) = if let Some(out) = syntax.wait_for_initial_result(Duration::from_millis(150))
    {
        let k = out.key;
        editor.install_ratatui_syntax_spans(out.spans);
        (Some(k), out.signs)
    } else {
        (Some((initial_dg, vp_top, vp_height)), Vec::new())
    };

    let mut slot = BufferSlot {
        buffer_id,
        editor,
        filename: None,
        dirty: false,
        is_new_file: false,
        is_untracked: false,
        diag_signs: signs,
        git_signs: Vec::new(),
        last_git_dirty_gen: None,
        last_git_refresh_at: Instant::now(),
        last_recompute_at: Instant::now() - Duration::from_secs(1),
        last_recompute_key: key,
        saved_hash: 0,
        saved_len: 0,
    };
    slot.snapshot_saved();
    Ok(slot)
}
