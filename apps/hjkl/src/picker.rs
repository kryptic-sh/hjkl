//! Modal fuzzy file picker — popup overlay over the editor pane.
//!
//! Opened via `<leader><space>` / `<leader>f`, the `:picker` ex command,
//! or the `+picker` startup arg. Uses [`hjkl_form::TextFieldEditor`] for
//! the query input (so the user gets vim grammar inside the prompt) and
//! a background thread to walk the cwd via the `ignore` crate
//! (gitignore-aware). Selection opens via the App's existing `:e <path>`
//! machinery.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hjkl_buffer::Buffer;
use hjkl_form::{Input as EngineInput, Key as EngineKey, TextFieldEditor};

/// Cap preview reads at this many lines so giant files don't stall the
/// render path.
const PREVIEW_MAX_LINES: usize = 200;
/// Skip preview entirely past this byte count — likely a binary or
/// large generated artefact that wouldn't render usefully anyway.
const PREVIEW_MAX_BYTES: u64 = 1_000_000;

/// Outcome of routing one key event into the picker.
pub enum PickerEvent {
    /// Key consumed; picker stays open.
    None,
    /// User dismissed the picker.
    Cancel,
    /// User picked a path. Caller should open it.
    Select(PathBuf),
}

/// Active picker state. Lives in `App::picker` while open.
pub struct FilePicker {
    /// Query input — vim modal text field. Lands in Insert at open so
    /// the user types immediately.
    pub query: TextFieldEditor,
    /// Discovered files (scan worker appends; main reads). Stored
    /// relative to `root` for shorter display.
    candidates: Arc<Mutex<Vec<PathBuf>>>,
    /// Indices into `candidates` ranked by score for the current query.
    /// Capped to a render-friendly size so the list build is bounded.
    filtered: Vec<usize>,
    /// Selection index into `filtered`.
    pub selected: usize,
    /// Set to `true` by the scan worker when the walk finishes.
    scan_done: Arc<AtomicBool>,
    /// Last query string the filter ran against. Used to skip refilter
    /// when nothing changed.
    last_query: String,
    /// Last `candidates.len()` the filter ran against. Used together
    /// with `last_query` to pick up streaming scan results.
    last_seen_count: usize,
    /// Background scan thread — joined on drop is implicit (detached).
    /// Held for liveness only; reads happen via `candidates`.
    _scan: Option<JoinHandle<()>>,
    /// Picker root (cwd at open). Preview reads resolve relative paths
    /// against this so the right pane works no matter what the editor's
    /// cwd has since become.
    root: PathBuf,
    /// Path the preview buffer currently holds (relative form, matching
    /// `candidates`). `None` when nothing is loaded.
    preview_path: Option<PathBuf>,
    /// Cached preview content. Empty buffer when the file couldn't be
    /// loaded (binary, too big, I/O error). The renderer reads this
    /// directly via `BufferView`.
    preview_buffer: Buffer,
    /// Status tag for the preview pane title — populated when a file
    /// is skipped (binary / oversize / I/O error). Empty when the
    /// preview is the file's actual content.
    preview_status: String,
}

impl FilePicker {
    /// Open a picker rooted at `cwd`. Spawns the scan worker
    /// immediately so candidates start streaming in before the user
    /// types their first character.
    pub fn open(cwd: &Path) -> Self {
        let candidates = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
        let scan_done = Arc::new(AtomicBool::new(false));

        let handle = {
            let cands = Arc::clone(&candidates);
            let done = Arc::clone(&scan_done);
            let cwd_owned = cwd.to_path_buf();
            thread::Builder::new()
                .name("hjkl-picker-scan".into())
                .spawn(move || scan_walk(cwd_owned.as_path(), cands, done))
                .ok()
        };

        let mut query = TextFieldEditor::new(true);
        query.enter_insert_at_end();

        Self {
            query,
            candidates,
            filtered: Vec::new(),
            selected: 0,
            scan_done,
            last_query: String::new(),
            last_seen_count: 0,
            _scan: handle,
            root: cwd.to_path_buf(),
            preview_path: None,
            preview_buffer: Buffer::new(),
            preview_status: String::new(),
        }
    }

    /// Borrow the cached preview buffer for the renderer's right pane.
    pub fn preview_buffer(&self) -> &Buffer {
        &self.preview_buffer
    }

    /// Status tag for the preview pane title (e.g. "binary",
    /// "1.0MB — too large"). Empty when the preview is regular
    /// content.
    pub fn preview_status(&self) -> &str {
        &self.preview_status
    }

    /// Path currently loaded into the preview, in the same relative
    /// form as `candidates`. `None` when no path is selected.
    pub fn preview_path(&self) -> Option<&Path> {
        self.preview_path.as_deref()
    }

    /// Refresh the preview buffer if the selection now points at a
    /// different path than the cached one. Called from the renderer.
    /// Cheap when the selection hasn't moved (path equality check
    /// only).
    pub fn refresh_preview(&mut self) {
        let target = self.selected_path();
        if target.as_deref() == self.preview_path.as_deref() {
            return;
        }
        self.preview_path = target.clone();
        let Some(rel) = target else {
            self.preview_buffer = Buffer::new();
            self.preview_status.clear();
            return;
        };
        let abs = self.root.join(&rel);
        let (content, status) = load_preview(&abs);
        self.preview_buffer = Buffer::from_str(&content);
        self.preview_status = status;
    }

    /// True once the background walk has finished. Used by the renderer
    /// to show "scanning…" while results are still streaming in.
    pub fn scan_done(&self) -> bool {
        self.scan_done.load(Ordering::Acquire)
    }

    /// Total candidate count (regardless of filter).
    pub fn total(&self) -> usize {
        self.candidates.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Number of candidates currently passing the query filter.
    pub fn matched(&self) -> usize {
        self.filtered.len()
    }

    /// Re-run the filter if the query or candidate count changed.
    /// Returns `true` when `filtered` was rebuilt — the renderer can
    /// use this to decide whether to redraw.
    pub fn refresh(&mut self) -> bool {
        let cands = match self.candidates.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let q = self.query.text();
        let q_changed = q != self.last_query;
        let count_changed = cands.len() != self.last_seen_count;
        if !q_changed && !count_changed {
            return false;
        }
        self.last_query.clone_from(&q);
        self.last_seen_count = cands.len();

        let q_lower = q.to_lowercase();
        let mut scored: Vec<(i64, usize)> = Vec::new();
        for (i, p) in cands.iter().enumerate() {
            let s = p.to_string_lossy();
            let s_lower = s.to_lowercase();
            let sc = if q.is_empty() {
                // No query → keep insertion order (path-sort below).
                0
            } else {
                match score(&s_lower, &q_lower) {
                    Some(v) => v,
                    None => continue,
                }
            };
            scored.push((sc, i));
        }
        // Score desc, ties by path asc.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| cands[a.1].cmp(&cands[b.1])));
        scored.truncate(500);
        self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
        true
    }

    /// Path at the current selection (if any).
    pub fn selected_path(&self) -> Option<PathBuf> {
        let idx = *self.filtered.get(self.selected)?;
        let cands = self.candidates.lock().ok()?;
        cands.get(idx).cloned()
    }

    /// First `n` filtered paths — for renderer's visible slice.
    pub fn visible(&self, n: usize) -> Vec<PathBuf> {
        let cands = match self.candidates.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        self.filtered
            .iter()
            .take(n)
            .filter_map(|&i| cands.get(i).cloned())
            .collect()
    }

    /// Route a key event. Special keys (Esc / Enter / C-n / C-p / Up /
    /// Down) drive picker navigation; everything else forwards to the
    /// query field's vim FSM.
    pub fn handle_key(&mut self, key: KeyEvent) -> PickerEvent {
        // Cancel.
        if key.code == KeyCode::Esc {
            // Insert + non-empty Esc drops to Normal mode in the field.
            // For pickers we just close on Esc — typing in the prompt
            // is what the user is here for, not vim motions on it.
            return PickerEvent::Cancel;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return PickerEvent::Cancel;
        }

        // Select.
        if key.code == KeyCode::Enter {
            return match self.selected_path() {
                Some(p) => PickerEvent::Select(p),
                None => PickerEvent::None,
            };
        }

        // Navigation.
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Down => {
                self.move_selection(1);
                return PickerEvent::None;
            }
            KeyCode::Up => {
                self.move_selection(-1);
                return PickerEvent::None;
            }
            KeyCode::Char('n') if ctrl => {
                self.move_selection(1);
                return PickerEvent::None;
            }
            KeyCode::Char('p') if ctrl => {
                self.move_selection(-1);
                return PickerEvent::None;
            }
            _ => {}
        }

        // Forward to the query field.
        let input: EngineInput = key.into();
        // Single-line: drop a stray Enter (already handled above) and
        // any Esc-derived noise (also handled above).
        if input.key == EngineKey::Enter || input.key == EngineKey::Esc {
            return PickerEvent::None;
        }
        self.query.handle_input(input);
        PickerEvent::None
    }

    fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            self.selected = 0;
            return;
        }
        let len = self.filtered.len() as i32;
        let next = self.selected as i32 + delta;
        let wrapped = next.rem_euclid(len);
        self.selected = wrapped as usize;
    }
}

/// Load a single file for the preview pane. Returns `(content,
/// status)`: when `status` is non-empty the file was skipped (binary /
/// oversized / I/O error) and `content` is empty.
fn load_preview(abs: &Path) -> (String, String) {
    let meta = match std::fs::metadata(abs) {
        Ok(m) => m,
        Err(e) => return (String::new(), format!("{e}")),
    };
    if meta.len() > PREVIEW_MAX_BYTES {
        let mb = meta.len() as f64 / 1_048_576.0;
        return (String::new(), format!("{mb:.1}MB — too large"));
    }
    let bytes = match std::fs::read(abs) {
        Ok(b) => b,
        Err(e) => return (String::new(), format!("{e}")),
    };
    // Binary heuristic: presence of a NUL byte in the first 8KB.
    let scan_end = bytes.len().min(8192);
    if bytes[..scan_end].contains(&0u8) {
        return (String::new(), "binary".into());
    }
    let text = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => return (String::new(), "non-utf8".into()),
    };
    let truncated: String = text
        .lines()
        .take(PREVIEW_MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    (truncated, String::new())
}

/// Background walker — streams `is_file()` entries into `out`, gitignore-
/// aware via `ignore::WalkBuilder`. Sets `done` on completion so the
/// picker can stop showing "scanning…".
fn scan_walk(root: &Path, out: Arc<Mutex<Vec<PathBuf>>>, done: Arc<AtomicBool>) {
    let walk = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .parents(true)
        .build();
    let mut batch: Vec<PathBuf> = Vec::with_capacity(256);
    let mut total = 0usize;
    const HARD_CAP: usize = 50_000;
    for entry in walk {
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
            && let Ok(mut g) = out.lock()
        {
            g.append(&mut batch);
        }
        if total >= HARD_CAP {
            break;
        }
    }
    if let Ok(mut g) = out.lock() {
        g.append(&mut batch);
    }
    done.store(true, Ordering::Release);
}

/// Subsequence-based fuzzy score. Returns `None` when not all needle
/// characters appear (in order) in the haystack.
///
/// Bonuses:
/// - `+8` per match at a word boundary (start, after `/`, `_`, `-`,
///   `.`, ` `).
/// - `+5` per consecutive match (run of adjacent matches).
/// - `+1` base hit per matched char.
///
/// Penalty: `-len(haystack)/8` so shorter overall paths win on ties.
fn score(haystack: &str, needle: &str) -> Option<i64> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    let mut hi = 0usize;
    let mut ni = 0usize;
    let mut total: i64 = 0;
    let mut prev_match = false;
    while ni < n.len() && hi < h.len() {
        if h[hi] == n[ni] {
            if prev_match {
                total += 5;
            }
            let at_boundary = hi == 0 || matches!(h[hi - 1], b'/' | b'_' | b'-' | b'.' | b' ');
            if at_boundary {
                total += 8;
            }
            total += 1;
            prev_match = true;
            ni += 1;
        } else {
            prev_match = false;
        }
        hi += 1;
    }
    if ni < n.len() {
        return None;
    }
    total -= h.len() as i64 / 8;
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_subsequence_match() {
        assert!(score("src/main.rs", "main").is_some());
        assert!(score("src/main.rs", "smr").is_some());
        assert!(score("src/main.rs", "xyz").is_none());
    }

    #[test]
    fn score_word_boundary_beats_mid_word() {
        // `main` → matches at the boundary in "src/main.rs",
        // outscoring a mid-word run in "src/domain.rs".
        let a = score("src/main.rs", "main").unwrap();
        let b = score("src/domain.rs", "main").unwrap();
        assert!(a > b, "boundary {a} should beat mid-word {b}");
    }

    #[test]
    fn score_shorter_wins_on_ties() {
        let a = score("a/b/foo.rs", "foo").unwrap();
        let b = score("a/b/c/d/e/foo.rs", "foo").unwrap();
        assert!(a > b);
    }
}
