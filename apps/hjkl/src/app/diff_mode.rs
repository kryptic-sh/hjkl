//! Diff mode (#208 Phase 2) — window diff-group management + alignment cache.
//!
//! `:diffthis` adds the focused window to the diff group; `:diffsplit {file}`
//! opens a file in a vertical split and diffs both; `:diffoff` removes the
//! focused window (and tears the group down once fewer than two remain).
//!
//! Highlighting engages only with **two** distinct-buffer windows. The pair's
//! line alignment ([`hjkl_app::diff::align_lines`]) is cached in
//! [`App::diff_cache`] and recomputed lazily by [`App::refresh_diff_alignment`]
//! whenever the participating windows or either buffer's `dirty_gen` change.

use std::collections::HashMap;
use std::ops::Range;

use super::{App, DiffCacheEntry};
use crate::app::window::WindowId;
use hjkl_app::diff::DiffRowKind;

/// Per-line diff classification for one window, consumed by the renderer.
pub(crate) enum DiffBand {
    /// Line exists only in this buffer (vim `DiffAdd`).
    Add,
    /// Line exists on both sides but differs (vim `DiffChange`); `text_ranges`
    /// carry the changed byte spans (vim `DiffText`).
    Change,
}

/// One changed/added line's render classification.
pub(crate) struct DiffLineClass {
    /// Whole-line band color class.
    pub band: DiffBand,
    /// Byte ranges within the line that differ (for `Change` rows only).
    pub text_ranges: Vec<Range<usize>>,
}

impl App {
    /// `:diffthis` — mark the focused window as part of the diff group.
    pub(crate) fn diff_this(&mut self) {
        let win = self.focused_window();
        if !self.diff_windows.contains(&win) {
            self.diff_windows.push(win);
        }
        self.refresh_diff_alignment();
        if self.diff_pair().is_none() {
            self.bus
                .info("diff: marked — open another window with :diffthis to compare");
        }
    }

    /// `:diffoff` — remove the focused window from the diff group. With fewer
    /// than two windows left the group is disbanded (nothing to compare).
    pub(crate) fn diff_off(&mut self) {
        let win = self.focused_window();
        self.diff_windows.retain(|&w| w != win);
        if self.diff_windows.len() < 2 {
            self.diff_windows.clear();
        }
        self.diff_cache = None;
        self.refresh_diff_alignment();
    }

    /// `:diffsplit {file}` — open `{file}` in a vertical split and diff it
    /// against the current window.
    pub(crate) fn diff_split(&mut self, arg: &str) {
        if arg.is_empty() {
            self.bus.error("E471: Argument required");
            return;
        }
        let original = self.focused_window();
        // Reuse the existing vsplit-with-file plumbing.
        self.do_vsplit(arg.trim());
        let opened = self.focused_window();
        if opened == original {
            // vsplit failed (e.g. file open error already reported) — nothing
            // to diff against.
            return;
        }
        self.diff_windows.clear();
        self.diff_windows.push(original);
        self.diff_windows.push(opened);
        self.refresh_diff_alignment();
    }

    /// The first two still-open, distinct-buffer windows in the diff group, in
    /// insertion order (`a` then `b`). `None` when fewer than two qualify.
    pub(crate) fn diff_pair(&self) -> Option<(WindowId, WindowId)> {
        let open: Vec<WindowId> = self
            .diff_windows
            .iter()
            .copied()
            .filter(|&w| self.windows.get(w).map(|o| o.is_some()).unwrap_or(false))
            .collect();
        if open.len() < 2 {
            return None;
        }
        let a = open[0];
        let a_slot = self.windows[a].as_ref().unwrap().slot;
        // First subsequent window pointing at a different slot.
        let b = open[1..]
            .iter()
            .copied()
            .find(|&w| self.windows[w].as_ref().unwrap().slot != a_slot)?;
        Some((a, b))
    }

    /// `true` when `win` participates in the active diff pair.
    pub(crate) fn is_diff_window(&self, win: WindowId) -> bool {
        matches!(self.diff_pair(), Some((a, b)) if win == a || win == b)
    }

    /// Materialize a slot's buffer to a `String` (rope chunks joined).
    fn slot_text(&self, slot: usize) -> String {
        self.slots[slot].editor.buffer().rope().to_string()
    }

    /// A single line's text (newline stripped) from a slot's buffer.
    fn line_text(&self, slot: usize, idx: usize) -> String {
        let rope = self.slots[slot].editor.buffer().rope();
        hjkl_buffer::rope_line_str(&rope, idx)
            .trim_end_matches('\n')
            .to_string()
    }

    /// Per-line diff classification for `win` (a diff-pair member), keyed by
    /// buffer line index. Equal lines are omitted. For `Change` rows the
    /// character-level differing byte ranges are computed for this side.
    ///
    /// This is the no-filler classification (band + DiffText); filler rows for
    /// lines that exist only in the *other* buffer are a separate render
    /// concern.
    pub(crate) fn diff_line_classes(&self, win: WindowId) -> HashMap<usize, DiffLineClass> {
        let mut out = HashMap::new();
        let Some(cache) = self.diff_cache.as_ref() else {
            return out;
        };
        let is_a = win == cache.a_win;
        let is_b = win == cache.b_win;
        if !is_a && !is_b {
            return out;
        }
        let (Some(a_slot), Some(b_slot)) = (
            self.windows[cache.a_win].as_ref().map(|w| w.slot),
            self.windows[cache.b_win].as_ref().map(|w| w.slot),
        ) else {
            return out;
        };
        for row in &cache.diff.rows {
            match row.kind {
                DiffRowKind::Equal => {}
                DiffRowKind::Change => {
                    let (Some(ai), Some(bi)) = (row.a, row.b) else {
                        continue;
                    };
                    let a_line = self.line_text(a_slot, ai);
                    let b_line = self.line_text(b_slot, bi);
                    let (ar, br) = hjkl_app::diff::char_ranges(&a_line, &b_line);
                    let (line, text_ranges) = if is_a { (ai, ar) } else { (bi, br) };
                    out.insert(
                        line,
                        DiffLineClass {
                            band: DiffBand::Change,
                            text_ranges,
                        },
                    );
                }
                DiffRowKind::Delete => {
                    // Exists only on the `a` side → DiffAdd in the `a` window.
                    if is_a && let Some(ai) = row.a {
                        out.insert(
                            ai,
                            DiffLineClass {
                                band: DiffBand::Add,
                                text_ranges: Vec::new(),
                            },
                        );
                    }
                }
                DiffRowKind::Insert => {
                    // Exists only on the `b` side → DiffAdd in the `b` window.
                    if is_b && let Some(bi) = row.b {
                        out.insert(
                            bi,
                            DiffLineClass {
                                band: DiffBand::Add,
                                text_ranges: Vec::new(),
                            },
                        );
                    }
                }
            }
        }
        out
    }

    /// Buffer-line indices (this side) where each change hunk begins, sorted
    /// ascending. A hunk is a maximal run of non-`Equal` aligned rows; its
    /// representative line is the first real this-side line at or after the run
    /// start (so a pure-filler hunk attributes to the line after the gap).
    fn diff_change_starts(&self, win: WindowId) -> Vec<usize> {
        let mut starts = Vec::new();
        let Some(cache) = self.diff_cache.as_ref() else {
            return starts;
        };
        let is_a = win == cache.a_win;
        if !is_a && win != cache.b_win {
            return starts;
        }
        let side = |r: &hjkl_app::diff::AlignedRow| if is_a { r.a } else { r.b };
        let rows = &cache.diff.rows;
        let mut prev_change = false;
        for (i, row) in rows.iter().enumerate() {
            let is_change = row.kind != DiffRowKind::Equal;
            if is_change && !prev_change {
                // First real this-side line from here onward (covers filler-only
                // hunks, which resolve to the following real line).
                if let Some(line) = rows[i..].iter().find_map(side)
                    && starts.last() != Some(&line)
                {
                    starts.push(line);
                }
            }
            prev_change = is_change;
        }
        starts
    }

    /// `]c` — jump the focused window's cursor to the next change hunk.
    pub(crate) fn diff_next_change(&mut self) {
        let win = self.focused_window();
        if !self.is_diff_window(win) {
            self.bus.info("not in diff mode");
            return;
        }
        let (row, _) = self.active().editor.cursor();
        let starts = self.diff_change_starts(win);
        if let Some(&target) = starts.iter().find(|&&l| l > row) {
            self.jump_diff_cursor(target);
        } else {
            self.bus.info("no more changes below");
        }
    }

    /// `[c` — jump the focused window's cursor to the previous change hunk.
    pub(crate) fn diff_prev_change(&mut self) {
        let win = self.focused_window();
        if !self.is_diff_window(win) {
            self.bus.info("not in diff mode");
            return;
        }
        let (row, _) = self.active().editor.cursor();
        let starts = self.diff_change_starts(win);
        if let Some(&target) = starts.iter().rev().find(|&&l| l < row) {
            self.jump_diff_cursor(target);
        } else {
            self.bus.info("no more changes above");
        }
    }

    /// Move the active cursor to the start of `line` and keep it on-screen.
    fn jump_diff_cursor(&mut self, line: usize) {
        self.active_mut().editor.jump_cursor(line, 0);
        self.active_mut().editor.ensure_cursor_in_scrolloff();
        self.sync_after_engine_mutation();
    }

    /// Recompute the cached alignment if the diff pair or either buffer changed.
    /// Cheap no-op when nothing relevant moved (gen-keyed).
    pub(crate) fn refresh_diff_alignment(&mut self) {
        let Some((a_win, b_win)) = self.diff_pair() else {
            self.diff_cache = None;
            return;
        };
        let a_slot = self.windows[a_win].as_ref().unwrap().slot;
        let b_slot = self.windows[b_win].as_ref().unwrap().slot;
        let a_gen = self.slots[a_slot].editor.buffer().dirty_gen();
        let b_gen = self.slots[b_slot].editor.buffer().dirty_gen();
        if let Some(c) = &self.diff_cache
            && c.a_win == a_win
            && c.b_win == b_win
            && c.a_gen == a_gen
            && c.b_gen == b_gen
        {
            return;
        }
        let a_text = self.slot_text(a_slot);
        let b_text = self.slot_text(b_slot);
        let diff = hjkl_app::diff::align_lines(&a_text, &b_text);
        self.diff_cache = Some(DiffCacheEntry {
            a_win,
            b_win,
            a_gen,
            b_gen,
            diff,
        });
    }
}
