use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use hjkl_buffer::Buffer;

use crate::logic::{PickerAction, PickerLogic, RequeryMode};
use crate::preview::{PreviewSpans, load_preview};

/// File-source: gitignore-aware cwd walker. Items are paths relative to
/// `root`, preview reads from disk capped at `PREVIEW_MAX_BYTES` with a
/// binary-byte heuristic.
///
/// This base source does not perform syntax highlighting — the preview
/// returns `PreviewSpans::default()`. Wrap in `HighlightedFileSource`
/// (in the app crate) to add tree-sitter highlighting.
pub struct FileSource {
    pub root: PathBuf,
    pub items: Arc<Mutex<Vec<PathBuf>>>,
    scan_done: Arc<AtomicBool>,
}

impl FileSource {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            items: Arc::new(Mutex::new(Vec::new())),
            scan_done: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl PickerLogic for FileSource {
    fn title(&self) -> &str {
        "files"
    }

    fn item_count(&self) -> usize {
        self.items.lock().map(|g| g.len()).unwrap_or(0)
    }

    fn label(&self, idx: usize) -> String {
        self.items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|p| p.to_string_lossy().into_owned()))
            .unwrap_or_default()
    }

    fn match_text(&self, idx: usize) -> String {
        self.label(idx)
    }

    fn preview(&self, idx: usize) -> (Buffer, String, PreviewSpans) {
        let path = match self.items.lock().ok().and_then(|g| g.get(idx).cloned()) {
            Some(p) => p,
            None => return (Buffer::new(), String::new(), PreviewSpans::default()),
        };
        let abs = self.root.join(&path);
        let (content, status) = load_preview(&abs);
        (Buffer::from_str(&content), status, PreviewSpans::default())
    }

    fn select(&self, idx: usize) -> PickerAction {
        match self.items.lock().ok().and_then(|g| g.get(idx).cloned()) {
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
        let root = self.root.clone();
        // Reset for re-enumerate.
        if let Ok(mut g) = items.lock() {
            g.clear();
        }
        done.store(false, Ordering::Release);
        thread::Builder::new()
            .name("hjkl-picker-scan".into())
            .spawn(move || scan_walk(&root, &items, &done, &cancel))
            .ok()
    }
}

/// Background walker — streams `is_file()` entries into `items`,
/// gitignore-aware via `ignore::WalkBuilder`.
fn scan_walk(
    root: &Path,
    items: &Arc<Mutex<Vec<PathBuf>>>,
    done: &Arc<AtomicBool>,
    cancel: &Arc<AtomicBool>,
) {
    let walk = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .parents(true)
        .build();
    let mut batch: Vec<PathBuf> = Vec::with_capacity(256);
    let mut total = 0usize;
    const HARD_CAP: usize = 50_000;
    for entry in walk {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_file() {
            continue;
        }
        let path = entry.into_path();
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_path_buf())
            .unwrap_or(path);
        batch.push(rel);
        total += 1;
        if batch.len() >= 256
            && let Ok(mut g) = items.lock()
        {
            g.extend(batch.drain(..));
        }
        if total >= HARD_CAP {
            break;
        }
    }
    if let Ok(mut g) = items.lock() {
        g.extend(batch.drain(..));
    }
    done.store(true, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_preview_has_no_highlight_spans() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("notes.txt");
        std::fs::write(&path, "hello world\nthis is plain text\n").unwrap();

        let mut source = FileSource::new(tmp.path().to_path_buf());
        let cancel = Arc::new(AtomicBool::new(false));
        let _handle = source.enumerate(None, Arc::clone(&cancel));
        // Wait for the scan thread to populate items.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if source.item_count() > 0 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let count = source.item_count();
        let mut found_idx = None;
        for i in 0..count {
            if source.label(i).contains("notes.txt") {
                found_idx = Some(i);
                break;
            }
        }
        let idx = found_idx.expect("notes.txt should appear in FileSource");
        let (_buf, status, spans) = source.preview(idx);
        assert!(status.is_empty(), "unexpected status: {status:?}");
        assert!(spans.styles.is_empty(), "got {} styles", spans.styles.len());
        for row in &spans.by_row {
            assert!(row.is_empty(), "unexpected spans on row: {row:?}");
        }
    }
}
