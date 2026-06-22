//! Hop / easymotion label-jump overlay (#197).
//!
//! When active, every word-start (or line, etc.) in the visible viewport gets
//! a short label painted over it. Typing the label jumps the cursor to that
//! position. Works in Normal mode (plain jump) and Visual mode (the engine
//! stays in visual, so moving the cursor to the target extends the selection).
//! Operator-pending is intentionally NOT supported — with leader=`<Space>`,
//! `d<Space>` is vim's delete-char motion, owned by the engine.
//!
//! Architecture: purely app-level overlay — the engine never sees the hop keys.

use hjkl_buffer::is_keyword_char;
use hjkl_engine::{Host, VimMode};

use crate::app::window::WindowId;

use super::App;

// ── Public types ────────────────────────────────────────────────────────────

/// The four hop target kinds bound to the leader keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HopKind {
    /// `<leader>w` — word-start targets (vim `w` semantics).
    Word,
    /// `<leader>W` — WORD-start targets (whitespace-delimited).
    WordCap,
    /// `<leader>j` — first-non-blank of each visible line below cursor.
    LineBelow,
    /// `<leader>k` — first-non-blank of each visible line above cursor.
    LineAbove,
}

/// One labeled jump target.
#[derive(Debug, Clone)]
pub(crate) struct HopTarget {
    /// Buffer document row (0-based).
    pub row: usize,
    /// Buffer document column (0-based char index).
    pub col: usize,
    /// Label string to paint and wait for ("a", "b", ..., "aa", "ab", ...).
    pub label: String,
}

/// Live hop overlay state stored on [`App`].
#[derive(Debug, Clone)]
pub(crate) struct HopState {
    /// Which window was focused when hop started (for rendering).
    pub win_id: WindowId,
    /// All labeled targets in reading order (top-to-bottom, left-to-right).
    pub targets: Vec<HopTarget>,
    /// Characters the user has typed so far while matching a label.
    pub typed: String,
    /// Whether the editor was in a Visual mode when hop started.
    /// When `true`, `jump_cursor` extends the active visual selection (the engine
    /// remains in visual mode so the anchor is preserved). Stored for future use
    /// (e.g. showing a "VISUAL HOP" mode label, restricting target set).
    #[allow(dead_code)]
    pub visual: bool,
}

// ── Label generation ────────────────────────────────────────────────────────

const LABEL_CHARS: &[char] = &[
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's',
    't', 'u', 'v', 'w', 'x', 'y', 'z', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L',
    'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
];

/// Assign labels to `n` targets. Guarantees:
/// - Every label is unique.
/// - No label is a strict prefix of another label.
/// - If `n <= 52`: all labels are 1 char.
/// - If `n > 52`: all labels are 2 chars (capacity 52*52 = 2704).
///
/// Pure 1-char or pure 2-char ensures prefix-freedom within each scheme.
pub(crate) fn assign_labels(n: usize) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    let alpha = LABEL_CHARS;
    let len = alpha.len(); // 52
    if n <= len {
        // All 1-char labels: a, b, c, ..., z, A, B, ..., Z
        alpha[..n].iter().map(|c| c.to_string()).collect()
    } else {
        // All 2-char labels: aa, ab, ..., az, aA, ..., zZ, Aa, ...
        // Capacity = 52*52 = 2704 targets.
        let mut labels = Vec::with_capacity(n);
        'outer: for &first in alpha {
            for &second in alpha {
                if labels.len() >= n {
                    break 'outer;
                }
                labels.push(format!("{first}{second}"));
            }
        }
        labels
    }
}

// ── Target computation ──────────────────────────────────────────────────────

/// Compute the first-non-blank column on line `row` from `line_str`.
/// Returns 0 if the line is blank (all whitespace).
fn first_non_blank(line_str: &str) -> usize {
    line_str
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(byte_idx, _)| line_str[..byte_idx].chars().count())
        .unwrap_or(0)
}

/// `CharKind` for the hop word-start scanner (mirrors motions.rs logic).
#[derive(PartialEq, Eq)]
enum CharKind {
    Word,
    Punct,
    Space,
}

fn char_kind(c: char, iskeyword: &str) -> CharKind {
    if c.is_whitespace() {
        CharKind::Space
    } else if is_keyword_char(c, iskeyword) {
        CharKind::Word
    } else {
        CharKind::Punct
    }
}

/// Collect all word-start column indices on `line_str` using vim `w` semantics.
/// A word start is:
/// - col 0, if the char there is non-blank (Word or Punct), OR
/// - any position where the kind transitions from Space (or from a different
///   non-Space kind) into a new non-Space kind.
///
/// Mirrors the `next_word_start` / `char_kind` logic in motions.rs but
/// operates on a pre-decoded line string.
fn word_starts(line_str: &str, iskeyword: &str) -> Vec<usize> {
    let mut targets = Vec::new();
    let chars: Vec<char> = line_str.chars().collect();
    let n = chars.len();
    if n == 0 {
        return targets;
    }
    // col 0: word start if non-blank
    if !chars[0].is_whitespace() {
        targets.push(0);
    }
    // Transition detection: new non-blank run after blank, or kind change.
    for i in 1..n {
        let prev_kind = char_kind(chars[i - 1], iskeyword);
        let cur_kind = char_kind(chars[i], iskeyword);
        if cur_kind != CharKind::Space && (prev_kind == CharKind::Space || prev_kind != cur_kind) {
            targets.push(i);
        }
    }
    targets
}

/// Collect WORD-start column indices (whitespace-delimited runs).
/// A WORD start is col 0 if non-blank, or any non-space preceded by space.
fn big_word_starts(line_str: &str) -> Vec<usize> {
    let mut targets = Vec::new();
    let chars: Vec<char> = line_str.chars().collect();
    let n = chars.len();
    if n == 0 {
        return targets;
    }
    if !chars[0].is_whitespace() {
        targets.push(0);
    }
    for i in 1..n {
        if !chars[i].is_whitespace() && chars[i - 1].is_whitespace() {
            targets.push(i);
        }
    }
    targets
}

// ── App implementation ──────────────────────────────────────────────────────

impl App {
    /// Compute raw (row, col) hop targets for the visible viewport of `win`.
    fn hop_targets(&self, win: WindowId, kind: HopKind) -> Vec<(usize, usize)> {
        let editor = self.window_editor(win);
        let vp = editor.host().viewport();
        let top = vp.top_row;
        let height = vp.height as usize;

        let rope = editor.buffer().rope();
        let total_lines = rope.len_lines();
        // Last visible row (exclusive)
        let bot = (top + height).min(total_lines);

        let iskeyword = editor.settings().iskeyword.clone();
        let (cursor_row, _) = editor.cursor();

        let mut results = Vec::new();

        for row in top..bot {
            let line_str = hjkl_buffer::rope_line_str(&rope, row);
            // Strip the trailing newline so col indices are char-accurate.
            let line_str = line_str.trim_end_matches('\n');

            match kind {
                HopKind::Word => {
                    for col in word_starts(line_str, &iskeyword) {
                        results.push((row, col));
                    }
                }
                HopKind::WordCap => {
                    for col in big_word_starts(line_str) {
                        results.push((row, col));
                    }
                }
                HopKind::LineBelow => {
                    if row > cursor_row {
                        let col = first_non_blank(line_str);
                        // Only include non-blank lines (skip empty lines).
                        if !line_str.is_empty() {
                            results.push((row, col));
                        }
                    }
                }
                HopKind::LineAbove => {
                    if row < cursor_row {
                        let col = first_non_blank(line_str);
                        if !line_str.is_empty() {
                            results.push((row, col));
                        }
                    }
                }
            }
        }

        results
    }

    /// Start the hop overlay for the given kind.
    ///
    /// Captures visual-mode state, computes targets in the visible viewport,
    /// assigns labels, and activates the overlay. No-op when there are no
    /// targets. (Operator-pending hop is intentionally not supported: `d<Space>`
    /// is vim's delete-char, owned by the engine, not a hop trigger.)
    pub(crate) fn start_hop(&mut self, kind: HopKind) {
        let win = self.focused_window();

        // Capture visual state before any mutable borrows.
        let visual = matches!(
            self.active_editor().vim_mode(),
            VimMode::Visual | VimMode::VisualLine | VimMode::VisualBlock
        );

        // Compute raw positions.
        let raw = self.hop_targets(win, kind);

        if raw.is_empty() {
            return;
        }

        // Assign labels.
        let labels = assign_labels(raw.len());
        let targets: Vec<HopTarget> = raw
            .into_iter()
            .zip(labels)
            .map(|((row, col), label)| HopTarget { row, col, label })
            .collect();

        self.hop = Some(HopState {
            win_id: win,
            targets,
            typed: String::new(),
            visual,
        });
        self.pending_recompute = true;
    }

    /// Handle a keypress while the hop overlay is active.
    ///
    /// - `esc = true` → cancel: restore cursor, clear pending op if any.
    /// - `ch = Some(c)` → type label char; resolve when unique match found.
    /// - `ch = None, esc = false` → non-char key cancels.
    pub(crate) fn hop_handle_key(&mut self, ch: Option<char>, esc: bool) {
        if esc || ch.is_none() {
            // Cancel hop — cursor unchanged.
            self.hop = None;
            self.pending_recompute = true;
            return;
        }

        let c = ch.unwrap();

        // Push typed char and check for matches.
        if let Some(h) = self.hop.as_mut() {
            h.typed.push(c);
        }

        let (typed, targets_snap) = {
            let h = self.hop.as_ref().unwrap();
            (h.typed.clone(), h.targets.clone())
        };

        // Partition: still-possible = label starts with typed.
        let possible: Vec<&HopTarget> = targets_snap
            .iter()
            .filter(|t| t.label.starts_with(&typed))
            .collect();

        // Exact match(es): label == typed.
        let exact: Vec<&HopTarget> = possible
            .iter()
            .copied()
            .filter(|t| t.label == typed)
            .collect();

        if possible.is_empty() {
            // No match at all → cancel.
            self.hop = None;
            self.pending_recompute = true;
            return;
        }

        if exact.len() == 1 && (exact[0].label.len() == typed.len()) {
            // Unique resolve.
            let target_row = exact[0].row;
            let target_col = exact[0].col;
            let h = self.hop.take().unwrap();
            self.resolve_hop(h, target_row, target_col);
            return;
        }

        // Still waiting for more chars (possible.len() > 1 or no exact yet).
        self.pending_recompute = true;
    }

    /// Execute the hop jump to `(target_row, target_col)`. In Normal mode this
    /// moves the cursor; in Visual mode the engine stays in visual so the anchor
    /// is preserved and the selection extends to the target.
    fn resolve_hop(&mut self, _h: HopState, target_row: usize, target_col: usize) {
        self.active_editor_mut().jump_cursor(target_row, target_col);
        self.sync_after_engine_mutation();
        self.pending_recompute = true;
    }
}
