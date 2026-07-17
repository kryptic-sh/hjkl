//! Quickfix-list / location-list host integration (#184): `:grep` / `:make`
//! population, the `:copen`/`:lopen` bottom-dock lifecycle (#63 Phase B), and
//! jump-to-entry. The agnostic list + cursor live in the `hjkl-quickfix`
//! crate; ex commands arrive as `ExEffect::Quickfix(QfCommand)` (global list)
//! or `ExEffect::Location(...)` (window-local list). Both lists share this
//! machinery via [`QfWhich`].
//!
//! `:copen`/`:lopen` used to show a `Clear`+`List` overlay with hardcoded key
//! interception (`j`/`k`/`<CR>`/`Esc`/`q`) that owned every keypress while up.
//! Phase B replaces that with a REAL window/buffer in `App::bottom_dock` (see
//! `crate::app::dock`): a real `Editor` backs it, so every vim motion,
//! search, and yank works on the list for free. The only key this module
//! still intercepts is `<CR>` (jump to the entry under the dock's cursor,
//! [`App::qf_dock_jump_at_cursor`]) — everything else (including `j`/`k`
//! navigation and `q`, which vim's real quickfix window does NOT close on)
//! falls straight through to the engine.

use hjkl_buffer::rope_line_str;
use hjkl_engine_tui::{EditorRatatuiExt, style_to_ratatui};
use hjkl_ex::QfCommand;
use hjkl_quickfix::{QfEntry, QfKind, QfList};
use ratatui::style::Style as RStyle;

use crate::app::types::DiagSeverity;

/// Which list a quickfix action targets: the global quickfix list (`:c*`) or
/// the location list (`:l*`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum QfWhich {
    Quickfix,
    Location,
}

impl QfWhich {
    /// Human label used in toasts and the dock title.
    fn label(self) -> &'static str {
        match self {
            QfWhich::Quickfix => "quickfix",
            QfWhich::Location => "location",
        }
    }

    /// The bottom-dock [`DockKind`](crate::app::dock::DockKind) this list
    /// shows when open (#63 Phase B).
    fn dock_kind(self) -> crate::app::dock::DockKind {
        match self {
            QfWhich::Quickfix => crate::app::dock::DockKind::Quickfix,
            QfWhich::Location => crate::app::dock::DockKind::Loclist,
        }
    }
}

/// Resolve the program + argument vector for `:make`.
///
/// The executable is always the first whitespace-delimited token of `makeprg`;
/// the user's `:make` args (`extra`) are only appended as trailing arguments.
/// This means `extra` can add flags/targets (matching vim's `:make {args}`),
/// but can never promote itself to the program that runs — not even when
/// `makeprg` is empty. Returns `None` when `makeprg` contains no program token.
///
/// The caller runs the result via `Command::args` (argv, no shell), so there
/// is also no shell-metacharacter injection from either `makeprg` or `extra`.
fn resolve_make_argv(makeprg: &str, extra: &str) -> Option<(String, Vec<String>)> {
    let mut make_tokens = makeprg.split_whitespace().map(str::to_string);
    let program = make_tokens.next()?;
    let mut rest: Vec<String> = make_tokens.collect();
    rest.extend(extra.split_whitespace().map(str::to_string));
    Some((program, rest))
}

impl crate::app::App {
    // ── list accessors ──────────────────────────────────────────────────────

    fn qf_list(&self, w: QfWhich) -> &QfList {
        match w {
            QfWhich::Quickfix => &self.quickfix,
            QfWhich::Location => &self.loclist,
        }
    }

    fn qf_list_mut(&mut self, w: QfWhich) -> &mut QfList {
        match w {
            QfWhich::Quickfix => &mut self.quickfix,
            QfWhich::Location => &mut self.loclist,
        }
    }

    /// `true` while the bottom dock is showing the quickfix list (#63 Phase
    /// B). Derived from `bottom_dock`'s kind rather than a separately-tracked
    /// bool — the dock's presence/kind now IS the "list is shown" state, so
    /// there's nothing left to go stale against it.
    pub(crate) fn quickfix_open(&self) -> bool {
        self.bottom_dock
            .as_ref()
            .is_some_and(|d| d.kind == crate::app::dock::DockKind::Quickfix)
    }

    /// `true` while the bottom dock is showing the location list. Twin of
    /// [`Self::quickfix_open`].
    pub(crate) fn loclist_open(&self) -> bool {
        self.bottom_dock
            .as_ref()
            .is_some_and(|d| d.kind == crate::app::dock::DockKind::Loclist)
    }

    /// Open (or close) the bottom dock showing list `w` (#63 Phase B).
    ///
    /// `open = true`: install the dock if it doesn't exist yet, or — if it's
    /// currently showing the OTHER list — reuse the same window/slot and just
    /// retarget which list it displays (vim shows one quickfix-style window
    /// at a time in practice; this mirrors that instead of stacking two
    /// docks). Either way the buffer is rebuilt from the current list content
    /// and the dock receives focus, matching vim's `:copen`/`:lopen`.
    ///
    /// `open = false`: closes the dock, but ONLY if list `w` currently owns
    /// it — closing quickfix must not accidentally close a loclist dock a
    /// moment after `:lopen` reused it, and vice versa.
    fn qf_set_open(&mut self, w: QfWhich, open: bool) {
        if open {
            self.open_bottom_dock_for(w);
        } else if self.qf_dock_shows(w) {
            self.close_bottom_dock();
        }
    }

    /// `true` when the bottom dock is currently showing list `w`.
    fn qf_dock_shows(&self, w: QfWhich) -> bool {
        match w {
            QfWhich::Quickfix => self.quickfix_open(),
            QfWhich::Location => self.loclist_open(),
        }
    }

    /// Install-or-reuse the bottom dock for list `w`, rebuild its buffer, and
    /// focus it. See [`Self::qf_set_open`] for the reuse contract.
    fn open_bottom_dock_for(&mut self, w: QfWhich) {
        let kind = w.dock_kind();
        self.sync_viewport_from_editor();
        let win_id = match self.bottom_dock.as_ref().map(|d| d.kind) {
            Some(k) if k == kind => self.bottom_dock.as_ref().unwrap().win_id,
            Some(_) => {
                let win_id = self.bottom_dock.as_ref().unwrap().win_id;
                if let Some(d) = self.bottom_dock.as_mut() {
                    d.kind = kind;
                }
                win_id
            }
            None => self.new_qf_dock_slot(kind),
        };
        self.set_focused_window(win_id);
        self.reconcile_window_editors();
        self.sync_viewport_to_editor();
        self.qf_rebuild_dock_buffer(w);
    }

    /// Build a fresh scratch/readonly [`BufferSlot`](super::BufferSlot) and
    /// install it as the bottom dock (#63 Phase B — mirrors
    /// `explorer::open_explorer`'s slot construction). Does NOT touch focus —
    /// [`Self::open_bottom_dock_for`] sequences that uniformly across the
    /// "already open" / "retarget" / "create" cases.
    fn new_qf_dock_slot(
        &mut self,
        kind: crate::app::dock::DockKind,
    ) -> crate::app::window::WindowId {
        use hjkl_buffer::View;
        use hjkl_engine::Settings;
        use std::time::Instant;

        let buffer_id = self.next_buffer_id;
        self.next_buffer_id += 1;

        // Read-only, no filename, no line numbers/signs — matches vim's
        // quickfix-window presentation. `filetype = "qf"` mirrors vim's
        // `&filetype` for the quickfix buffer (ftplugins / statusline
        // detection can key off it later).
        //
        // BOTH `readonly` AND `modifiable = false` are set, matching vim's
        // real quickfix window exactly (`&readonly` + `&nomodifiable`):
        // `readonly` alone only gates `:w` (E45) in this engine —
        // `Editor::mutate_edit` explicitly does NOT check it (see its doc
        // comment) — so `modifiable = false` is what actually makes x/dd/i
        // no-ops here (`nomodifiable` silently refuses insert/replace entry
        // and swallows every edit funnel, per `vim::comment` /
        // `Editor::mutate_edit`). Unlike the explorer slot (intentionally
        // modifiable for oil.nvim-style renaming), the qf dock must reject
        // ALL edits — there's no fs-transaction path backing it.
        let settings = Settings {
            filetype: "qf".to_string(),
            number: false,
            relativenumber: false,
            signcolumn: hjkl_engine::types::SignColumnMode::No,
            cursorline: true,
            foldcolumn: 0,
            readonly: true,
            modifiable: false,
            ..Settings::default()
        };

        let slot = super::BufferSlot {
            buffer_id,
            is_explorer: false,
            features: super::BufferFeatures {
                syntax: false,
                lsp: false,
                hover: false,
                end_of_buffer: false,
            },
            view: View::new(),
            settings,
            filename: None,
            dirty: false,
            is_new_file: false,
            is_untracked: false,
            diag_signs: Vec::new(),
            diag_signs_lsp: Vec::new(),
            lsp_diags: Vec::new(),
            last_lsp_dirty_gen: None,
            git_signs: Vec::new(),
            last_git_dirty_gen: None,
            last_git_refresh_at: Instant::now(),
            blame: Vec::new(),
            last_blame_dirty_gen: None,
            last_blame_refresh_at: Instant::now(),
            saved_hash: 0,
            saved_len: 0,
            signature_cache: None,
            disk_mtime: None,
            disk_len: None,
            disk_state: super::DiskState::Synced,
            swap_path: None,
            last_swap_dirty_gen: None,
            last_fold_dirty_gen: None,
            git_repo_present: None,
            commit_ctx: None,
        };
        self.slots.push(slot);
        let slot_idx = self.slots.len() - 1;
        self.install_bottom_dock(slot_idx, kind)
    }

    /// Rebuild the bottom dock's buffer text from list `w`'s current entries,
    /// one line per entry in vim's quickfix-window format:
    /// `path|row col N| message` (1-based row/col, matching what the user
    /// sees in the editor — `QfEntry::row`/`col` are stored 0-based). No-op
    /// when the dock isn't currently showing `w` (a stale rebuild call after
    /// the dock switched lists or closed).
    ///
    /// Called on every dock open/reuse AND on every list mutation while the
    /// dock is open (`:grep`/`:make`/`:cexpr`/.../`:colder`/`:cnewer` all
    /// route through here via `qf_set_open`/`qf_refresh_dock_if_open`) so the
    /// buffer never shows stale content. Bypasses the readonly guard directly
    /// (same pattern as `explorer::explorer_rebuild_buffer`) since this is a
    /// structural content reset, not a user edit.
    fn qf_rebuild_dock_buffer(&mut self, w: QfWhich) {
        let Some(dock) = self.bottom_dock.as_ref() else {
            return;
        };
        if dock.kind != w.dock_kind() {
            return;
        }
        let win_id = dock.win_id;
        let Some(slot_idx) = self
            .windows
            .get(win_id)
            .and_then(|win| win.as_ref())
            .map(|win| win.slot)
        else {
            return;
        };

        let layouts = qf_row_layouts(self.qf_list(w));
        let text = layouts
            .iter()
            .map(|r| r.line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        // Computed BEFORE the content mutation below: highlighting reads
        // `self.directory` / `self.preview_highlighters` / `self.theme`
        // (immutable), not the slot being rebuilt.
        let spans = self.qf_dock_spans(self.qf_list(w).entries(), &layouts);

        self.slots[slot_idx].set_content(&text);
        let _ = self.slots[slot_idx].take_content_edits();
        let _ = self.slots[slot_idx].take_content_reset();

        // Direct install into the dock's one window — no worker pipeline
        // can race this: `recompute_and_install` only ever touches the
        // FOCUSED slot, and every qf/loclist dock slot is created with
        // `features.syntax = false` (see `new_qf_dock_slot`), so even a
        // focused dock short-circuits there before it would install
        // anything. See the module doc for the full argument.
        if let Some(editor) = self.window_editors.get_mut(&win_id) {
            editor.install_ratatui_syntax_spans(spans);
        }

        let cursor_row = self.qf_list(w).cursor();
        if let Some(editor) = self.window_editors.get_mut(&win_id) {
            editor.jump_cursor(cursor_row, 0);
        }
    }

    /// Build the dock's per-row ratatui-styled spans (#63 quickfix-dock
    /// highlight upgrade): the location column's `path` part in one theme
    /// color, its `:line:col` suffix (plus alignment padding) in a dimmer
    /// one, the `│` separator in the border color, and — budget permitting
    /// — the target file's OWN syntax highlighting laid over the
    /// message/code column, shifted by that column's byte offset.
    ///
    /// Reuses [`Self::preview_spans_for`] — the exact function the picker
    /// preview pane already calls per visible line — so the language
    /// grammar is resolved from each entry's path the same way file
    /// previews do, and the underlying per-language `Highlighter` cache
    /// (`self.preview_highlighters`, keyed by grammar name) is shared
    /// across every entry in this rebuild AND across every other preview
    /// call this session: entries in the same language reuse one already-
    /// warm parser instead of constructing one per line.
    ///
    /// Only the first [`QF_HIGHLIGHT_BUDGET`] entries get code-column
    /// syntax spans — see its doc for why. Location-column spans (path /
    /// suffix / separator) are O(1) string-slicing per row, not a parse,
    /// so they're applied to every row regardless of the budget: entries
    /// past the cap still render the fully formatted, aligned line, just
    /// without highlighting on the message text.
    fn qf_dock_spans(
        &self,
        entries: &[QfEntry],
        layouts: &[QfRowLayout],
    ) -> Vec<Vec<(usize, usize, RStyle)>> {
        let path_style = RStyle::default().fg(self.theme.ui.text);
        let loc_style = RStyle::default().fg(self.theme.ui.non_text);
        let sep_style = RStyle::default().fg(self.theme.ui.border);

        entries
            .iter()
            .zip(layouts.iter())
            .enumerate()
            .map(|(i, (entry, layout))| {
                let mut spans: Vec<(usize, usize, RStyle)> = Vec::with_capacity(4);
                if layout.path_end > 0 {
                    spans.push((0, layout.path_end, path_style));
                }
                if layout.loc_col_end > layout.path_end {
                    spans.push((layout.path_end, layout.loc_col_end, loc_style));
                }
                let sep_end = layout.loc_col_end + QF_COL_SEP.len();
                spans.push((layout.loc_col_end, sep_end, sep_style));

                if i < QF_HIGHLIGHT_BUDGET && !entry.message.is_empty() {
                    let preview = self.preview_spans_for(&entry.path, entry.message.as_bytes());
                    if let Some(row0) = preview.by_row.first() {
                        for span in row0 {
                            let style = preview
                                .styles
                                .get(span.style as usize)
                                .copied()
                                .map(style_to_ratatui)
                                .unwrap_or_default();
                            spans.push((
                                layout.code_col_start + span.start_byte,
                                layout.code_col_start + span.end_byte,
                                style,
                            ));
                        }
                    }
                }
                spans
            })
            .collect()
    }

    /// If the bottom dock is currently showing list `w`, rebuild its buffer
    /// from the (just-mutated) list content. No-op — cheaply — otherwise.
    /// Used by list-population commands that don't go through
    /// `qf_set_open` (`:colder`/`:cnewer`, which never change open/closed
    /// state on their own).
    fn qf_refresh_dock_if_open(&mut self, w: QfWhich) {
        if self.qf_dock_shows(w) {
            self.qf_rebuild_dock_buffer(w);
        }
    }

    /// Move the bottom dock's cursor to list `w`'s current entry row, when
    /// the dock is open and showing `w`. Called after `:cnext`/`:cprev`/etc.
    /// move the list cursor so the dock's highlighted row (the cursor line)
    /// stays in sync with which entry the editor jumped to.
    fn qf_sync_dock_cursor(&mut self, w: QfWhich) {
        let Some(dock) = self.bottom_dock.as_ref() else {
            return;
        };
        if dock.kind != w.dock_kind() {
            return;
        }
        let win_id = dock.win_id;
        let row = self.qf_list(w).cursor();
        if let Some(editor) = self.window_editors.get_mut(&win_id) {
            editor.jump_cursor(row, 0);
        }
    }

    /// `<CR>` in the bottom dock (#63 Phase B): jump to the entry under the
    /// dock's cursor. The dock buffer is 1:1 with the list's entries (row `i`
    /// ↔ `entries()[i]`, built that way by `qf_rebuild_dock_buffer`), so the
    /// dock's own cursor row IS the target entry index — no separate
    /// row→entry mapping needed.
    ///
    /// Routes the file open through [`Self::focus_editor_window_for_open`]
    /// FIRST so the jump lands in a REGULAR window, never back into the
    /// (readonly) dock itself — matching vim: the quickfix window is never
    /// the target of the file it opens.
    pub(crate) fn qf_dock_jump_at_cursor(&mut self) {
        let Some(dock) = self.bottom_dock.as_ref() else {
            return;
        };
        let w = match dock.kind {
            crate::app::dock::DockKind::Quickfix => QfWhich::Quickfix,
            crate::app::dock::DockKind::Loclist => QfWhich::Location,
            // The bottom dock never hosts the explorer.
            crate::app::dock::DockKind::Explorer => return,
        };
        let win_id = dock.win_id;
        let row = self.window_cursor(win_id).0;
        if row < self.qf_list(w).len() {
            self.qf_list_mut(w).set_cursor(row);
        }
        self.focus_editor_window_for_open();
        self.qf_jump_to_current(w);
    }

    /// Mutable reference to the `older` stack for `w`.
    fn qf_older_mut(&mut self, w: QfWhich) -> &mut Vec<QfList> {
        match w {
            QfWhich::Quickfix => &mut self.quickfix_older,
            QfWhich::Location => &mut self.loclist_older,
        }
    }

    /// Mutable reference to the `newer` stack for `w`.
    fn qf_newer_mut(&mut self, w: QfWhich) -> &mut Vec<QfList> {
        match w {
            QfWhich::Quickfix => &mut self.quickfix_newer,
            QfWhich::Location => &mut self.loclist_newer,
        }
    }

    /// Snapshot the current list onto `older` before a SET-population
    /// (non-append) replaces it (#261 Phase 5b).
    ///
    /// - If the current list is NON-empty, push a clone onto `older`.
    /// - Cap `older` at 9 entries (oldest dropped from front) so the total
    ///   including the live list stays ≤ 10 (vim's max).
    /// - ALWAYS clear `newer`: a fresh population discards the redo branch.
    pub(crate) fn qf_push_history(&mut self, w: QfWhich) {
        // Clear newer unconditionally — fresh population kills the redo branch.
        self.qf_newer_mut(w).clear();
        // Only push non-empty lists to avoid cluttering history with no-ops.
        if !self.qf_list(w).is_empty() {
            let snapshot = self.qf_list(w).clone();
            let older = self.qf_older_mut(w);
            if older.len() >= 9 {
                older.remove(0); // drop the oldest to stay within the 9-entry cap
            }
            older.push(snapshot);
        }
    }

    // ── command dispatch ────────────────────────────────────────────────────

    /// Dispatch a quickfix ex command (`:copen`, `:cnext`, `:grep`, …).
    pub(crate) fn handle_quickfix_command(&mut self, cmd: QfCommand) {
        self.handle_qf_command(QfWhich::Quickfix, cmd);
    }

    /// Dispatch a location-list ex command (`:lopen`, `:lnext`, `:lgrep`, …).
    pub(crate) fn handle_loclist_command(&mut self, cmd: QfCommand) {
        self.handle_qf_command(QfWhich::Location, cmd);
    }

    fn handle_qf_command(&mut self, w: QfWhich, cmd: QfCommand) {
        match cmd {
            QfCommand::Grep(pat) => self.qf_run_grep(w, &pat),
            QfCommand::Make(extra) => self.qf_run_make(w, &extra),
            QfCommand::Expr { text, append, jump } => {
                self.qf_run_expr(w, &text, append, jump);
            }
            QfCommand::FromBuffer { append, jump } => {
                self.qf_run_from_buffer(w, append, jump);
            }
            QfCommand::FromFile { path, append, jump } => {
                self.qf_run_from_file(w, &path, append, jump);
            }
            QfCommand::Open => {
                if self.qf_list(w).is_empty() {
                    self.bus.info(format!("{} list is empty", w.label()));
                } else {
                    self.qf_set_open(w, true);
                }
            }
            QfCommand::Close => self.qf_set_open(w, false),
            QfCommand::Window => {
                // `:cwindow` / `:lwindow` — open only when the list has
                // entries; close when it's empty (vim `:h :cwindow`). Unlike
                // `:copen`, an empty list is silent — vim shows no message.
                let open = !self.qf_list(w).is_empty();
                self.qf_set_open(w, open);
            }
            QfCommand::Next => {
                self.qf_list_mut(w).next();
                self.qf_after_nav(w);
            }
            QfCommand::Prev => {
                self.qf_list_mut(w).prev();
                self.qf_after_nav(w);
            }
            QfCommand::First => {
                self.qf_list_mut(w).first();
                self.qf_after_nav(w);
            }
            QfCommand::Last => {
                self.qf_list_mut(w).last();
                self.qf_after_nav(w);
            }
            QfCommand::Nth(n) => {
                // `0` means "current" (bare `:cc`/`:ll`); otherwise 1-based.
                if n > 0 {
                    self.qf_list_mut(w).nth(n);
                }
                self.qf_after_nav(w);
            }
            QfCommand::Older(n) => self.qf_do_older(w, n),
            QfCommand::Newer(n) => self.qf_do_newer(w, n),
            QfCommand::Do { cmd, per_file } => self.qf_run_do(w, &cmd, per_file),
            QfCommand::Diagnostics => self.qf_run_diagnostics(w),
        }
    }

    /// `:colder [N]` / `:lolder [N]` — activate an older error list.
    ///
    /// Repeats N times (default 1): push current list onto `newer`, pop the
    /// top of `older` into current. Stops early if `older` empties.
    /// Does NOT navigate within the restored list — vim `:colder` just makes
    /// the list active; the user then uses `:cc`/`:cnext` to jump.
    fn qf_do_older(&mut self, w: QfWhich, count: usize) {
        let steps = count.max(1);
        for _ in 0..steps {
            // Check if there's anything to go back to.
            if self.qf_older_mut(w).is_empty() {
                self.bus
                    .info(format!("{} list is already the oldest", w.label()));
                break;
            }
            // Push current → newer.
            let current = self.qf_list(w).clone();
            self.qf_newer_mut(w).push(current);
            // Pop from older → current.
            let prev = self.qf_older_mut(w).pop().unwrap();
            *self.qf_list_mut(w) = prev;
        }
        let older_len = self.qf_older_mut(w).len();
        let newer_len = self.qf_newer_mut(w).len();
        // List index: current is at position (older_len + 1) out of (older_len + 1 + newer_len).
        let total = older_len + 1 + newer_len;
        let current_idx = older_len + 1;
        self.bus
            .info(format!("{} list {} of {}", w.label(), current_idx, total));
        // `:colder` doesn't open/close the dock on its own (vim just makes
        // the list active), but if it's ALREADY open on this list the
        // buffer must reflect the newly-activated list's entries.
        self.qf_refresh_dock_if_open(w);
    }

    /// `:cnewer [N]` / `:lnewer [N]` — activate a newer error list.
    ///
    /// Mirror of [`qf_do_older`]: push current list onto `older`, pop from `newer`.
    fn qf_do_newer(&mut self, w: QfWhich, count: usize) {
        let steps = count.max(1);
        for _ in 0..steps {
            if self.qf_newer_mut(w).is_empty() {
                self.bus
                    .info(format!("{} list is already the newest", w.label()));
                break;
            }
            // Push current → older.
            let current = self.qf_list(w).clone();
            self.qf_older_mut(w).push(current);
            // Pop from newer → current.
            let next = self.qf_newer_mut(w).pop().unwrap();
            *self.qf_list_mut(w) = next;
        }
        let older_len = self.qf_older_mut(w).len();
        let newer_len = self.qf_newer_mut(w).len();
        let total = older_len + 1 + newer_len;
        let current_idx = older_len + 1;
        self.bus
            .info(format!("{} list {} of {}", w.label(), current_idx, total));
        self.qf_refresh_dock_if_open(w);
    }

    /// `:cdo {cmd}` / `:cfdo {cmd}` (and `l*` variants) — run an ex command at
    /// each quickfix/location-list entry.
    ///
    /// - `per_file = false` (`:cdo`/`:ldo`): visit every entry in order.
    /// - `per_file = true` (`:cfdo`/`:lfdo`): visit only the FIRST entry for
    ///   each distinct file path, preserving original order; empty-path entries
    ///   all collapse to a single visit (matches "current buffer" semantics).
    ///
    /// The entry snapshot is taken up front so mutations caused by `cmd` (e.g.
    /// a substitute that changes buffer content) don't affect iteration order.
    /// The popup is NOT opened — vim's `:cdo` leaves the popup as-is.
    /// On completion the cursor position is wherever the last `cmd` left it,
    /// matching vim behaviour.
    fn qf_run_do(&mut self, w: QfWhich, cmd: &str, per_file: bool) {
        // Snapshot entries so the command loop iterates a stable list even if
        // the list or buffer is mutated by `cmd`.
        let entries: Vec<QfEntry> = self.qf_list(w).entries().to_vec();
        if entries.is_empty() {
            self.bus.info("no entries");
            return;
        }

        // Compute which indices to visit.
        let indices: Vec<usize> = if per_file {
            // One visit per distinct file path; preserve original order.
            let mut seen = std::collections::HashSet::new();
            entries
                .iter()
                .enumerate()
                .filter_map(|(i, e)| {
                    if seen.insert(e.path.clone()) {
                        Some(i)
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            (0..entries.len()).collect()
        };

        // Route off any special pane (e.g. the bottom dock itself) BEFORE
        // running per-entry edits — `cmd` must never execute against the
        // dock's readonly buffer just because it happened to be focused when
        // `:cdo`/`:cfdo` was invoked.
        self.focus_editor_window_for_open();
        let cmd_owned = cmd.to_string();
        for i in indices {
            self.qf_list_mut(w).set_cursor(i);
            self.qf_jump_to_current(w);
            self.dispatch_ex(&cmd_owned);
        }
    }

    /// `]q`/`[q` (and `]l`/`[l`) and the `:cnext`/`:lnext` families route here
    /// after moving the list cursor: jump the editor and, if the bottom dock
    /// is open on this list, keep its highlighted row in sync.
    fn qf_after_nav(&mut self, w: QfWhich) {
        if self.qf_list(w).is_empty() {
            self.bus.info(format!("{} list is empty", w.label()));
            return;
        }
        // Land the jump in a REGULAR window even when `:cnext`/`[q`/etc. was
        // invoked while the (readonly) dock itself was focused — matches
        // vim: the quickfix window is never the target of the file it opens.
        self.focus_editor_window_for_open();
        self.qf_jump_to_current(w);
        self.qf_sync_dock_cursor(w);
    }

    /// Open the current entry's file and place the cursor on it. No-op when the
    /// list is empty.
    ///
    /// When the entry's path is empty (no `%f` in the errorformat pattern) or
    /// equals the currently-open file, the cursor is moved within the current
    /// buffer without calling `do_edit` — matching vim's `:cexpr` behaviour for
    /// current-buffer entries.
    fn qf_jump_to_current(&mut self, w: QfWhich) {
        let Some(entry) = self.qf_list(w).current() else {
            return;
        };
        let entry_path = entry.path.clone();
        let (row, col) = (entry.row, entry.col);
        // Decide whether we need to open a different file.
        let needs_edit = if entry_path.as_os_str().is_empty() {
            // Empty path → current-buffer entry, no file switch.
            false
        } else {
            // Compare against the active buffer's filename.
            let current = self.active().filename.as_deref();
            match current {
                Some(cur) => cur != entry_path,
                None => true, // scratch buffer → open the entry's file
            }
        };
        if needs_edit {
            let path_str = entry_path.to_string_lossy().to_string();
            self.do_edit(&path_str, false);
        }
        self.active_editor_mut().jump_cursor(row, col);
        self.sync_after_engine_mutation();
    }

    // ── population: :grep / :make ───────────────────────────────────────────

    /// `:grep <pat>` / `:lgrep <pat>` — run the detected grep backend
    /// (blocking), parse hits into the target list, and open the popup. Reuses
    /// `hjkl-picker`'s rg parsers.
    fn qf_run_grep(&mut self, w: QfWhich, pat: &str) {
        use hjkl_picker::source::rg::{
            GrepBackend, detect_grep_backend, parse_grep_line, parse_rg_json_line,
        };
        const MAX_ENTRIES: usize = 10_000;
        let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let root_str = root.to_str().unwrap_or(".");
        let backend = detect_grep_backend();

        let output = match backend {
            GrepBackend::Rg => std::process::Command::new("rg")
                .args(["--json", "--no-config", "--smart-case", "--", pat, root_str])
                .output(),
            GrepBackend::Grep => std::process::Command::new("grep")
                .args(["-rnH", "--", pat, root_str])
                .output(),
            GrepBackend::Findstr => std::process::Command::new("findstr")
                // `/c:` binds the pattern as the search string so a pattern
                // starting with `/` can't be parsed as a findstr option;
                // `/r` keeps regex semantics (findstr's default).
                .args(["/s", "/n", "/r", &format!("/c:{pat}"), "*"])
                .output(),
            GrepBackend::Neither => {
                self.bus.error("no search backend found (install ripgrep)");
                return;
            }
        };
        let out = match output {
            Ok(o) => o,
            Err(e) => {
                self.bus.error(format!("grep failed: {e}"));
                return;
            }
        };

        let text = String::from_utf8_lossy(&out.stdout);
        let mut entries: Vec<QfEntry> = Vec::new();
        for line in text.lines() {
            let m = match backend {
                GrepBackend::Rg => parse_rg_json_line(line, &root),
                _ => parse_grep_line(line, &root),
            };
            if let Some(m) = m {
                entries.push(QfEntry {
                    path: m.path,
                    row: (m.line.saturating_sub(1)) as usize, // 1-based → 0-based
                    col: (m._col.saturating_sub(1)) as usize,
                    kind: QfKind::Grep,
                    message: m.text.trim_end().to_string(),
                });
                if entries.len() >= MAX_ENTRIES {
                    break;
                }
            }
        }

        let n = entries.len();
        self.qf_push_history(w);
        self.qf_list_mut(w).set(entries);
        if n == 0 {
            self.qf_set_open(w, false);
            self.bus.info(format!("no matches for \"{pat}\""));
        } else {
            self.qf_set_open(w, true);
            self.bus.info(format!("{n} matches"));
        }
    }

    /// `:make [extra]` / `:lmake [extra]` — run `makeprg` (with `extra`
    /// appended) blocking, parse stdout+stderr through the errorformat, populate
    /// the target list, and open the popup. Blocking: the TUI is frozen for the
    /// build's duration (cargo check can take seconds). Async is a follow-up.
    fn qf_run_make(&mut self, w: QfWhich, extra: &str) {
        const MAX_ENTRIES: usize = 10_000;
        let makeprg = self.active_editor().settings().makeprg.clone();
        let Some((program, rest)) = resolve_make_argv(&makeprg, extra) else {
            self.bus.error("makeprg is empty");
            return;
        };

        let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let output = std::process::Command::new(&program)
            .args(&rest)
            .current_dir(&root)
            .output();
        let out = match output {
            Ok(o) => o,
            Err(e) => {
                self.bus.error(format!("make failed: {e}"));
                return;
            }
        };

        // Compilers write diagnostics to stderr (cargo/rustc/gcc); parse both
        // streams so stdout-emitting tools also work.
        let mut text = String::from_utf8_lossy(&out.stderr).into_owned();
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&out.stdout));
        let mut entries = hjkl_quickfix::parse_make_output(&text, &root);
        entries.truncate(MAX_ENTRIES);

        let n = entries.len();
        self.qf_push_history(w);
        self.qf_list_mut(w).set(entries);
        if n == 0 {
            self.qf_set_open(w, false);
            self.bus.info(format!("{makeprg}: no errors"));
        } else {
            self.qf_set_open(w, true);
            self.bus.info(format!("{n} entries"));
        }
    }

    /// `:cexpr` / `:cgetexpr` / `:caddexpr` (and `l*` variants) — parse `text`
    /// via the current `&errorformat` and populate the target list.
    ///
    /// If `text` is a double-quoted string (starts and ends with `"`), the
    /// quotes are stripped and vimscript escape sequences are expanded:
    /// `\n`→newline, `\t`→tab, `\\`→`\`, `\"`→`"`. Otherwise `text` is used
    /// verbatim as a single line.
    ///
    /// If `append` is `true` the new entries are appended to the existing list;
    /// otherwise the list is replaced. When `jump` is `true` and the resulting
    /// list is non-empty the editor cursor is moved to the FIRST entry.
    fn qf_run_expr(&mut self, w: QfWhich, text: &str, append: bool, jump: bool) {
        let parsed = parse_expr_text(text);
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let efm = self.active_editor().settings().errorformat.clone();
        let entries = hjkl_quickfix::parse_errorformat(&parsed, &efm, &cwd);
        if append {
            let new = entries;
            self.qf_list_mut(w).extend(new);
        } else {
            self.qf_push_history(w);
            self.qf_list_mut(w).set(entries);
        }
        if jump && !self.qf_list(w).is_empty() {
            // Jump to the first entry (cursor was reset to 0 by set, or stays wherever
            // it was for append — vim's :cexpr always goes to the first entry).
            if !append {
                // set() already put cursor at 0; jump there.
            } else {
                // For append/jump (not a vim command; kept symmetric) stay at 0.
                self.qf_list_mut(w).first();
            }
            self.qf_jump_to_current(w);
        }
    }

    /// `:cbuffer` / `:cgetbuffer` / `:caddbuffer` (and `l*` variants) — parse
    /// the current buffer's text via `&errorformat` and populate the target list.
    ///
    /// The buffer text is materialized by joining all rope lines with newlines.
    /// If `append` is `true` the new entries are appended; otherwise the list is
    /// replaced. When `jump` is `true` and the resulting list is non-empty the
    /// editor cursor is moved to the FIRST entry.
    fn qf_run_from_buffer(&mut self, w: QfWhich, append: bool, jump: bool) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let efm = self.active_editor().settings().errorformat.clone();
        // Materialize the buffer text from the rope.
        let rope = self.active_editor().buffer().rope();
        let n_lines = rope.len_lines();
        let mut text = String::with_capacity(rope.len_bytes() + n_lines);
        for i in 0..n_lines {
            let line = rope_line_str(&rope, i);
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&line);
        }
        drop(rope);
        let entries = hjkl_quickfix::parse_errorformat(&text, &efm, &cwd);
        if append {
            self.qf_list_mut(w).extend(entries);
        } else {
            self.qf_push_history(w);
            self.qf_list_mut(w).set(entries);
        }
        if jump && !self.qf_list(w).is_empty() {
            if append {
                self.qf_list_mut(w).first();
            }
            self.qf_jump_to_current(w);
        }
    }

    /// `:cfile` / `:cgetfile` / `:caddfile` (and `l*` variants) — read `path`
    /// from disk, parse via `&errorformat`, and populate the target list.
    ///
    /// An empty `path` falls back to `"errors.err"` (vim's default `'errorfile'`).
    /// If `append` is `true` the new entries are appended; otherwise the list is
    /// replaced. When `jump` is `true` and the resulting list is non-empty the
    /// editor cursor is moved to the FIRST entry.
    fn qf_run_from_file(&mut self, w: QfWhich, path: &str, append: bool, jump: bool) {
        let resolved = if path.is_empty() {
            "errors.err".to_string()
        } else {
            path.to_string()
        };
        let text = match std::fs::read_to_string(&resolved) {
            Ok(s) => s,
            Err(e) => {
                self.bus.error(format!("cannot read \"{resolved}\": {e}"));
                return;
            }
        };
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let efm = self.active_editor().settings().errorformat.clone();
        let entries = hjkl_quickfix::parse_errorformat(&text, &efm, &cwd);
        if append {
            self.qf_list_mut(w).extend(entries);
        } else {
            self.qf_push_history(w);
            self.qf_list_mut(w).set(entries);
        }
        if jump && !self.qf_list(w).is_empty() {
            if append {
                self.qf_list_mut(w).first();
            }
            self.qf_jump_to_current(w);
        }
    }

    /// Replace the location list with a set of entries and open the popup.
    /// Used by LSP references (`gr`) to fill the loclist alongside the picker.
    pub(crate) fn set_loclist(&mut self, entries: Vec<QfEntry>) {
        self.qf_push_history(QfWhich::Location);
        self.loclist.set(entries);
        // Populate silently; user opens via :lopen. Closes the dock only if
        // it currently owns it — a quickfix dock left open is untouched.
        self.qf_set_open(QfWhich::Location, false);
    }

    /// `:diagnostics` / `:ldiagnostics` — populate the quickfix / location list
    /// from the LSP diagnostics stored on buffer slots.
    ///
    /// For [`QfWhich::Quickfix`]: iterates ALL non-explorer slots.
    /// For [`QfWhich::Location`]: only the ACTIVE slot.
    ///
    /// Maps [`DiagSeverity`] → [`QfKind`]:
    /// `Error → Error`, `Warning → Warning`, `Info → Info`, `Hint → Note`.
    ///
    /// Entries are sorted by `(path, row, col)` (vim convention).
    /// When the resulting list is non-empty the popup is opened; when empty
    /// the popup is closed.  No cursor jump on population.
    fn qf_run_diagnostics(&mut self, w: QfWhich) {
        let mut entries: Vec<QfEntry> = match w {
            QfWhich::Quickfix => {
                // Collect from all non-explorer slots.
                self.slots
                    .iter()
                    .filter(|s| !s.is_explorer)
                    .flat_map(|s| {
                        let path = s.filename.clone().unwrap_or_default();
                        s.lsp_diags.iter().map(move |d| QfEntry {
                            path: path.clone(),
                            row: d.start_row,
                            col: d.start_col,
                            kind: diag_severity_to_qf_kind(d.severity),
                            message: d.message.clone(),
                        })
                    })
                    .collect()
            }
            QfWhich::Location => {
                // Collect from the active slot only.
                let slot = self.active();
                let path = slot.filename.clone().unwrap_or_default();
                slot.lsp_diags
                    .iter()
                    .map(|d| QfEntry {
                        path: path.clone(),
                        row: d.start_row,
                        col: d.start_col,
                        kind: diag_severity_to_qf_kind(d.severity),
                        message: d.message.clone(),
                    })
                    .collect()
            }
        };

        // Sort by (path, row, col) — vim convention.
        entries.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then(a.row.cmp(&b.row))
                .then(a.col.cmp(&b.col))
        });

        let n = entries.len();
        self.qf_push_history(w);
        self.qf_list_mut(w).set(entries);
        if n > 0 {
            self.qf_set_open(w, true);
            self.bus.info(format!("{n} diagnostics"));
        } else {
            self.qf_set_open(w, false);
            self.bus.info("no diagnostics");
        }
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Separator glyph between the merged `path:line:col` location column and
/// the message/code column. A single space pads each side (` │ `); both are
/// plain ASCII, so its BYTE length equals its char length — no char/byte
/// conversion needed at any span-offset call site below.
const QF_COL_SEP: &str = " │ ";

/// Highlight at most this many entries' message text per dock rebuild
/// (`qf_dock_spans`'s code-column syntax pass).
///
/// Each entry's syntax parse is cheap in isolation (a single short message
/// line through an already-warm, per-language `Highlighter` — see
/// [`App::qf_dock_spans`]'s doc), but list length is unbounded: a
/// project-wide `:grep`/`:make` can produce tens of thousands of hits. This
/// cap bounds worst-case rebuild cost to a fixed number of short parses
/// regardless of list size, so `:copen`/`:cnext`/`:colder`/... on a huge
/// list can never turn into a multi-second stall. Entries past the cap
/// still render the fully formatted, aligned `path:line:col │ message`
/// line — just without syntax color on the message text; the location
/// column's own coloring (path / suffix / separator) is O(1) per row and is
/// NOT capped (see `qf_dock_spans`).
const QF_HIGHLIGHT_BUDGET: usize = 1000;

/// One dock buffer row's rendered text plus the byte offsets
/// [`App::qf_dock_spans`] needs to lay styled spans over it. Computed once
/// per rebuild by [`qf_row_layouts`] and shared by both the plain-text
/// formatter and the span builder so the two can never drift out of
/// alignment with each other.
struct QfRowLayout {
    /// The full rendered line: `{loc_padded}{QF_COL_SEP}{message}`.
    line: String,
    /// Byte offset where the `path` part of the location column ends
    /// (`[0, path_end)` is the path).
    path_end: usize,
    /// Byte offset where the location column (path + `:line:col` +
    /// alignment padding) ends — `[path_end, loc_col_end)` is the
    /// `:line:col` suffix plus any trailing pad spaces.
    loc_col_end: usize,
    /// Byte offset where the message/code column starts, i.e. right after
    /// `QF_COL_SEP`.
    code_col_start: usize,
}

/// Compute one [`QfRowLayout`] per entry in `list`: merge each entry's
/// location into a single `path:line:col` column (colon-joined, the
/// grep/compiler convention), padded so the message/code column aligns
/// vertically across the whole list. Row/col are rendered 1-based
/// (`QfEntry::row`/`col` are stored 0-based, see the doc on
/// [`hjkl_quickfix::QfEntry`]).
///
/// DELIBERATE deviation from vim: real vim's quickfix window is the jagged,
/// unaligned `path|lnum col N| text`. Alignment is a pure presentation
/// upgrade — the row↔entry mapping (`qf_dock_jump_at_cursor`) is positional,
/// so the text format carries no parsing responsibility.
///
/// Entries with an empty path (current-buffer entries, e.g. from `:cexpr`
/// with no `%f`) render as `[No Name]:line:col` so every line stays
/// non-ambiguous and non-empty.
fn qf_row_layouts(list: &QfList) -> Vec<QfRowLayout> {
    let locs: Vec<(String, String)> = list
        .entries()
        .iter()
        .map(|e| {
            let path = if e.path.as_os_str().is_empty() {
                "[No Name]".to_string()
            } else {
                e.path.display().to_string()
            };
            let loc = format!("{path}:{}:{}", e.row + 1, e.col + 1);
            (path, loc)
        })
        .collect();
    // Char counts, not byte lengths, for the padding WIDTH — paths can
    // carry non-ASCII and the column must align visually. The padding
    // itself is always plain ASCII spaces, so once its char count is
    // decided its byte length is identical (see `QF_COL_SEP`'s doc).
    let loc_w = locs
        .iter()
        .map(|(_, l)| l.chars().count())
        .max()
        .unwrap_or(0);
    locs.iter()
        .zip(list.entries())
        .map(|((path, loc), e)| {
            let pad = loc_w.saturating_sub(loc.chars().count());
            let loc_padded = format!("{loc}{:pad$}", "");
            let path_end = path.len();
            let loc_col_end = loc_padded.len();
            let code_col_start = loc_col_end + QF_COL_SEP.len();
            let line = format!("{loc_padded}{QF_COL_SEP}{}", e.message);
            QfRowLayout {
                line,
                path_end,
                loc_col_end,
                code_col_start,
            }
        })
        .collect()
}

/// Render list `list`'s entries into the bottom dock's buffer text, one line
/// per entry — see [`qf_row_layouts`] for the format. Plain-text-only
/// convenience wrapper around it (used directly by unit tests that only
/// care about the rendered text, not the span layout).
#[cfg(test)]
fn qf_format_list(list: &QfList) -> String {
    qf_row_layouts(list)
        .into_iter()
        .map(|r| r.line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Map LSP [`DiagSeverity`] to quickfix [`QfKind`].
fn diag_severity_to_qf_kind(sev: DiagSeverity) -> QfKind {
    match sev {
        DiagSeverity::Error => QfKind::Error,
        DiagSeverity::Warning => QfKind::Warning,
        DiagSeverity::Info => QfKind::Info,
        DiagSeverity::Hint => QfKind::Note,
    }
}

// ── expression text parser ──────────────────────────────────────────────────

/// Parse the argument to `:cexpr` / `:lexpr` etc.
///
/// If `text` (already trimmed by the handler) starts and ends with `"`, the
/// quotes are stripped and these vimscript escape sequences are expanded:
/// `\n`→newline, `\t`→tab, `\\`→backslash, `\"`→`"`.  Everything else is
/// passed through verbatim.
///
/// Any other form is returned as-is (a single non-quoted line).
fn parse_expr_text(text: &str) -> String {
    if text.starts_with('"') && text.ends_with('"') && text.len() >= 2 {
        let inner = &text[1..text.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(ch);
            }
        }
        out
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{QF_HIGHLIGHT_BUDGET, resolve_make_argv};
    use crate::app::App;
    use hjkl_quickfix::{QfEntry, QfKind, QfList};
    use std::path::PathBuf;

    #[test]
    fn make_argv_appends_extra_after_makeprg() {
        let (prog, args) = resolve_make_argv("cargo check", "--offline -j4").unwrap();
        assert_eq!(prog, "cargo");
        assert_eq!(args, vec!["check", "--offline", "-j4"]);
    }

    #[test]
    fn make_argv_program_always_from_makeprg_not_extra() {
        // Even a hostile `extra` cannot change which binary runs.
        let (prog, args) = resolve_make_argv("make", "/tmp/evil --do-bad").unwrap();
        assert_eq!(prog, "make", "program must come from makeprg");
        assert_eq!(args, vec!["/tmp/evil", "--do-bad"]);
    }

    #[test]
    fn make_argv_empty_makeprg_is_none_even_with_extra() {
        // With an empty makeprg, extra must NOT be promoted to the program.
        assert!(resolve_make_argv("", "/tmp/evil").is_none());
        assert!(resolve_make_argv("   ", "evilprog arg").is_none());
    }

    fn entry(path: &str, row: usize, col: usize, message: &str) -> QfEntry {
        QfEntry {
            path: PathBuf::from(path),
            row,
            col,
            kind: QfKind::Grep,
            message: message.to_string(),
        }
    }

    // ── format alignment ────────────────────────────────────────────────

    /// Two entries with different-width `path:line:col` columns must still
    /// have their `│` separator land at the SAME char index on both lines —
    /// the whole point of merging + padding the location column.
    #[test]
    fn qf_format_list_aligns_code_column_with_mixed_path_widths() {
        let mut list = QfList::new();
        list.set(vec![
            entry("a.rs", 0, 0, "short"),
            entry("a_much_longer_path/name.rs", 11, 33, "longer one"),
        ]);
        let text = super::qf_format_list(&list);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "a.rs:1:1                         │ short");
        assert_eq!(lines[1], "a_much_longer_path/name.rs:12:34 │ longer one");
        let sep0 = lines[0].find('│').expect("line 0 has a separator");
        let sep1 = lines[1].find('│').expect("line 1 has a separator");
        assert_eq!(
            sep0, sep1,
            "the │ must sit at the same byte/char index on both lines \
             (content here is pure ASCII so byte index == char index)"
        );
    }

    // ── span layout ─────────────────────────────────────────────────────

    /// The path part of the location column must be styled differently from
    /// its `:line:col` suffix, and the boundary between the two spans must
    /// land EXACTLY at the path/colon transition (not one byte off, which
    /// would bleed the wrong color onto `:` or the last path char).
    ///
    /// Uses an unrecognised extension (`.qf-test-unknown-ext`) so grammar
    /// resolution deterministically returns `GrammarRequest::Unknown`
    /// (a synchronous, network-free registry lookup) — this test is about
    /// the location-column span geometry, not code highlighting, so it must
    /// not depend on any grammar being installed/loadable in the test
    /// environment.
    #[test]
    fn qf_dock_spans_styles_path_and_loc_suffix_separately() {
        let app = App::new(None, false, None, None).unwrap();
        let entries = vec![entry(
            "src/main.qf-test-unknown-ext",
            4,
            7,
            "unused variable",
        )];
        let layouts = super::qf_row_layouts(&{
            let mut l = QfList::new();
            l.set(entries.clone());
            l
        });
        let spans = app.qf_dock_spans(&entries, &layouts);
        assert_eq!(spans.len(), 1);
        let row = &spans[0];
        // path span + loc-suffix span + separator span; no code span (the
        // extension is unrecognised, so highlighting yields nothing).
        assert_eq!(row.len(), 3, "expected exactly 3 format spans, got {row:?}");

        let path_len = "src/main.qf-test-unknown-ext".len();
        let (p_start, p_end, p_style) = row[0];
        assert_eq!((p_start, p_end), (0, path_len), "path span boundaries");
        assert_eq!(p_style.fg, Some(app.theme.ui.text));

        let (l_start, l_end, l_style) = row[1];
        assert_eq!(
            l_start, path_len,
            "loc-suffix span must start EXACTLY where the path span ends"
        );
        assert!(l_end > l_start);
        assert_eq!(l_style.fg, Some(app.theme.ui.non_text));

        let (s_start, s_end, s_style) = row[2];
        assert_eq!(s_start, l_end, "separator span must start where loc ends");
        assert_eq!(s_end - s_start, super::QF_COL_SEP.len());
        assert_eq!(s_style.fg, Some(app.theme.ui.border));
    }

    // ── budget cap ──────────────────────────────────────────────────────

    /// A list longer than [`QF_HIGHLIGHT_BUDGET`] must not panic, must
    /// format every entry, and entries past the cap must carry ONLY the
    /// format spans (path/loc/separator) — no attempt at code-column
    /// highlighting beyond the budget.
    #[test]
    fn qf_dock_spans_caps_highlighting_beyond_budget() {
        let app = App::new(None, false, None, None).unwrap();
        let n = QF_HIGHLIGHT_BUDGET + 5;
        let entries: Vec<QfEntry> = (0..n)
            .map(|i| entry("f.qf-test-unknown-ext", i, 0, "some message"))
            .collect();
        let mut list = QfList::new();
        list.set(entries.clone());
        let layouts = super::qf_row_layouts(&list);

        let spans = app.qf_dock_spans(&entries, &layouts);
        assert_eq!(spans.len(), n, "every entry must still get a row, no panic");

        for (i, row) in spans.iter().enumerate().skip(QF_HIGHLIGHT_BUDGET) {
            assert_eq!(
                row.len(),
                3,
                "entry {i} is past the budget — must be format-only spans"
            );
        }
    }
}
