//! Quickfix-list / location-list host integration (#184): `:grep` / `:make`
//! population, the `:copen`/`:lopen` popup navigation, and jump-to-entry. The
//! agnostic list + cursor live in the `hjkl-quickfix` crate; ex commands arrive
//! as `ExEffect::Quickfix(QfCommand)` (global list) or `ExEffect::Location(...)`
//! (window-local list). Both lists share this machinery via [`QfWhich`].

use hjkl_ex::QfCommand;
use hjkl_quickfix::{QfEntry, QfKind, QfList};

/// Which list a quickfix action targets: the global quickfix list (`:c*`) or
/// the location list (`:l*`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum QfWhich {
    Quickfix,
    Location,
}

impl QfWhich {
    /// Human label used in toasts and the popup title.
    fn label(self) -> &'static str {
        match self {
            QfWhich::Quickfix => "quickfix",
            QfWhich::Location => "location",
        }
    }
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

    fn qf_set_open(&mut self, w: QfWhich, open: bool) {
        match w {
            QfWhich::Quickfix => self.quickfix_open = open,
            QfWhich::Location => self.loclist_open = open,
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
            QfCommand::Open => {
                if self.qf_list(w).is_empty() {
                    self.bus.info(format!("{} list is empty", w.label()));
                } else {
                    self.qf_set_open(w, true);
                }
            }
            QfCommand::Close => self.qf_set_open(w, false),
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
        }
    }

    /// `]q`/`[q` (and `]l`/`[l`) and the `:cnext`/`:lnext` families route here
    /// after moving the list cursor: keep the popup in sync and jump the editor.
    fn qf_after_nav(&mut self, w: QfWhich) {
        if self.qf_list(w).is_empty() {
            self.bus.info(format!("{} list is empty", w.label()));
            return;
        }
        self.qf_jump_to_current(w);
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
                .args(["--json", "--no-config", "--smart-case", pat, root_str])
                .output(),
            GrepBackend::Grep => std::process::Command::new("grep")
                .args(["-rnH", "--", pat, root_str])
                .output(),
            GrepBackend::Findstr => std::process::Command::new("findstr")
                .args(["/s", "/n", pat, "*"])
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
        let mut argv: Vec<String> = makeprg.split_whitespace().map(str::to_string).collect();
        argv.extend(extra.split_whitespace().map(str::to_string));
        let Some((program, rest)) = argv.split_first() else {
            self.bus.error("makeprg is empty");
            return;
        };

        let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let output = std::process::Command::new(program)
            .args(rest)
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

    /// Replace the location list with a set of entries and open the popup.
    /// Used by LSP references (`gr`) to fill the loclist alongside the picker.
    pub(crate) fn set_loclist(&mut self, entries: Vec<QfEntry>) {
        self.loclist.set(entries);
        self.loclist_open = false; // populate silently; user opens via :lopen
    }

    // ── popup navigation (event loop) ───────────────────────────────────────

    /// `:copen` popup: move the highlight down (`j` / `<Down>`). Does NOT jump —
    /// vim's quickfix window only jumps on `<CR>`. The render `ListState`
    /// auto-scrolls to keep the selected entry visible.
    pub(crate) fn quickfix_popup_down(&mut self) {
        self.quickfix.next();
    }

    /// `:copen` popup: move the highlight up (`k` / `<Up>`).
    pub(crate) fn quickfix_popup_up(&mut self) {
        self.quickfix.prev();
    }

    /// `:copen` popup: jump to the highlighted entry (`<CR>`).
    pub(crate) fn quickfix_jump_to_current(&mut self) {
        self.qf_jump_to_current(QfWhich::Quickfix);
    }

    /// `:lopen` popup: move the highlight down.
    pub(crate) fn loclist_popup_down(&mut self) {
        self.loclist.next();
    }

    /// `:lopen` popup: move the highlight up.
    pub(crate) fn loclist_popup_up(&mut self) {
        self.loclist.prev();
    }

    /// `:lopen` popup: jump to the highlighted entry.
    pub(crate) fn loclist_jump_to_current(&mut self) {
        self.qf_jump_to_current(QfWhich::Location);
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
