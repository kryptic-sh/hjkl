use hjkl_engine::{Host, Query};
use std::time::{Duration, Instant};

use super::App;

impl App {
    /// Recompute git diff signs from the current buffer content (vs
    /// the HEAD blob) when `dirty_gen` has advanced since the last rebuild.
    pub(crate) fn refresh_git_signs(&mut self) {
        self.refresh_git_signs_inner(false);
    }

    pub(crate) fn refresh_git_signs_force(&mut self) {
        self.refresh_git_signs_inner(true);
    }

    pub(crate) fn refresh_git_signs_inner(&mut self, force: bool) {
        const HUGE_FILE_LINES: u32 = 50_000;
        const REFRESH_MIN_INTERVAL: Duration = Duration::from_millis(250);

        let path = match self.active().filename.as_deref() {
            Some(p) => p.to_path_buf(),
            None => {
                let slot = self.active_mut();
                slot.git_signs.clear();
                slot.last_git_dirty_gen = None;
                return;
            }
        };
        let dg = self.active().editor.buffer().dirty_gen();
        if !force && self.active().last_git_dirty_gen == Some(dg) {
            return;
        }
        if !force && self.active().editor.buffer().line_count() >= HUGE_FILE_LINES {
            return;
        }
        let now = Instant::now();
        if !force && now.duration_since(self.active().last_git_refresh_at) < REFRESH_MIN_INTERVAL {
            return;
        }

        let lines = self.active().editor.buffer().lines();
        let mut bytes = lines.join("\n").into_bytes();
        if !bytes.is_empty() {
            bytes.push(b'\n');
        }
        let git_signs = crate::git::signs_for_bytes(&path, &bytes);
        let is_untracked = crate::git::is_untracked(&path);
        let slot = self.active_mut();
        slot.git_signs = git_signs;
        slot.is_untracked = is_untracked;
        slot.last_git_dirty_gen = Some(dg);
        slot.last_git_refresh_at = now;
    }

    /// Submit a new viewport-scoped parse on the syntax worker and install
    /// whatever the worker has produced since the last frame.
    pub(crate) fn recompute_and_install(&mut self) {
        const RECOMPUTE_THROTTLE: Duration = Duration::from_millis(100);
        let buffer_id = self.active().buffer_id;
        let (top, height) = {
            let vp = self.active().editor.host().viewport();
            (vp.top_row, vp.height as usize)
        };
        let dg = self.active().editor.buffer().dirty_gen();
        let key = (dg, top, height);

        let prev_dirty_gen = self
            .active()
            .last_recompute_key
            .map(|(prev_dg, _, _)| prev_dg);

        let t_total = Instant::now();
        let mut submitted = false;
        if self.active().last_recompute_key == Some(key) {
            self.recompute_hits = self.recompute_hits.saturating_add(1);
        } else {
            let buffer_changed = self
                .active()
                .last_recompute_key
                .map(|(prev_dg, _, _)| prev_dg != dg)
                .unwrap_or(true);
            let now = Instant::now();
            if buffer_changed
                && now.duration_since(self.active().last_recompute_at) < RECOMPUTE_THROTTLE
            {
                self.recompute_throttled = self.recompute_throttled.saturating_add(1);
            } else {
                self.recompute_runs = self.recompute_runs.saturating_add(1);
                // Split borrow: get a raw pointer to the buffer so `self.syntax`
                // can be borrowed mutably without fighting the borrow checker on
                // `self.slots`. Safety: the buffer lives inside `self.slots[active]`
                // which is not touched inside `submit_render`.
                let submit_result = {
                    let buf = self.slots[self.active].editor.buffer();
                    self.syntax.submit_render(buffer_id, buf, top, height)
                };
                if submit_result.is_some() {
                    submitted = true;
                    self.active_mut().last_recompute_at = Instant::now();
                    self.active_mut().last_recompute_key = Some(key);
                }
            }
        }

        let t_install = Instant::now();
        let drained = if submitted {
            let viewport_only = prev_dirty_gen == Some(dg);
            if viewport_only {
                self.syntax.wait_result(Duration::from_millis(5))
            } else {
                self.syntax.take_result()
            }
        } else {
            self.syntax.take_result()
        };
        if let Some(out) = drained {
            self.active_mut()
                .editor
                .install_ratatui_syntax_spans(out.spans);
            self.active_mut().diag_signs = out.signs;
            self.last_install_us = t_install.elapsed().as_micros();
        } else {
            self.last_install_us = 0;
        }
        self.last_perf = self.syntax.last_perf;

        let t_git = Instant::now();
        self.refresh_git_signs();
        self.last_git_us = t_git.elapsed().as_micros();
        self.last_recompute_us = t_total.elapsed().as_micros();
        let _ = submitted;
    }
}
