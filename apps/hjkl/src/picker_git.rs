use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use git2::{DiffFormat, DiffOptions, Repository, Status, StatusOptions};
use hjkl_bonsai::{CommentMarkerPass, Highlighter, Theme};
use hjkl_buffer::Buffer;
use hjkl_picker::{PickerAction, PickerLogic, PreviewSpans, RequeryMode, load_preview};
use ratatui::style::{Color, Style};

use crate::lang::LanguageDirectory;

const SENTINEL_LABEL: &str = "  not a git repo";

struct GitStatusItem {
    status: [u8; 2],
    path: PathBuf,
    is_untracked: bool,
}

pub struct GitStatusPicker {
    root: PathBuf,
    items: Arc<Mutex<Vec<GitStatusItem>>>,
    scan_done: Arc<AtomicBool>,
    is_sentinel: Arc<AtomicBool>,
    directory: Arc<LanguageDirectory>,
    theme: Arc<dyn Theme + Send + Sync>,
    highlighters: Mutex<HashMap<String, Highlighter>>,
}

impl GitStatusPicker {
    pub fn new(
        root: PathBuf,
        theme: Arc<dyn Theme + Send + Sync>,
        directory: Arc<LanguageDirectory>,
    ) -> Self {
        Self {
            root,
            items: Arc::new(Mutex::new(Vec::new())),
            scan_done: Arc::new(AtomicBool::new(false)),
            is_sentinel: Arc::new(AtomicBool::new(false)),
            directory,
            theme,
            highlighters: Mutex::new(HashMap::new()),
        }
    }

    fn diff_spans(&self, content: &str) -> PreviewSpans {
        let bytes = content.as_bytes();
        let mut ranges: Vec<(std::ops::Range<usize>, Style)> = Vec::new();

        let added_style = Style::default().fg(Color::Green);
        let removed_style = Style::default().fg(Color::Red);
        let hunk_style = Style::default().fg(Color::Cyan);
        let header_style = Style::default().fg(Color::Blue);

        let mut pos = 0usize;
        for line in content.lines() {
            let line_start = pos;
            let line_end = pos + line.len();
            if line.starts_with("+++") || line.starts_with("---") {
                ranges.push((line_start..line_end, header_style));
            } else if line.starts_with("@@") {
                ranges.push((line_start..line_end, hunk_style));
            } else if line.starts_with('+') {
                ranges.push((line_start..line_end, added_style));
            } else if line.starts_with('-') {
                ranges.push((line_start..line_end, removed_style));
            }
            pos = line_end + 1;
            if pos > bytes.len() {
                pos = bytes.len();
            }
        }

        PreviewSpans::from_byte_ranges(&ranges, bytes)
    }

    fn highlight_file(&self, abs: &std::path::Path, content: &str) -> PreviewSpans {
        let bytes = content.as_bytes();
        let grammar = match self.directory.for_path(abs) {
            Some(g) => g,
            None => return PreviewSpans::default(),
        };
        let name = grammar.name().to_string();
        let mut hl_cache = match self.highlighters.lock() {
            Ok(g) => g,
            Err(_) => return PreviewSpans::default(),
        };
        let h = match hl_cache.entry(name) {
            std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => match Highlighter::new(grammar) {
                Ok(h) => v.insert(h),
                Err(_) => return PreviewSpans::default(),
            },
        };
        h.reset();
        h.parse_initial(bytes);
        let mut flat = h.highlight_range(bytes, 0..bytes.len());
        drop(hl_cache);
        CommentMarkerPass::new().apply(&mut flat, bytes);
        let theme = Arc::clone(&self.theme);
        let ranges: Vec<(std::ops::Range<usize>, ratatui::style::Style)> = flat
            .into_iter()
            .filter_map(|span| {
                theme
                    .style(span.capture())
                    .map(|s| (span.byte_range.clone(), s.to_ratatui()))
            })
            .collect();
        PreviewSpans::from_byte_ranges(&ranges, bytes)
    }
}

fn status_flags_to_xy(st: Status) -> [u8; 2] {
    let x = if st.contains(Status::INDEX_RENAMED) {
        b'R'
    } else if st.contains(Status::INDEX_NEW) {
        b'A'
    } else if st.contains(Status::INDEX_MODIFIED) {
        b'M'
    } else if st.contains(Status::INDEX_DELETED) {
        b'D'
    } else if st.contains(Status::INDEX_TYPECHANGE) {
        b'T'
    } else {
        b' '
    };

    let y = if st.contains(Status::WT_NEW) {
        b'?'
    } else if st.contains(Status::WT_RENAMED) {
        b'R'
    } else if st.contains(Status::WT_MODIFIED) {
        b'M'
    } else if st.contains(Status::WT_DELETED) {
        b'D'
    } else if st.contains(Status::WT_TYPECHANGE) {
        b'T'
    } else {
        b' '
    };

    // Untracked: both columns `?`
    if st.contains(Status::WT_NEW) && x == b' ' {
        return [b'?', b'?'];
    }

    [x, y]
}

fn git_diff_for_path(repo: &Repository, root: &std::path::Path, path: &std::path::Path) -> String {
    let path_str = path.to_string_lossy();
    let mut opts = DiffOptions::new();
    opts.pathspec(path_str.as_ref());

    let diff = match repo.head() {
        Ok(head) => {
            let tree = match head.peel_to_tree() {
                Ok(t) => t,
                Err(_) => {
                    return match repo.diff_index_to_workdir(None, Some(&mut opts)) {
                        Ok(d) => collect_diff(d),
                        Err(_) => String::new(),
                    };
                }
            };
            match repo.diff_tree_to_workdir_with_index(Some(&tree), Some(&mut opts)) {
                Ok(d) => d,
                Err(_) => return String::new(),
            }
        }
        Err(_) => match repo.diff_index_to_workdir(None, Some(&mut opts)) {
            Ok(d) => d,
            Err(_) => return String::new(),
        },
    };

    let _ = root; // root is the workdir; path is relative — no join needed for diff
    collect_diff(diff)
}

fn collect_diff(diff: git2::Diff) -> String {
    let mut out = String::new();
    let _ = diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        let content = String::from_utf8_lossy(line.content());
        out.push_str(&content);
        true
    });
    out
}

fn scan_git_status(
    root: PathBuf,
    items: Arc<Mutex<Vec<GitStatusItem>>>,
    done: Arc<AtomicBool>,
    sentinel: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
) {
    let repo = match Repository::discover(&root) {
        Ok(r) => r,
        Err(_) => {
            sentinel.store(true, Ordering::Release);
            if let Ok(mut g) = items.lock() {
                g.push(GitStatusItem {
                    status: [b'?', b'?'],
                    path: PathBuf::new(),
                    is_untracked: true,
                });
            }
            done.store(true, Ordering::Release);
            return;
        }
    };

    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);

    let statuses = match repo.statuses(Some(&mut opts)) {
        Ok(s) => s,
        Err(_) => {
            sentinel.store(true, Ordering::Release);
            if let Ok(mut g) = items.lock() {
                g.push(GitStatusItem {
                    status: [b'?', b'?'],
                    path: PathBuf::new(),
                    is_untracked: true,
                });
            }
            done.store(true, Ordering::Release);
            return;
        }
    };

    if cancel.load(Ordering::Acquire) {
        done.store(true, Ordering::Release);
        return;
    }

    let mut parsed: Vec<GitStatusItem> = Vec::new();
    for entry in statuses.iter() {
        let path_str = match entry.path() {
            Some(p) => p,
            None => continue,
        };
        let path = PathBuf::from(path_str);
        let st = entry.status();
        let xy = status_flags_to_xy(st);
        let is_untracked = xy == [b'?', b'?'];
        parsed.push(GitStatusItem {
            status: xy,
            path,
            is_untracked,
        });
    }

    if !parsed.is_empty()
        && let Ok(mut g) = items.lock()
    {
        g.extend(parsed);
    }

    done.store(true, Ordering::Release);
}

impl PickerLogic for GitStatusPicker {
    fn title(&self) -> &str {
        "git status"
    }

    fn item_count(&self) -> usize {
        self.items.lock().map(|g| g.len()).unwrap_or(0)
    }

    fn label(&self, idx: usize) -> String {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return SENTINEL_LABEL.to_owned();
        }
        self.items
            .lock()
            .ok()
            .and_then(|g| {
                g.get(idx).map(|item| {
                    let xy = format!("{}{}", item.status[0] as char, item.status[1] as char);
                    format!("  {} {}", xy, item.path.to_string_lossy())
                })
            })
            .unwrap_or_default()
    }

    fn match_text(&self, idx: usize) -> String {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return SENTINEL_LABEL.to_owned();
        }
        self.items
            .lock()
            .ok()
            .and_then(|g| {
                g.get(idx)
                    .map(|item| item.path.to_string_lossy().into_owned())
            })
            .unwrap_or_default()
    }

    fn preview(&self, idx: usize) -> (Buffer, String, PreviewSpans) {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return (Buffer::new(), String::new(), PreviewSpans::default());
        }

        let item = match self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|i| (i.path.clone(), i.is_untracked)))
        {
            Some(v) => v,
            None => return (Buffer::new(), String::new(), PreviewSpans::default()),
        };
        let (path, is_untracked) = item;
        let abs = self.root.join(&path);

        if is_untracked {
            let (content, _load_status) = load_preview(&abs);
            let status_line = format!("{} (untracked)", path.to_string_lossy());
            let spans = self.highlight_file(&abs, &content);
            return (Buffer::from_str(&content), status_line, spans);
        }

        let status_line = format!("git diff HEAD -- {}", path.to_string_lossy());

        let repo = match Repository::discover(&self.root) {
            Ok(r) => r,
            Err(e) => {
                return (
                    Buffer::new(),
                    format!("git error: {e}"),
                    PreviewSpans::default(),
                );
            }
        };

        let diff_text = git_diff_for_path(&repo, &self.root, &path);
        let spans = self.diff_spans(&diff_text);
        (Buffer::from_str(&diff_text), status_line, spans)
    }

    fn select(&self, idx: usize) -> PickerAction {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return PickerAction::None;
        }
        match self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|i| i.path.clone()))
        {
            Some(p) => PickerAction::OpenPath(p),
            None => PickerAction::None,
        }
    }

    fn requery_mode(&self) -> RequeryMode {
        RequeryMode::FilterInMemory
    }

    fn enumerate(
        &mut self,
        _query: Option<&str>,
        cancel: Arc<AtomicBool>,
    ) -> Option<JoinHandle<()>> {
        let items = Arc::clone(&self.items);
        let done = Arc::clone(&self.scan_done);
        let sentinel = Arc::clone(&self.is_sentinel);
        let root = self.root.clone();

        if let Ok(mut g) = items.lock() {
            g.clear();
        }
        done.store(false, Ordering::Release);
        sentinel.store(false, Ordering::Release);

        thread::Builder::new()
            .name("hjkl-picker-git-status".into())
            .spawn(move || scan_git_status(root, items, done, sentinel, cancel))
            .ok()
    }
}
