use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use hjkl_buffer::View;

use crate::logic::{PickerAction, PickerLogic, RequeryMode};
use crate::preview::load_preview;

/// One ripgrep match result.
pub struct RgMatch {
    pub path: PathBuf,
    pub line: u32, // 1-based
    pub _col: u32, // 1-based, byte column (reserved for future use)
    pub text: String,
}

/// Which search backend is available on this system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrepBackend {
    /// ripgrep (`rg`) — preferred; produces rich JSON output.
    Rg,
    /// POSIX `grep` — fallback when ripgrep is not installed.
    Grep,
    /// Windows-native `findstr` — fallback on vanilla Windows.
    Findstr,
    /// No supported search tool found on PATH.
    Neither,
}

/// Decide which search backend to use, caching the result for the process.
///
/// This used to shell out up to three `--version`/`/?` probes on **every**
/// requery (i.e. every keystroke in the grep picker). The available backend
/// doesn't change during a session, so the probe now runs once and the answer
/// is memoized.
pub fn detect_grep_backend() -> GrepBackend {
    static CACHED: std::sync::OnceLock<GrepBackend> = std::sync::OnceLock::new();
    *CACHED.get_or_init(probe_grep_backend)
}

/// Probe PATH to decide which backend is available. Cheap
/// (`--version` exits immediately) but spawns subprocesses, so callers should
/// go through the memoized [`detect_grep_backend`].
fn probe_grep_backend() -> GrepBackend {
    if std::process::Command::new("rg")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return GrepBackend::Rg;
    }
    if std::process::Command::new("grep")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return GrepBackend::Grep;
    }
    if std::process::Command::new("findstr")
        .arg("/?")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return GrepBackend::Findstr;
    }
    GrepBackend::Neither
}

/// Parse one JSON line from `rg --json` output. Returns `Some(RgMatch)` for
/// lines of `"type":"match"`, `None` for everything else.
pub fn parse_rg_json_line(line: &str, root: &Path) -> Option<RgMatch> {
    if !line.contains("\"type\":\"match\"") {
        return None;
    }

    let path_text = extract_json_string(line, "\"path\":{\"text\":")?;
    let line_number: u32 = extract_json_u32(line, "\"line_number\":")?;
    let col: u32 = extract_json_u32(line, "\"start\":").unwrap_or(0) + 1;
    let match_text = extract_json_string(line, "\"lines\":{\"text\":").unwrap_or_default();
    let match_text = match_text.trim_end_matches('\n').to_owned();

    let abs_path = PathBuf::from(&path_text);
    let rel_path = abs_path
        .strip_prefix(root)
        .map(|p| p.to_path_buf())
        .unwrap_or(abs_path);

    Some(RgMatch {
        path: rel_path,
        line: line_number,
        _col: col,
        text: match_text,
    })
}

/// Extract a JSON string value that immediately follows the given key pattern.
pub fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let start = json.find(key)? + key.len();
    let rest = &json[start..];
    let rest = rest.trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    let inner = &rest[1..];
    let mut result = String::new();
    let mut chars = inner.chars();
    loop {
        match chars.next()? {
            '"' => break,
            '\\' => match chars.next()? {
                '"' => result.push('"'),
                '\\' => result.push('\\'),
                'n' => result.push('\n'),
                't' => result.push('\t'),
                c => {
                    result.push('\\');
                    result.push(c);
                }
            },
            c => result.push(c),
        }
    }
    Some(result)
}

/// Extract a u32 JSON number value that immediately follows the given key pattern.
pub fn extract_json_u32(json: &str, key: &str) -> Option<u32> {
    let start = json.find(key)? + key.len();
    let rest = json[start..].trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Parse one line of `grep -rn` output (`path:line:text`).
///
/// Splits on `:` from the left: first segment is path, second is the 1-based
/// line number, everything after is the matched text (which may itself contain
/// `:`). Returns `None` for lines that don't conform (binary-file warnings,
/// etc.).
pub fn parse_grep_line(raw: &str, root: &Path) -> Option<RgMatch> {
    // Windows paths include a drive-letter prefix like `C:` that collides with
    // grep's `:` field separator. Skip past it before splitting so the colon
    // count matches the GNU/POSIX format the parser expects (path:line:text).
    let scan_start =
        if raw.len() >= 2 && raw.as_bytes()[1] == b':' && raw.as_bytes()[0].is_ascii_alphabetic() {
            2
        } else {
            0
        };
    let scan = &raw[scan_start..];
    let sep1 = scan.find(':')?;
    let after1 = sep1 + 1;
    let sep2 = scan[after1..].find(':')? + after1;
    let path_str = &raw[..scan_start + sep1];
    let line_str = &scan[after1..sep2];
    let text = scan[sep2 + 1..].trim_end_matches('\n').to_owned();

    let line: u32 = line_str.parse().ok()?;

    let abs_path = PathBuf::from(path_str);
    let rel_path = abs_path
        .strip_prefix(root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| abs_path);

    Some(RgMatch {
        path: rel_path,
        line,
        _col: 1,
        text,
    })
}

/// Source for the ripgrep content-search picker.
///
/// Bonsai-agnostic — preview returns the file contents only. The host
/// (e.g. apps/hjkl) layers syntax spans via `preview_path`.
pub struct RgSource {
    root: PathBuf,
    pub items: Arc<Mutex<Vec<RgMatch>>>,
}

impl RgSource {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            items: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl PickerLogic for RgSource {
    fn title(&self) -> &str {
        "grep"
    }

    fn requery_mode(&self) -> RequeryMode {
        RequeryMode::Spawn
    }

    fn item_count(&self) -> usize {
        self.items.lock().map(|g| g.len()).unwrap_or(0)
    }

    fn label(&self, idx: usize) -> String {
        self.items
            .lock()
            .ok()
            .and_then(|g| {
                g.get(idx).map(|m| {
                    let path = m.path.display().to_string();
                    let text = if m.text.chars().count() > 80 {
                        let cut: String = m.text.chars().take(79).collect();
                        format!("{cut}…")
                    } else {
                        m.text.clone()
                    };
                    // Two-cell prefix matches BufferSource's marker column
                    // so labels stay vertically aligned across pickers.
                    format!("  {}:{}: {}", path, m.line, text)
                })
            })
            .unwrap_or_default()
    }

    fn match_text(&self, idx: usize) -> String {
        self.label(idx)
    }

    fn has_preview(&self) -> bool {
        true
    }

    fn preview(&self, idx: usize) -> (View, String) {
        let (path, line) = match self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|m| (m.path.clone(), m.line)))
        {
            Some(v) => v,
            None => return (View::new(), String::new()),
        };
        // Sentinel: no path means rg wasn't found.
        if path.as_os_str().is_empty() {
            return (View::new(), String::new());
        }
        let abs = self.root.join(&path);
        let (content, status) = load_preview(&abs);
        if !status.is_empty() {
            return (View::from_str(&content), status);
        }

        // Render the full file; the picker's `preview_top_row` puts the
        // match line near the top of the visible window. Keeping the buffer
        // intact preserves correct gutter line numbers.
        let mut buf = View::from_str(&content);
        let match_row = (line as usize).saturating_sub(1);
        buf.set_cursor(hjkl_buffer::Position {
            row: match_row,
            col: 0,
        });
        (buf, String::new())
    }

    fn preview_path(&self, idx: usize) -> Option<PathBuf> {
        let path = self
            .items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|m| m.path.clone()))?;
        if path.as_os_str().is_empty() {
            return None;
        }
        Some(self.root.join(path))
    }

    fn preview_top_row(&self, idx: usize) -> usize {
        self.items
            .lock()
            .ok()
            .and_then(|g| {
                g.get(idx)
                    .map(|m| (m.line as usize).saturating_sub(1).saturating_sub(2))
            })
            .unwrap_or(0)
    }

    fn preview_match_row(&self, idx: usize) -> Option<usize> {
        self.items
            .lock()
            .ok()
            .and_then(|g| g.get(idx).map(|m| (m.line as usize).saturating_sub(1)))
    }

    fn select(&self, _idx: usize) -> PickerAction {
        // RgSource is always wrapped by an app-side source (e.g.
        // HighlightedRgSource) that overrides `select` and boxes an
        // app-specific `AppAction`. This base impl is never called directly.
        PickerAction::None
    }

    fn label_match_positions(&self, idx: usize, query: &str, label: &str) -> Option<Vec<usize>> {
        if query.is_empty() {
            return Some(Vec::new());
        }
        // Retrieve the text portion of the match so we can compute the prefix
        // length and restrict highlighting to content only.
        let text = self.items.lock().ok().and_then(|g| {
            g.get(idx).map(|m| {
                // Mirror the truncation applied in `label()`.
                if m.text.chars().count() > 80 {
                    let cut: String = m.text.chars().take(79).collect();
                    format!("{cut}\u{2026}") // U+2026 HORIZONTAL ELLIPSIS
                } else {
                    m.text.clone()
                }
            })
        })?;

        // The label is "path:line: text". Prefix char count = label char
        // count minus text char count.
        let label_chars = label.chars().count();
        let text_chars = text.chars().count();
        let prefix_len = label_chars.saturating_sub(text_chars);

        // Build regex from query: try literal compile first, fall back to
        // regex::escape for literal matching.
        let re = regex::Regex::new(query)
            .or_else(|_| regex::Regex::new(&regex::escape(query)))
            .ok()?;

        // Collect byte-offset → char-index mapping for `text` so we can
        // convert regex byte ranges to char indices.
        let char_byte_offsets: Vec<usize> =
            text.char_indices().map(|(byte_off, _)| byte_off).collect();

        let mut positions: Vec<usize> = Vec::new();
        for m in re.find_iter(&text) {
            let byte_start = m.start();
            let byte_end = m.end();
            // Find which char indices in `text` fall within [byte_start, byte_end).
            for (char_i, &byte_off) in char_byte_offsets.iter().enumerate() {
                if byte_off >= byte_start && byte_off < byte_end {
                    // Offset by prefix_len to get the char index in the label.
                    positions.push(prefix_len + char_i);
                }
            }
        }
        positions.sort_unstable();
        positions.dedup();
        Some(positions)
    }

    fn enumerate(
        &mut self,
        query: Option<&str>,
        cancel: Arc<AtomicBool>,
    ) -> Option<JoinHandle<()>> {
        // NOTE: Do NOT clear items here. The clear is deferred into the spawn
        // closure so that the previous results stay visible until the first
        // new batch arrives, preventing a flash-on-each-keystroke.
        // If the query is empty, clear synchronously (nothing to show).
        let q = match query {
            Some(q) if !q.trim().is_empty() => q.to_owned(),
            // Empty query → clear and show nothing.
            _ => {
                if let Ok(mut g) = self.items.lock() {
                    g.clear();
                }
                return None;
            }
        };

        let items = Arc::clone(&self.items);
        let root = self.root.clone();

        thread::Builder::new()
            .name("hjkl-rg-scan".into())
            .spawn(move || {
                use std::io::{BufRead, BufReader};
                use std::process::Stdio;

                let backend = detect_grep_backend();

                match backend {
                    GrepBackend::Rg => {
                        let child = std::process::Command::new("rg")
                            .args([
                                "--json",
                                "--no-config",
                                "--smart-case",
                                "--max-count",
                                "200",
                                "--",
                                &q,
                                root.to_str().unwrap_or("."),
                            ])
                            .stdout(Stdio::piped())
                            .stderr(Stdio::null())
                            .spawn();

                        let mut child = match child {
                            Ok(c) => c,
                            Err(_) => {
                                // Spawn failed — clear stale results.
                                if let Ok(mut g) = items.lock() {
                                    g.clear();
                                }
                                return;
                            }
                        };

                        let stdout = match child.stdout.take() {
                            Some(s) => s,
                            None => {
                                if let Ok(mut g) = items.lock() {
                                    g.clear();
                                }
                                return;
                            }
                        };

                        let reader = BufReader::new(stdout);
                        let mut batch: Vec<RgMatch> = Vec::with_capacity(32);
                        let mut total = 0usize;
                        // Cleared atomically on first push so old results
                        // remain visible during rg startup latency.
                        let mut first_push_done = false;
                        // `--max-count` is per-file; cap the overall total too
                        // so a broad pattern over a huge tree can't grow the
                        // item vec without bound (matches GREP_CAP below).
                        const RG_CAP: usize = 1000;

                        for line_result in reader.lines() {
                            if cancel.load(Ordering::Acquire) {
                                let _ = child.kill();
                                let _ = child.wait();
                                return;
                            }
                            let line = match line_result {
                                Ok(l) => l,
                                Err(_) => continue,
                            };
                            if let Some(rg_match) = parse_rg_json_line(&line, &root) {
                                batch.push(rg_match);
                                total += 1;
                                if batch.len() >= 32
                                    && let Ok(mut g) = items.lock()
                                {
                                    if !first_push_done {
                                        g.clear();
                                        first_push_done = true;
                                    }
                                    g.extend(batch.drain(..));
                                }
                                if total >= RG_CAP {
                                    let _ = child.kill();
                                    break;
                                }
                            }
                            if cancel.load(Ordering::Acquire) {
                                let _ = child.kill();
                                let _ = child.wait();
                                return;
                            }
                        }
                        // Flush the remaining batch — or clear stale results
                        // when rg exited with zero matches. Skipped when a
                        // newer requery superseded this one: `items` belongs
                        // to the new scan now, so flushing (or clearing)
                        // would clobber its fresh results.
                        if let Ok(mut g) = items.lock()
                            && !cancel.load(Ordering::Acquire)
                        {
                            if !first_push_done {
                                g.clear();
                            }
                            g.extend(batch.drain(..));
                        }
                        let _ = child.wait();
                    }

                    GrepBackend::Grep => {
                        let child = std::process::Command::new("grep")
                            .args([
                                "-rn",
                                "-E",
                                "--color=never",
                                "--",
                                &q,
                                root.to_str().unwrap_or("."),
                            ])
                            .stdout(Stdio::piped())
                            .stderr(Stdio::null())
                            .spawn();

                        let mut child = match child {
                            Ok(c) => c,
                            Err(_) => {
                                if let Ok(mut g) = items.lock() {
                                    g.clear();
                                }
                                return;
                            }
                        };

                        let stdout = match child.stdout.take() {
                            Some(s) => s,
                            None => {
                                if let Ok(mut g) = items.lock() {
                                    g.clear();
                                }
                                return;
                            }
                        };

                        let reader = BufReader::new(stdout);
                        let mut batch: Vec<RgMatch> = Vec::with_capacity(32);
                        let mut total = 0usize;
                        let mut first_push_done = false;
                        const GREP_CAP: usize = 1000;

                        for line_result in reader.lines() {
                            if cancel.load(Ordering::Acquire) {
                                let _ = child.kill();
                                let _ = child.wait();
                                return;
                            }
                            let raw = match line_result {
                                Ok(l) => l,
                                Err(_) => continue,
                            };
                            if raw.is_empty() {
                                continue;
                            }
                            // Format: path:line_number:text
                            // Split on ':' from the left, first two segments
                            // are path and line number; rest is text (may
                            // contain ':'). Skip lines that don't conform
                            // (binary file warnings, etc.).
                            if let Some(m) = parse_grep_line(&raw, &root) {
                                batch.push(m);
                                total += 1;
                                if batch.len() >= 32
                                    && let Ok(mut g) = items.lock()
                                {
                                    if !first_push_done {
                                        g.clear();
                                        first_push_done = true;
                                    }
                                    g.extend(batch.drain(..));
                                }
                                if total >= GREP_CAP {
                                    let _ = child.kill();
                                    break;
                                }
                            }
                            if cancel.load(Ordering::Acquire) {
                                let _ = child.kill();
                                let _ = child.wait();
                                return;
                            }
                        }
                        // Flush / zero-match clear, skipped when superseded
                        // (see the Rg arm above for rationale).
                        if let Ok(mut g) = items.lock()
                            && !cancel.load(Ordering::Acquire)
                        {
                            if !first_push_done {
                                g.clear();
                            }
                            g.extend(batch.drain(..));
                        }
                        let _ = child.wait();
                    }

                    GrepBackend::Findstr => {
                        // Windows-native findstr: findstr /S /N /R <pattern> <root>\*
                        // Output format: path:line:text — same as grep -n, reuse parse_grep_line.
                        let search_glob = root.join("*");
                        let child = std::process::Command::new("findstr")
                            .args([
                                "/S",
                                "/N",
                                "/R",
                                &q,
                                search_glob.to_str().unwrap_or("*"),
                            ])
                            .stdout(Stdio::piped())
                            .stderr(Stdio::null())
                            .spawn();

                        let mut child = match child {
                            Ok(c) => c,
                            Err(_) => {
                                if let Ok(mut g) = items.lock() {
                                    g.clear();
                                }
                                return;
                            }
                        };

                        let stdout = match child.stdout.take() {
                            Some(s) => s,
                            None => {
                                if let Ok(mut g) = items.lock() {
                                    g.clear();
                                }
                                return;
                            }
                        };

                        let reader = BufReader::new(stdout);
                        let mut batch: Vec<RgMatch> = Vec::with_capacity(32);
                        let mut total = 0usize;
                        let mut first_push_done = false;
                        const FINDSTR_CAP: usize = 1000;

                        for line_result in reader.lines() {
                            if cancel.load(Ordering::Acquire) {
                                let _ = child.kill();
                                let _ = child.wait();
                                return;
                            }
                            let raw = match line_result {
                                Ok(l) => l,
                                Err(_) => continue,
                            };
                            if raw.is_empty() {
                                continue;
                            }
                            if let Some(m) = parse_grep_line(&raw, &root) {
                                batch.push(m);
                                total += 1;
                                if batch.len() >= 32
                                    && let Ok(mut g) = items.lock()
                                {
                                    if !first_push_done {
                                        g.clear();
                                        first_push_done = true;
                                    }
                                    g.extend(batch.drain(..));
                                }
                                if total >= FINDSTR_CAP {
                                    let _ = child.kill();
                                    break;
                                }
                            }
                            if cancel.load(Ordering::Acquire) {
                                let _ = child.kill();
                                let _ = child.wait();
                                return;
                            }
                        }
                        // Flush / zero-match clear, skipped when superseded
                        // (see the Rg arm above for rationale).
                        if let Ok(mut g) = items.lock()
                            && !cancel.load(Ordering::Acquire)
                        {
                            if !first_push_done {
                                g.clear();
                            }
                            g.extend(batch.drain(..));
                        }
                        let _ = child.wait();
                    }

                    GrepBackend::Neither => {
                        // No search tool found — push sentinel item.
                        // Clear first so the sentinel replaces stale results.
                        if let Ok(mut g) = items.lock() {
                            g.clear();
                            g.push(RgMatch {
                                path: PathBuf::new(),
                                line: 0,
                                _col: 0,
                                text: "no grep tool found — install ripgrep, grep, or findstr to use :rg"
                                    .into(),
                            });
                        }
                    }
                }
            })
            .ok()
    }
}

#[cfg(test)]
mod parse_grep_line_tests {
    use super::*;

    #[test]
    fn parses_posix_path_line_text() {
        let m = parse_grep_line("src/main.rs:42:fn main()", Path::new("/tmp/proj"))
            .expect("must parse posix path");
        assert_eq!(m.path, PathBuf::from("src/main.rs"));
        assert_eq!(m.line, 42);
        assert_eq!(m.text, "fn main()");
    }

    #[test]
    fn parses_path_with_colon_in_text() {
        let m = parse_grep_line("a.txt:7:label: value", Path::new("/tmp"))
            .expect("colon-in-text must not break parsing");
        assert_eq!(m.line, 7);
        assert_eq!(m.text, "label: value");
    }

    #[test]
    fn parses_windows_drive_letter_path() {
        // Windows grep output: "C:\Users\...\findme.txt:2:UNIQUE"
        // The drive-letter colon must not be treated as a field separator.
        let m = parse_grep_line(
            r"C:\Users\runneradmin\Temp\findme.txt:2:UNIQUE_NEEDLE_42",
            Path::new(r"C:\Users\runneradmin\Temp"),
        )
        .expect("windows drive letter must parse");
        assert_eq!(m.line, 2);
        assert_eq!(m.text, "UNIQUE_NEEDLE_42");
        // strip_prefix below only triggers on Windows because the host OS
        // determines whether `\\` are treated as path separators. The
        // load-bearing assertion is just that the path contains the file
        // name — without the drive-letter fix, parse returns None entirely.
        assert!(m.path.to_string_lossy().ends_with("findme.txt"));
    }

    #[test]
    fn rejects_line_without_two_colons() {
        assert!(parse_grep_line("nopaththereonly", Path::new("/tmp")).is_none());
        assert!(parse_grep_line("only:onecolon", Path::new("/tmp")).is_none());
    }

    #[test]
    fn rejects_non_numeric_line() {
        // Without the drive-letter fix this is what we used to do to ALL
        // windows lines — making sure we still reject genuinely bad input.
        assert!(parse_grep_line("path:notanumber:text", Path::new("/tmp")).is_none());
    }
}
