use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use git2::{
    BranchType, Commit, DiffFormat, DiffOptions, ObjectType, Repository, Sort, Status,
    StatusOptions, Time,
};
use hjkl_bonsai::{CommentMarkerPass, Highlighter, Theme};
use hjkl_buffer::Buffer;
use hjkl_picker::{PickerAction, PickerLogic, PreviewSpans, RequeryMode, load_preview};
use ratatui::style::{Color, Style};

use crate::lang::{GrammarRequest, LanguageDirectory};
use crate::picker_action::AppAction;

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

/// 2-char initials for an author name, lazygit-style. Wide unicode → that
/// single grapheme; single word → first 2 chars; multi-word → first char of
/// word[0] + first char of word[1]. Empty input returns two spaces.
fn author_initials(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return "  ".to_string();
    }
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    let take_first = |w: &str| w.chars().next().unwrap_or(' ').to_uppercase().to_string();
    if words.len() == 1 {
        let w = words[0];
        let mut chars = w.chars();
        let a = chars.next().unwrap_or(' ').to_uppercase().to_string();
        let b = chars
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| " ".to_string());
        return format!("{a}{b}");
    }
    format!("{}{}", take_first(words[0]), take_first(words[1]))
}

/// Deterministic color from author name. FNV-1a hash → HSL → RGB. Matches
/// lazygit's behavior: same name always picks the same color.
fn author_color(name: &str) -> Color {
    let h = fnv1a_64(name.as_bytes());
    let hue = ((h & 0xFFFF) as f32 / 65535.0) * 360.0;
    let sat = 0.6 + (((h >> 16) & 0xFFFF) as f32 / 65535.0) * 0.4;
    let lit = 0.45 + (((h >> 32) & 0xFFFF) as f32 / 65535.0) * 0.15;
    let (r, g, b) = hsl_to_rgb(hue, sat, lit);
    Color::Rgb(r, g, b)
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h_p = h / 60.0;
    let x = c * (1.0 - (h_p.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match h_p as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to_u8 = |v: f32| ((v + m) * 255.0).clamp(0.0, 255.0) as u8;
    (to_u8(r1), to_u8(g1), to_u8(b1))
}

/// Detects a Conventional Commits prefix at `start` (char index) inside
/// `label`. Returns the char index just past the colon if matched.
/// Pattern: `<type>(<scope>)?!?:` where type is alphanumerics + `_`/`-`,
/// scope is anything except `)`/`(`.
fn conv_commit_prefix_end(label: &str, start: usize) -> Option<usize> {
    let mut iter = label.chars().enumerate().skip_while(|&(i, _)| i < start);
    let mut ci = start;
    let mut saw_type = false;
    // Type chars
    for (i, c) in iter.by_ref() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            saw_type = true;
            ci = i + 1;
        } else {
            ci = i;
            break;
        }
    }
    if !saw_type {
        return None;
    }
    // Look at next char: '(' for scope, '!' for breaking, ':' for end.
    let rest: String = label.chars().skip(ci).collect();
    let mut chars = rest.chars();
    let mut consumed = 0usize;
    match chars.next()? {
        '(' => {
            consumed += 1;
            // Scope until ')'
            let mut closed = false;
            for c in chars.by_ref() {
                consumed += 1;
                if c == ')' {
                    closed = true;
                    break;
                }
                if c == '(' {
                    return None;
                }
            }
            if !closed {
                return None;
            }
            // Optional '!'
            if let Some('!') = chars.clone().next() {
                chars.next();
                consumed += 1;
            }
            if chars.next()? != ':' {
                return None;
            }
            consumed += 1;
        }
        '!' => {
            consumed += 1;
            if chars.next()? != ':' {
                return None;
            }
            consumed += 1;
        }
        ':' => {
            consumed += 1;
        }
        _ => return None,
    }
    Some(ci + consumed)
}

fn diff_spans(content: &str) -> PreviewSpans {
    let bytes = content.as_bytes();
    let mut ranges: Vec<(std::ops::Range<usize>, Style)> = Vec::new();

    let added_style = Style::default().fg(Color::Green);
    let removed_style = Style::default().fg(Color::Red);
    let hunk_style = Style::default().fg(Color::Cyan);
    let file_header_style = Style::default().fg(Color::Blue);
    let bold = ratatui::style::Modifier::BOLD;
    let dim = ratatui::style::Modifier::DIM;
    let label_style = Style::default().add_modifier(dim);
    let sha_style = Style::default().fg(Color::Yellow);
    let email_style = Style::default().add_modifier(dim);
    let prefix_style = Style::default().fg(Color::Magenta).add_modifier(bold);

    let mut pos = 0usize;
    let mut in_diff = false;
    let mut header_done = false;
    for line in content.lines() {
        let line_start = pos;
        let line_end = pos + line.len();

        if !in_diff && line.starts_with("diff --git") {
            in_diff = true;
            ranges.push((line_start..line_end, file_header_style));
        } else if in_diff {
            if line.starts_with("+++") || line.starts_with("---") {
                ranges.push((line_start..line_end, file_header_style));
            } else if line.starts_with("@@") {
                ranges.push((line_start..line_end, hunk_style));
            } else if line.starts_with('+') {
                ranges.push((line_start..line_end, added_style));
            } else if line.starts_with('-') {
                ranges.push((line_start..line_end, removed_style));
            }
        } else if let Some(rest) = line.strip_prefix("commit ") {
            ranges.push((line_start..line_start + 6, label_style));
            let sha_start = line_start + 7;
            ranges.push((sha_start..sha_start + rest.len(), sha_style));
        } else if let Some(rest) = line.strip_prefix("Author: ") {
            ranges.push((line_start..line_start + 7, label_style));
            // Color the name via author_color; dim the <email> tail.
            let name_start = line_start + 8;
            let (name, email_part) = match rest.find(" <") {
                Some(i) => (&rest[..i], &rest[i..]),
                None => (rest, ""),
            };
            let name_end = name_start + name.len();
            ranges.push((
                name_start..name_end,
                Style::default().fg(author_color(name)),
            ));
            if !email_part.is_empty() {
                ranges.push((name_end..name_end + email_part.len(), email_style));
            }
        } else if line.starts_with("Date:") {
            // "Date:" label dimmed; the date itself stays default.
            ranges.push((line_start..line_start + 5, label_style));
        } else if !header_done && line.starts_with("    ") && !line.trim().is_empty() {
            // First indented non-empty line in the header is the subject.
            // Color any conventional-commit prefix.
            if let Some(end) = conv_commit_prefix_end(line, 4) {
                ranges.push((line_start + 4..line_start + end, prefix_style));
            }
            header_done = true;
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
        // Picker preview runs on the UI thread; using the async grammar API
        // avoids a multi-second clone+compile blocking the renderer when a
        // user navigates to a file with an uncached grammar. Loading kicks
        // off the background compile and we render plain text this frame;
        // the next refresh after the load lands picks up Cached.
        let grammar = match self.directory.request_for_path(abs) {
            GrammarRequest::Cached(g) => g,
            GrammarRequest::Loading { .. } | GrammarRequest::Unknown => {
                return PreviewSpans::default();
            }
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
        // Identical to label so fuzzy match positions index into the visible
        // string. As a side benefit, users can filter on the status code
        // (e.g. typing `??` finds untracked).
        self.label(idx)
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
            Some(p) => PickerAction::Custom(Box::new(AppAction::OpenPath(p))),
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

    fn preserve_source_order(&self) -> bool {
        true
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
                    let initials = author_initials(&item.author);
                    format!("  {}  {}  {}", item.short_sha, initials, item.subject)
                })
            })
            .unwrap_or_default()
    }

    fn match_text(&self, idx: usize) -> String {
        // Match on the visible label (sha + initials + subject).
        self.label(idx)
    }

    fn label_styles(
        &self,
        idx: usize,
        label: &str,
    ) -> Option<Vec<(std::ops::Range<usize>, Style)>> {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return None;
        }
        let (short_len, author) = self.items.lock().ok().and_then(|g| {
            g.get(idx)
                .map(|i| (i.short_sha.chars().count(), i.author.clone()))
        })?;
        let mut out: Vec<(std::ops::Range<usize>, Style)> = Vec::new();
        let hash_start = 2usize;
        let hash_end = hash_start + short_len;
        out.push((hash_start..hash_end, Style::default().fg(Color::Yellow)));
        // Initials column: 2 chars after a 2-space gap.
        let initials_start = hash_end + 2;
        let initials_end = initials_start + 2;
        out.push((
            initials_start..initials_end,
            Style::default()
                .fg(author_color(&author))
                .add_modifier(ratatui::style::Modifier::BOLD),
        ));
        // Subject starts after another 2-space gap.
        let subject_start = initials_end + 2;
        if let Some(end) = conv_commit_prefix_end(label, subject_start) {
            out.push((
                subject_start..end,
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ));
        }
        Some(out)
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
            Some(sha) => PickerAction::Custom(Box::new(AppAction::ShowCommit(sha))),
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

// ── GitFileHistoryPicker ──────────────────────────────────────────────────

pub struct GitFileHistoryPicker {
    root: PathBuf,
    /// Path relative to the repo workdir.
    rel_path: PathBuf,
    items: Arc<Mutex<Vec<GitLogItem>>>,
    scan_done: Arc<AtomicBool>,
    is_sentinel: Arc<AtomicBool>,
    #[allow(dead_code)]
    directory: Arc<LanguageDirectory>,
    #[allow(dead_code)]
    theme: Arc<dyn Theme + Send + Sync>,
}

impl GitFileHistoryPicker {
    pub fn new(
        root: PathBuf,
        rel_path: PathBuf,
        theme: Arc<dyn Theme + Send + Sync>,
        directory: Arc<LanguageDirectory>,
    ) -> Self {
        Self {
            root,
            rel_path,
            items: Arc::new(Mutex::new(Vec::new())),
            scan_done: Arc::new(AtomicBool::new(false)),
            is_sentinel: Arc::new(AtomicBool::new(false)),
            directory,
            theme,
        }
    }
}

fn scan_git_file_history(
    root: PathBuf,
    rel_path: PathBuf,
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

        // Check whether this commit touched rel_path by comparing the blob
        // OID in this commit's tree vs its first parent's tree (or vs
        // "absent" for root commits / file additions).
        let this_tree = match commit.tree() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let this_entry = this_tree.get_path(&rel_path).ok();

        let touched = if let Ok(parent) = commit.parent(0) {
            let parent_tree = match parent.tree() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let parent_entry = parent_tree.get_path(&rel_path).ok();
            match (this_entry.as_ref(), parent_entry.as_ref()) {
                (Some(a), Some(b)) => a.id() != b.id(),
                (Some(_), None) => true, // file was added
                (None, Some(_)) => true, // file was deleted
                (None, None) => false,   // path never existed
            }
        } else {
            // Root commit: touched if path exists in this tree.
            this_entry.is_some()
        };

        if !touched {
            continue;
        }

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
        if count.is_multiple_of(32) && cancel.load(Ordering::Acquire) {
            break;
        }
    }

    if batch.is_empty() {
        // No commits touched this file — show a "no commits" sentinel row.
        sentinel.store(true, Ordering::Release);
        if let Ok(mut g) = items.lock() {
            g.push(GitLogItem {
                sha: String::new(),
                short_sha: String::new(),
                author: String::new(),
                subject: "no commits".to_owned(),
            });
        }
    } else if let Ok(mut g) = items.lock() {
        g.extend(batch);
    }

    done.store(true, Ordering::Release);
}

impl PickerLogic for GitFileHistoryPicker {
    fn title(&self) -> &str {
        // PickerLogic requires &str but the title is dynamic; store a
        // 'static sentinel and let the picker title widget call make_title()
        // separately. We return a fixed prefix here — the full path is in
        // the window title set by open_git_file_history_picker.
        "git file history"
    }

    fn preserve_source_order(&self) -> bool {
        true
    }

    fn item_count(&self) -> usize {
        self.items.lock().map(|g| g.len()).unwrap_or(0)
    }

    fn label(&self, idx: usize) -> String {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            let subject = self
                .items
                .lock()
                .ok()
                .and_then(|g| g.first().map(|i| i.subject.clone()))
                .unwrap_or_default();
            if subject.is_empty() {
                return SENTINEL_LABEL.to_owned();
            }
            return format!("  {subject}");
        }
        self.items
            .lock()
            .ok()
            .and_then(|g| {
                g.get(idx).map(|item| {
                    let initials = author_initials(&item.author);
                    format!("  {}  {}  {}", item.short_sha, initials, item.subject)
                })
            })
            .unwrap_or_default()
    }

    fn match_text(&self, idx: usize) -> String {
        self.label(idx)
    }

    fn label_styles(
        &self,
        idx: usize,
        label: &str,
    ) -> Option<Vec<(std::ops::Range<usize>, Style)>> {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return None;
        }
        let (short_len, author) = self.items.lock().ok().and_then(|g| {
            g.get(idx)
                .map(|i| (i.short_sha.chars().count(), i.author.clone()))
        })?;
        let mut out: Vec<(std::ops::Range<usize>, Style)> = Vec::new();
        let hash_start = 2usize;
        let hash_end = hash_start + short_len;
        out.push((hash_start..hash_end, Style::default().fg(Color::Yellow)));
        let initials_start = hash_end + 2;
        let initials_end = initials_start + 2;
        out.push((
            initials_start..initials_end,
            Style::default()
                .fg(author_color(&author))
                .add_modifier(ratatui::style::Modifier::BOLD),
        ));
        let subject_start = initials_end + 2;
        if let Some(end) = conv_commit_prefix_end(label, subject_start) {
            out.push((
                subject_start..end,
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ));
        }
        Some(out)
    }

    fn preview(&self, idx: usize) -> (hjkl_buffer::Buffer, String, PreviewSpans) {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return (
                hjkl_buffer::Buffer::new(),
                String::new(),
                PreviewSpans::default(),
            );
        }

        let sha = match self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|i| i.sha.clone()))
        {
            Some(s) if !s.is_empty() => s,
            _ => {
                return (
                    hjkl_buffer::Buffer::new(),
                    String::new(),
                    PreviewSpans::default(),
                );
            }
        };

        let repo = match Repository::discover(&self.root) {
            Ok(r) => r,
            Err(e) => {
                return (
                    hjkl_buffer::Buffer::new(),
                    format!("git error: {e}"),
                    PreviewSpans::default(),
                );
            }
        };

        let oid = match git2::Oid::from_str(&sha) {
            Ok(o) => o,
            Err(e) => {
                return (
                    hjkl_buffer::Buffer::new(),
                    format!("git error: {e}"),
                    PreviewSpans::default(),
                );
            }
        };

        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(e) => {
                return (
                    hjkl_buffer::Buffer::new(),
                    format!("git error: {e}"),
                    PreviewSpans::default(),
                );
            }
        };

        let body = render_commit(&repo, &commit);
        let spans = diff_spans(&body);
        (hjkl_buffer::Buffer::from_str(&body), String::new(), spans)
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
            Some(sha) if !sha.is_empty() => {
                PickerAction::Custom(Box::new(AppAction::ShowCommit(sha)))
            }
            _ => PickerAction::None,
        }
    }

    fn requery_mode(&self) -> RequeryMode {
        RequeryMode::FilterInMemory
    }

    fn enumerate(
        &mut self,
        _query: Option<&str>,
        cancel: Arc<AtomicBool>,
    ) -> Option<std::thread::JoinHandle<()>> {
        let items = Arc::clone(&self.items);
        let done = Arc::clone(&self.scan_done);
        let sentinel = Arc::clone(&self.is_sentinel);
        let root = self.root.clone();
        let rel_path = self.rel_path.clone();

        if let Ok(mut g) = items.lock() {
            g.clear();
        }
        done.store(false, Ordering::Release);
        sentinel.store(false, Ordering::Release);

        thread::Builder::new()
            .name("hjkl-picker-git-file-history".into())
            .spawn(move || scan_git_file_history(root, rel_path, items, done, sentinel, cancel))
            .ok()
    }
}

// ── GitBranchPicker ───────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum BranchKind {
    Local,
    Remote,
}

struct GitBranchItem {
    name: String,
    kind: BranchKind,
    is_head: bool,
    target_sha: Option<String>,
}

pub struct GitBranchPicker {
    root: PathBuf,
    items: Arc<Mutex<Vec<GitBranchItem>>>,
    scan_done: Arc<AtomicBool>,
    is_sentinel: Arc<AtomicBool>,
}

impl GitBranchPicker {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            items: Arc::new(Mutex::new(Vec::new())),
            scan_done: Arc::new(AtomicBool::new(false)),
            is_sentinel: Arc::new(AtomicBool::new(false)),
        }
    }
}

fn scan_git_branches(
    root: PathBuf,
    items: Arc<Mutex<Vec<GitBranchItem>>>,
    done: Arc<AtomicBool>,
    sentinel: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
) {
    let repo = match Repository::discover(&root) {
        Ok(r) => r,
        Err(_) => {
            sentinel.store(true, Ordering::Release);
            if let Ok(mut g) = items.lock() {
                g.push(GitBranchItem {
                    name: String::new(),
                    kind: BranchKind::Local,
                    is_head: false,
                    target_sha: None,
                });
            }
            done.store(true, Ordering::Release);
            return;
        }
    };

    let branches_iter = match repo.branches(None) {
        Ok(b) => b,
        Err(_) => {
            sentinel.store(true, Ordering::Release);
            done.store(true, Ordering::Release);
            return;
        }
    };

    struct RawBranch {
        name: String,
        kind: BranchKind,
        is_head: bool,
        commit_time: i64,
        sha: String,
    }

    let mut raw: Vec<RawBranch> = Vec::new();

    for result in branches_iter {
        if cancel.load(Ordering::Acquire) {
            done.store(true, Ordering::Release);
            return;
        }
        let (branch, branch_type) = match result {
            Ok(b) => b,
            Err(_) => continue,
        };
        let name = match branch.name() {
            Ok(Some(n)) => n.to_owned(),
            _ => continue,
        };
        // Skip HEAD symbolic remote refs.
        if name.ends_with("/HEAD") {
            continue;
        }
        let kind = match branch_type {
            BranchType::Local => BranchKind::Local,
            BranchType::Remote => BranchKind::Remote,
        };
        let is_head = branch.is_head();
        let (commit_time, sha) = match branch.get().peel(ObjectType::Commit) {
            Ok(obj) => match obj.into_commit() {
                Ok(c) => {
                    let time = c.time().seconds();
                    let sha = c.id().to_string();
                    (time, sha)
                }
                Err(_) => (-1i64, String::new()),
            },
            Err(_) => (-1i64, String::new()),
        };
        raw.push(RawBranch {
            name,
            kind,
            is_head,
            commit_time,
            sha,
        });
        if raw.len() >= 500 {
            break;
        }
    }

    // Sort buckets:
    //   0 — HEAD
    //   1 — local, top-level (no '/' in name)
    //   2 — local, namespaced (contains '/', e.g. feature/x)
    //   3 — remote
    // Within each bucket: most-recent commit first.
    raw.sort_by(|a, b| {
        let rank = |r: &RawBranch| {
            if r.is_head {
                0u8
            } else if r.kind == BranchKind::Local {
                if r.name.contains('/') { 2u8 } else { 1u8 }
            } else {
                3u8
            }
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| b.commit_time.cmp(&a.commit_time))
    });

    let mut parsed: Vec<GitBranchItem> = raw
        .into_iter()
        .map(|r| GitBranchItem {
            name: r.name,
            kind: r.kind,
            is_head: r.is_head,
            target_sha: if r.sha.is_empty() { None } else { Some(r.sha) },
        })
        .collect();

    if let Ok(mut g) = items.lock() {
        g.append(&mut parsed);
    }

    done.store(true, Ordering::Release);
}

impl PickerLogic for GitBranchPicker {
    fn title(&self) -> &str {
        "git branches"
    }

    fn preserve_source_order(&self) -> bool {
        true
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
                    let marker = if item.is_head { '*' } else { ' ' };
                    format!("  {} {}", marker, item.name)
                })
            })
            .unwrap_or_default()
    }

    fn match_text(&self, idx: usize) -> String {
        self.label(idx)
    }

    fn label_styles(
        &self,
        idx: usize,
        _label: &str,
    ) -> Option<Vec<(std::ops::Range<usize>, ratatui::style::Style)>> {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return None;
        }
        let (is_head, kind, name_len) = self.items.lock().ok().and_then(|g| {
            g.get(idx)
                .map(|i| (i.is_head, i.kind, i.name.chars().count()))
        })?;
        let _ = name_len;
        let mut out: Vec<(std::ops::Range<usize>, ratatui::style::Style)> = Vec::new();

        // Marker at char index 2..3.
        if is_head {
            out.push((
                2..3,
                ratatui::style::Style::default().fg(ratatui::style::Color::Green),
            ));
        }

        // Branch name starts at char index 4.
        // Compute byte offset for char 4 to end of label.
        // We store char ranges per the PickerLogic contract.
        let name_char_start = 4usize;
        let label = self.label(idx);
        let name_char_end = label.chars().count();
        if name_char_start < name_char_end {
            let style = match kind {
                BranchKind::Local => {
                    ratatui::style::Style::default().fg(ratatui::style::Color::Cyan)
                }
                BranchKind::Remote => {
                    ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM)
                }
            };
            out.push((name_char_start..name_char_end, style));
        }

        Some(out)
    }

    fn preview(&self, idx: usize) -> (hjkl_buffer::Buffer, String, PreviewSpans) {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return (
                hjkl_buffer::Buffer::new(),
                String::new(),
                PreviewSpans::default(),
            );
        }

        let target_sha = match self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).and_then(|i| i.target_sha.clone()))
        {
            Some(s) => s,
            None => {
                return (
                    hjkl_buffer::Buffer::new(),
                    String::new(),
                    PreviewSpans::default(),
                );
            }
        };

        let repo = match Repository::discover(&self.root) {
            Ok(r) => r,
            Err(e) => {
                return (
                    hjkl_buffer::Buffer::new(),
                    format!("git error: {e}"),
                    PreviewSpans::default(),
                );
            }
        };

        let oid = match git2::Oid::from_str(&target_sha) {
            Ok(o) => o,
            Err(e) => {
                return (
                    hjkl_buffer::Buffer::new(),
                    format!("git error: {e}"),
                    PreviewSpans::default(),
                );
            }
        };

        let mut revwalk = match repo.revwalk() {
            Ok(r) => r,
            Err(e) => {
                return (
                    hjkl_buffer::Buffer::new(),
                    format!("git error: {e}"),
                    PreviewSpans::default(),
                );
            }
        };

        if let Err(e) = revwalk.push(oid) {
            return (
                hjkl_buffer::Buffer::new(),
                format!("git error: {e}"),
                PreviewSpans::default(),
            );
        }

        let mut text = String::new();
        let mut count = 0usize;
        for oid_result in revwalk {
            let oid = match oid_result {
                Ok(o) => o,
                Err(_) => continue,
            };
            let commit = match repo.find_commit(oid) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let sha = commit.id().to_string();
            let short_sha = &sha[..7.min(sha.len())];
            let subject = commit.summary().unwrap_or("").to_owned();
            text.push_str(&format!("{short_sha}  {subject}\n"));
            count += 1;
            if count >= 30 {
                break;
            }
        }

        (
            hjkl_buffer::Buffer::from_str(&text),
            String::new(),
            PreviewSpans::default(),
        )
    }

    fn select(&self, idx: usize) -> PickerAction {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return PickerAction::None;
        }
        match self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|i| i.name.clone()))
        {
            Some(name) => PickerAction::Custom(Box::new(AppAction::CheckoutBranch(name))),
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
            .name("hjkl-picker-git-branch".into())
            .spawn(move || scan_git_branches(root, items, done, sentinel, cancel))
            .ok()
    }
}

// ── GitStashPicker ────────────────────────────────────────────────────────

struct StashItem {
    index: usize,
    oid: git2::Oid,
    message: String,
    branch_hint: String,
}

pub struct GitStashPicker {
    root: PathBuf,
    items: Arc<Mutex<Vec<StashItem>>>,
    scan_done: Arc<AtomicBool>,
    is_sentinel: Arc<AtomicBool>,
}

impl GitStashPicker {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            items: Arc::new(Mutex::new(Vec::new())),
            scan_done: Arc::new(AtomicBool::new(false)),
            is_sentinel: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Parse branch hint from stash message.
/// "WIP on <branch>: ..." or "On <branch>: ..." → branch name.
fn parse_stash_branch(msg: &str) -> String {
    let body = msg
        .strip_prefix("WIP on ")
        .or_else(|| msg.strip_prefix("On "))
        .unwrap_or("");
    match body.find(':') {
        Some(i) => body[..i].to_string(),
        None => String::new(),
    }
}

fn scan_git_stashes(
    root: PathBuf,
    items: Arc<Mutex<Vec<StashItem>>>,
    done: Arc<AtomicBool>,
    sentinel: Arc<AtomicBool>,
    _cancel: Arc<AtomicBool>,
) {
    let mut repo = match Repository::discover(&root) {
        Ok(r) => r,
        Err(_) => {
            sentinel.store(true, Ordering::Release);
            if let Ok(mut g) = items.lock() {
                g.push(StashItem {
                    index: 0,
                    oid: git2::Oid::zero(),
                    message: String::new(),
                    branch_hint: String::new(),
                });
            }
            done.store(true, Ordering::Release);
            return;
        }
    };

    let mut collected: Vec<StashItem> = Vec::new();
    let _ = repo.stash_foreach(|index, message, oid| {
        let branch_hint = parse_stash_branch(message);
        collected.push(StashItem {
            index,
            oid: *oid,
            message: message.to_string(),
            branch_hint,
        });
        true
    });

    if collected.is_empty() {
        sentinel.store(true, Ordering::Release);
        if let Ok(mut g) = items.lock() {
            g.push(StashItem {
                index: 0,
                oid: git2::Oid::zero(),
                message: "no stashes".to_string(),
                branch_hint: String::new(),
            });
        }
    } else if let Ok(mut g) = items.lock() {
        g.extend(collected);
    }

    done.store(true, Ordering::Release);
}

impl PickerLogic for GitStashPicker {
    fn title(&self) -> &str {
        "git stashes"
    }

    fn preserve_source_order(&self) -> bool {
        true
    }

    fn item_count(&self) -> usize {
        self.items.lock().map(|g| g.len()).unwrap_or(0)
    }

    fn label(&self, idx: usize) -> String {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            let msg = self
                .items
                .lock()
                .ok()
                .and_then(|g| g.first().map(|i| i.message.clone()))
                .unwrap_or_default();
            if msg.is_empty() {
                return SENTINEL_LABEL.to_owned();
            }
            return format!("  {msg}");
        }
        self.items
            .lock()
            .ok()
            .and_then(|g| {
                g.get(idx).map(|item| {
                    let branch = if item.branch_hint.is_empty() {
                        String::new()
                    } else {
                        format!("  on {}", item.branch_hint)
                    };
                    format!("  stash@{{{}}}{}  {}", item.index, branch, item.message)
                })
            })
            .unwrap_or_default()
    }

    fn match_text(&self, idx: usize) -> String {
        self.label(idx)
    }

    fn label_styles(
        &self,
        idx: usize,
        _label: &str,
    ) -> Option<Vec<(std::ops::Range<usize>, ratatui::style::Style)>> {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return None;
        }
        let (stash_idx, branch_hint) = self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|i| (i.index, i.branch_hint.clone())))?;
        let index_str = format!("stash@{{{stash_idx}}}");
        let index_len = index_str.chars().count();
        let mut out: Vec<(std::ops::Range<usize>, ratatui::style::Style)> = Vec::new();
        // "  stash@{N}" — index starts at char 2.
        let index_start = 2usize;
        let index_end = index_start + index_len;
        out.push((
            index_start..index_end,
            ratatui::style::Style::default().fg(ratatui::style::Color::Yellow),
        ));
        if !branch_hint.is_empty() {
            // "  on <branch>" — 2 chars gap after index
            let branch_label = format!("  on {branch_hint}");
            let branch_start = index_end;
            let branch_end = branch_start + branch_label.chars().count();
            out.push((
                branch_start..branch_end,
                ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM),
            ));
        }
        Some(out)
    }

    fn preview(&self, idx: usize) -> (hjkl_buffer::Buffer, String, PreviewSpans) {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return (
                hjkl_buffer::Buffer::new(),
                String::new(),
                PreviewSpans::default(),
            );
        }

        let oid = match self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|i| i.oid))
        {
            Some(o) if o != git2::Oid::zero() => o,
            _ => {
                return (
                    hjkl_buffer::Buffer::new(),
                    String::new(),
                    PreviewSpans::default(),
                );
            }
        };

        let repo = match Repository::discover(&self.root) {
            Ok(r) => r,
            Err(e) => {
                return (
                    hjkl_buffer::Buffer::new(),
                    format!("git error: {e}"),
                    PreviewSpans::default(),
                );
            }
        };

        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(e) => {
                return (
                    hjkl_buffer::Buffer::new(),
                    format!("git error: {e}"),
                    PreviewSpans::default(),
                );
            }
        };

        // Diff stash commit tree against parent 0 (HEAD at stash time).
        let diff_text = if let Ok(parent) = commit.parent(0) {
            let parent_tree = parent.tree().ok();
            let stash_tree = commit.tree().ok();
            match repo.diff_tree_to_tree(parent_tree.as_ref(), stash_tree.as_ref(), None) {
                Ok(d) => collect_diff(d),
                Err(_) => String::new(),
            }
        } else {
            String::new()
        };

        let spans = diff_spans(&diff_text);
        (
            hjkl_buffer::Buffer::from_str(&diff_text),
            String::new(),
            spans,
        )
    }

    fn select(&self, idx: usize) -> PickerAction {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return PickerAction::None;
        }
        match self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|i| i.index))
        {
            Some(stash_idx) => PickerAction::Custom(Box::new(AppAction::StashApply(stash_idx))),
            None => PickerAction::None,
        }
    }

    fn handle_key(&self, idx: usize, key: crossterm::event::KeyEvent) -> Option<PickerAction> {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return None;
        }
        let stash_idx = self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|i| i.index))?;
        // Alt+P / Alt+D so plain p/d stay free for filter input.
        if !key.modifiers.contains(crossterm::event::KeyModifiers::ALT) {
            return None;
        }
        match key.code {
            crossterm::event::KeyCode::Char('p') => Some(PickerAction::Custom(Box::new(
                AppAction::StashPop(stash_idx),
            ))),
            crossterm::event::KeyCode::Char('d') => Some(PickerAction::Custom(Box::new(
                AppAction::StashDrop(stash_idx),
            ))),
            _ => None,
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
            .name("hjkl-picker-git-stash".into())
            .spawn(move || scan_git_stashes(root, items, done, sentinel, cancel))
            .ok()
    }
}

// ── GitTagsPicker ─────────────────────────────────────────────────────────

struct TagItem {
    name: String,
    target_oid: git2::Oid,
    message: String,
    tagger: Option<(String, i64)>,
}

pub struct GitTagsPicker {
    root: PathBuf,
    items: Arc<Mutex<Vec<TagItem>>>,
    scan_done: Arc<AtomicBool>,
    is_sentinel: Arc<AtomicBool>,
}

impl GitTagsPicker {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            items: Arc::new(Mutex::new(Vec::new())),
            scan_done: Arc::new(AtomicBool::new(false)),
            is_sentinel: Arc::new(AtomicBool::new(false)),
        }
    }
}

fn format_tag_time(secs: i64) -> String {
    let t = Time::new(secs, 0);
    let adjusted = t.seconds();
    let (year, month, day, _weekday) = days_to_ymd(adjusted.div_euclid(86400));
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mon_str = months.get((month - 1) as usize).copied().unwrap_or("???");
    format!("{day} {mon_str} {year}")
}

fn scan_git_tags(
    root: PathBuf,
    items: Arc<Mutex<Vec<TagItem>>>,
    done: Arc<AtomicBool>,
    sentinel: Arc<AtomicBool>,
    _cancel: Arc<AtomicBool>,
) {
    let repo = match Repository::discover(&root) {
        Ok(r) => r,
        Err(_) => {
            sentinel.store(true, Ordering::Release);
            if let Ok(mut g) = items.lock() {
                g.push(TagItem {
                    name: "no tags".to_string(),
                    target_oid: git2::Oid::zero(),
                    message: String::new(),
                    tagger: None,
                });
            }
            done.store(true, Ordering::Release);
            return;
        }
    };

    let mut tags: Vec<TagItem> = Vec::new();
    let _ = repo.tag_foreach(|oid, name_bytes| {
        let name = String::from_utf8_lossy(name_bytes)
            .strip_prefix("refs/tags/")
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            return true;
        }
        let (target_oid, message, tagger) = if let Ok(tag) = repo.find_tag(oid) {
            let target = tag.target_id();
            let msg = tag.message().unwrap_or("").trim().to_string();
            let sig = tag
                .tagger()
                .map(|s| (s.name().unwrap_or("").to_string(), s.when().seconds()));
            (target, msg, sig)
        } else {
            (oid, String::new(), None)
        };
        tags.push(TagItem {
            name,
            target_oid,
            message,
            tagger,
        });
        true
    });

    if tags.is_empty() {
        sentinel.store(true, Ordering::Release);
        if let Ok(mut g) = items.lock() {
            g.push(TagItem {
                name: "no tags".to_string(),
                target_oid: git2::Oid::zero(),
                message: String::new(),
                tagger: None,
            });
        }
        done.store(true, Ordering::Release);
        return;
    }

    tags.sort_by(|a, b| {
        let ta = a.tagger.as_ref().map(|(_, t)| *t).unwrap_or(0);
        let tb = b.tagger.as_ref().map(|(_, t)| *t).unwrap_or(0);
        tb.cmp(&ta).then(b.name.cmp(&a.name))
    });

    if let Ok(mut g) = items.lock() {
        g.extend(tags);
    }

    done.store(true, Ordering::Release);
}

impl PickerLogic for GitTagsPicker {
    fn title(&self) -> &str {
        "git tags"
    }

    fn preserve_source_order(&self) -> bool {
        true
    }

    fn item_count(&self) -> usize {
        self.items.lock().map(|g| g.len()).unwrap_or(0)
    }

    fn label(&self, idx: usize) -> String {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            let name = self
                .items
                .lock()
                .ok()
                .and_then(|g| g.first().map(|i| i.name.clone()))
                .unwrap_or_default();
            return format!("  {name}");
        }
        self.items
            .lock()
            .ok()
            .and_then(|g| {
                g.get(idx).map(|item| {
                    if item.message.is_empty() {
                        format!("  {}", item.name)
                    } else {
                        format!("  {}  {}", item.name, item.message)
                    }
                })
            })
            .unwrap_or_default()
    }

    fn match_text(&self, idx: usize) -> String {
        self.label(idx)
    }

    fn label_styles(
        &self,
        idx: usize,
        _label: &str,
    ) -> Option<Vec<(std::ops::Range<usize>, ratatui::style::Style)>> {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return None;
        }
        let name_len = self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|i| i.name.chars().count()))?;
        let mut out: Vec<(std::ops::Range<usize>, ratatui::style::Style)> = Vec::new();
        let name_start = 2usize;
        let name_end = name_start + name_len;
        out.push((
            name_start..name_end,
            ratatui::style::Style::default().fg(ratatui::style::Color::Yellow),
        ));
        Some(out)
    }

    fn preview(&self, idx: usize) -> (hjkl_buffer::Buffer, String, PreviewSpans) {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return (
                hjkl_buffer::Buffer::new(),
                String::new(),
                PreviewSpans::default(),
            );
        }

        let (target_oid, tag_name, tag_message, tag_tagger) =
            match self.items.lock().ok().and_then(|g| {
                g.get(idx).map(|i| {
                    (
                        i.target_oid,
                        i.name.clone(),
                        i.message.clone(),
                        i.tagger.clone(),
                    )
                })
            }) {
                Some(v) => v,
                None => {
                    return (
                        hjkl_buffer::Buffer::new(),
                        String::new(),
                        PreviewSpans::default(),
                    );
                }
            };

        let repo = match Repository::discover(&self.root) {
            Ok(r) => r,
            Err(e) => {
                return (
                    hjkl_buffer::Buffer::new(),
                    format!("git error: {e}"),
                    PreviewSpans::default(),
                );
            }
        };

        let commit = match repo
            .find_object(target_oid, None)
            .and_then(|o| o.peel_to_commit())
        {
            Ok(c) => c,
            Err(e) => {
                return (
                    hjkl_buffer::Buffer::new(),
                    format!("git error: {e}"),
                    PreviewSpans::default(),
                );
            }
        };

        let commit_body = render_commit(&repo, &commit);

        let body = if !tag_message.is_empty() || tag_tagger.is_some() {
            let mut header = format!("Tag: {tag_name}\n");
            if let Some((tagger_name, tagger_secs)) = tag_tagger {
                header.push_str(&format!("Tagger: {tagger_name}\n"));
                header.push_str(&format!("Date:   {}\n", format_tag_time(tagger_secs)));
            }
            if !tag_message.is_empty() {
                header.push('\n');
                for line in tag_message.lines() {
                    header.push_str("    ");
                    header.push_str(line);
                    header.push('\n');
                }
            }
            header.push_str("\n--- COMMIT ---\n\n");
            header + &commit_body
        } else {
            commit_body
        };

        let spans = diff_spans(&body);
        (hjkl_buffer::Buffer::from_str(&body), String::new(), spans)
    }

    fn select(&self, idx: usize) -> PickerAction {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return PickerAction::None;
        }
        match self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|i| i.name.clone()))
        {
            Some(name) => PickerAction::Custom(Box::new(AppAction::CheckoutTag(name))),
            None => PickerAction::None,
        }
    }

    fn requery_mode(&self) -> hjkl_picker::RequeryMode {
        hjkl_picker::RequeryMode::FilterInMemory
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
            .name("hjkl-picker-git-tags".into())
            .spawn(move || scan_git_tags(root, items, done, sentinel, cancel))
            .ok()
    }
}

// ── GitRemotesPicker ──────────────────────────────────────────────────────

struct RemoteItem {
    name: String,
    url: String,
    branch_count: usize,
}

pub struct GitRemotesPicker {
    root: PathBuf,
    items: Arc<Mutex<Vec<RemoteItem>>>,
    scan_done: Arc<AtomicBool>,
    is_sentinel: Arc<AtomicBool>,
}

impl GitRemotesPicker {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            items: Arc::new(Mutex::new(Vec::new())),
            scan_done: Arc::new(AtomicBool::new(false)),
            is_sentinel: Arc::new(AtomicBool::new(false)),
        }
    }
}

fn scan_git_remotes(
    root: PathBuf,
    items: Arc<Mutex<Vec<RemoteItem>>>,
    done: Arc<AtomicBool>,
    sentinel: Arc<AtomicBool>,
    _cancel: Arc<AtomicBool>,
) {
    let repo = match Repository::discover(&root) {
        Ok(r) => r,
        Err(_) => {
            sentinel.store(true, Ordering::Release);
            if let Ok(mut g) = items.lock() {
                g.push(RemoteItem {
                    name: "no remotes".to_string(),
                    url: String::new(),
                    branch_count: 0,
                });
            }
            done.store(true, Ordering::Release);
            return;
        }
    };

    let names = match repo.remotes() {
        Ok(n) => n,
        Err(_) => {
            sentinel.store(true, Ordering::Release);
            if let Ok(mut g) = items.lock() {
                g.push(RemoteItem {
                    name: "no remotes".to_string(),
                    url: String::new(),
                    branch_count: 0,
                });
            }
            done.store(true, Ordering::Release);
            return;
        }
    };

    let mut remotes: Vec<RemoteItem> = Vec::new();
    for name in names.iter().flatten() {
        if let Ok(remote) = repo.find_remote(name) {
            let url = remote.url().unwrap_or("").to_string();
            let prefix = format!("refs/remotes/{name}/");
            let branch_count = repo
                .references_glob(&format!("{prefix}*"))
                .map(|iter| iter.count())
                .unwrap_or(0);
            remotes.push(RemoteItem {
                name: name.to_string(),
                url,
                branch_count,
            });
        }
    }

    if remotes.is_empty() {
        sentinel.store(true, Ordering::Release);
        if let Ok(mut g) = items.lock() {
            g.push(RemoteItem {
                name: "no remotes".to_string(),
                url: String::new(),
                branch_count: 0,
            });
        }
        done.store(true, Ordering::Release);
        return;
    }

    remotes.sort_by(|a, b| a.name.cmp(&b.name));

    if let Ok(mut g) = items.lock() {
        g.extend(remotes);
    }

    done.store(true, Ordering::Release);
}

impl PickerLogic for GitRemotesPicker {
    fn title(&self) -> &str {
        "git remotes"
    }

    fn preserve_source_order(&self) -> bool {
        true
    }

    fn item_count(&self) -> usize {
        self.items.lock().map(|g| g.len()).unwrap_or(0)
    }

    fn label(&self, idx: usize) -> String {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            let name = self
                .items
                .lock()
                .ok()
                .and_then(|g| g.first().map(|i| i.name.clone()))
                .unwrap_or_default();
            return format!("  {name}");
        }
        self.items
            .lock()
            .ok()
            .and_then(|g| {
                g.get(idx)
                    .map(|item| format!("  {}  ↑{}  {}", item.name, item.branch_count, item.url))
            })
            .unwrap_or_default()
    }

    fn match_text(&self, idx: usize) -> String {
        self.label(idx)
    }

    fn label_styles(
        &self,
        idx: usize,
        _label: &str,
    ) -> Option<Vec<(std::ops::Range<usize>, ratatui::style::Style)>> {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return None;
        }
        let (name_len, branch_count) = self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|i| (i.name.chars().count(), i.branch_count)))?;
        let mut out: Vec<(std::ops::Range<usize>, ratatui::style::Style)> = Vec::new();
        // Name in yellow: "  <name>"
        let name_start = 2usize;
        let name_end = name_start + name_len;
        out.push((
            name_start..name_end,
            ratatui::style::Style::default().fg(ratatui::style::Color::Yellow),
        ));
        // Branch count dim: "  ↑N" — ↑ is 3 bytes (U+2191), count digits vary.
        // char positions: name_end, then 2 spaces, then ↑, then digits.
        // We work in chars: ↑ is 1 char.
        let count_arrow_start = name_end + 2; // "  "
        let count_str = format!("↑{branch_count}");
        let count_end = count_arrow_start + count_str.chars().count();
        out.push((
            count_arrow_start..count_end,
            ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM),
        ));
        Some(out)
    }

    fn preview(&self, idx: usize) -> (hjkl_buffer::Buffer, String, PreviewSpans) {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return (
                hjkl_buffer::Buffer::new(),
                String::new(),
                PreviewSpans::default(),
            );
        }

        let (name, url) = match self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|i| (i.name.clone(), i.url.clone())))
        {
            Some(v) => v,
            None => {
                return (
                    hjkl_buffer::Buffer::new(),
                    String::new(),
                    PreviewSpans::default(),
                );
            }
        };

        let repo = match Repository::discover(&self.root) {
            Ok(r) => r,
            Err(e) => {
                return (
                    hjkl_buffer::Buffer::new(),
                    format!("git error: {e}"),
                    PreviewSpans::default(),
                );
            }
        };

        let mut body = format!("Remote: {name}\nURL:    {url}\n\nBranches:\n");
        let prefix = format!("refs/remotes/{name}/");
        if let Ok(iter) = repo.references_glob(&format!("{prefix}*")) {
            let mut count = 0usize;
            for r in iter.flatten() {
                let short = r
                    .shorthand()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| r.name().unwrap_or("").to_string());
                if short.ends_with("/HEAD") {
                    continue;
                }
                body.push_str(&format!("  {short}\n"));
                count += 1;
                if count >= 50 {
                    body.push_str("  ...\n");
                    break;
                }
            }
            if count == 0 {
                body.push_str("  (none fetched yet)\n");
            }
        }

        (
            hjkl_buffer::Buffer::from_str(&body),
            String::new(),
            PreviewSpans::default(),
        )
    }

    fn select(&self, idx: usize) -> PickerAction {
        if self.is_sentinel.load(Ordering::Acquire) && idx == 0 {
            return PickerAction::None;
        }
        match self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|i| i.name.clone()))
        {
            Some(name) => PickerAction::Custom(Box::new(AppAction::FetchRemote(name))),
            None => PickerAction::None,
        }
    }

    fn requery_mode(&self) -> hjkl_picker::RequeryMode {
        hjkl_picker::RequeryMode::FilterInMemory
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
            .name("hjkl-picker-git-remotes".into())
            .spawn(move || scan_git_remotes(root, items, done, sentinel, cancel))
            .ok()
    }
}
