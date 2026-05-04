use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use git2::{Commit, DiffFormat, DiffOptions, Repository, Sort, Status, StatusOptions};
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

fn diff_spans(content: &str) -> PreviewSpans {
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

/// Build the header + diff text for a commit. Shared by the picker preview
/// and `do_show_commit` so both display identical content.
pub fn render_commit(repo: &Repository, commit: &Commit) -> String {
    let id = commit.id().to_string();
    let author = commit.author();
    let name = author.name().unwrap_or("");
    let email = author.email().unwrap_or("");
    let time = commit.time();
    // git2 time is seconds since epoch; build an RFC2822-ish string manually
    // without pulling in an extra time crate.
    let secs = time.seconds();
    let offset_min = time.offset_minutes();
    let sign = if offset_min >= 0 { '+' } else { '-' };
    let abs_off = offset_min.unsigned_abs();
    let off_h = abs_off / 60;
    let off_m = abs_off % 60;
    // Convert epoch seconds to a calendar date/time string (UTC + offset).
    let adjusted = secs + (offset_min as i64) * 60;
    let date_str = epoch_to_date_str(adjusted, sign, off_h, off_m);

    let subject = commit.summary().unwrap_or("");
    let body = commit.body().unwrap_or("").trim_end();

    let mut header =
        format!("commit {id}\nAuthor: {name} <{email}>\nDate:   {date_str}\n\n    {subject}\n");
    if !body.is_empty() {
        header.push('\n');
        for line in body.lines() {
            header.push_str("    ");
            header.push_str(line);
            header.push('\n');
        }
    }
    header.push('\n');

    let diff_text = if let Ok(parent) = commit.parent(0) {
        let pt = parent.tree().ok();
        let ct = commit.tree().ok();
        match repo.diff_tree_to_tree(pt.as_ref(), ct.as_ref(), None) {
            Ok(d) => collect_diff(d),
            Err(_) => String::new(),
        }
    } else {
        // Root commit: diff against empty tree.
        let ct = commit.tree().ok();
        match repo.diff_tree_to_tree(None, ct.as_ref(), None) {
            Ok(d) => collect_diff(d),
            Err(_) => String::new(),
        }
    };

    header + &diff_text
}

/// Minimal epoch-to-human-readable conversion without external time crates.
/// Produces a string in the format `Mon DD HH:MM:SS YYYY +HHMM`.
fn epoch_to_date_str(adjusted_secs: i64, sign: char, off_h: u32, off_m: u32) -> String {
    // Days since Unix epoch
    let secs_in_day: i64 = 86400;
    let days = adjusted_secs.div_euclid(secs_in_day);
    let time_of_day = adjusted_secs.rem_euclid(secs_in_day);
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;

    // Compute year/month/day from days (Gregorian calendar)
    let (year, month, day, weekday) = days_to_ymd(days);

    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let weekdays = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let mon_str = months.get((month - 1) as usize).copied().unwrap_or("???");
    let dow_str = weekdays.get(weekday as usize).copied().unwrap_or("???");
    format!("{dow_str} {mon_str} {day:2} {h:02}:{m:02}:{s:02} {year} {sign}{off_h:02}{off_m:02}")
}

fn days_to_ymd(days: i64) -> (i32, u32, u32, u32) {
    // Algorithm: civil date from days (Howard Hinnant's algorithm)
    let z = days + 719468;
    let era: i64 = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    // Day of week: Jan 1, 1970 was a Thursday (4)
    let weekday = (days + 4).rem_euclid(7);
    (year as i32, m as u32, d as u32, weekday as u32)
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
        // line.content() omits the leading +/-/space; line.origin() carries
        // it. For file/hunk headers (origin 'F'/'H') the content already
        // includes the marker, so don't double-prefix those.
        match line.origin() {
            '+' | '-' | ' ' => out.push(line.origin()),
            _ => {}
        }
        out.push_str(&String::from_utf8_lossy(line.content()));
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
            let (content, load_status) = load_preview(&abs);
            let spans = if load_status.is_empty() {
                self.highlight_file(&abs, &content)
            } else {
                PreviewSpans::default()
            };
            return (Buffer::from_str(&content), load_status, spans);
        }

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
        let spans = diff_spans(&diff_text);
        (Buffer::from_str(&diff_text), String::new(), spans)
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

// ── GitLogPicker ──────────────────────────────────────────────────────────

struct GitLogItem {
    sha: String,
    short_sha: String,
    author: String,
    subject: String,
}

pub struct GitLogPicker {
    root: PathBuf,
    items: Arc<Mutex<Vec<GitLogItem>>>,
    scan_done: Arc<AtomicBool>,
    is_sentinel: Arc<AtomicBool>,
    #[allow(dead_code)]
    directory: Arc<LanguageDirectory>,
    #[allow(dead_code)]
    theme: Arc<dyn Theme + Send + Sync>,
}

impl GitLogPicker {
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
        }
    }
}

fn scan_git_log(
    root: PathBuf,
    items: Arc<Mutex<Vec<GitLogItem>>>,
    done: Arc<AtomicBool>,
    sentinel: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
) {
    let repo = match Repository::discover(&root) {
        Ok(r) => r,
        Err(_) => {
            sentinel.store(true, Ordering::Release);
            if let Ok(mut g) = items.lock() {
                g.push(GitLogItem {
                    sha: String::new(),
                    short_sha: String::new(),
                    author: String::new(),
                    subject: String::new(),
                });
            }
            done.store(true, Ordering::Release);
            return;
        }
    };

    let mut revwalk = match repo.revwalk() {
        Ok(r) => r,
        Err(_) => {
            sentinel.store(true, Ordering::Release);
            done.store(true, Ordering::Release);
            return;
        }
    };

    if revwalk.push_head().is_err() || revwalk.set_sorting(Sort::TIME).is_err() {
        sentinel.store(true, Ordering::Release);
        done.store(true, Ordering::Release);
        return;
    }

    let mut batch: Vec<GitLogItem> = Vec::new();
    let mut count = 0usize;

    for oid_result in revwalk {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let oid = match oid_result {
            Ok(o) => o,
            Err(_) => continue,
        };
        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let sha = commit.id().to_string();
        let short_sha = sha[..7.min(sha.len())].to_string();
        let author = commit.author().name().unwrap_or("").to_owned();
        let subject = commit.summary().unwrap_or("").to_owned();
        batch.push(GitLogItem {
            sha,
            short_sha,
            author,
            subject,
        });
        count += 1;
        if count >= 1000 {
            break;
        }
        // Poll cancel every 32 commits.
        if count.is_multiple_of(32) && cancel.load(Ordering::Acquire) {
            break;
        }
    }

    if let Ok(mut g) = items.lock() {
        g.extend(batch);
    }

    done.store(true, Ordering::Release);
}

impl PickerLogic for GitLogPicker {
    fn title(&self) -> &str {
        "git log"
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
                g.get(idx)
                    .map(|item| format!("  {}  {}", item.short_sha, item.subject))
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
                    .map(|item| format!("{} {} {}", item.short_sha, item.author, item.subject))
            })
            .unwrap_or_default()
    }

    fn preview(&self, idx: usize) -> (Buffer, String, PreviewSpans) {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return (Buffer::new(), String::new(), PreviewSpans::default());
        }

        let sha = match self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|i| i.sha.clone()))
        {
            Some(s) => s,
            None => return (Buffer::new(), String::new(), PreviewSpans::default()),
        };

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

        let oid = match git2::Oid::from_str(&sha) {
            Ok(o) => o,
            Err(e) => {
                return (
                    Buffer::new(),
                    format!("git error: {e}"),
                    PreviewSpans::default(),
                );
            }
        };

        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(e) => {
                return (
                    Buffer::new(),
                    format!("git error: {e}"),
                    PreviewSpans::default(),
                );
            }
        };

        let body = render_commit(&repo, &commit);
        let spans = diff_spans(&body);
        (Buffer::from_str(&body), String::new(), spans)
    }

    fn select(&self, idx: usize) -> PickerAction {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return PickerAction::None;
        }
        match self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|i| i.sha.clone()))
        {
            Some(sha) => PickerAction::ShowCommit(sha),
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
            .name("hjkl-picker-git-log".into())
            .spawn(move || scan_git_log(root, items, done, sentinel, cancel))
            .ok()
    }
}
