//! Quickfix-list host integration (#184): `:grep` population, the `:copen`
//! popup navigation, and jump-to-entry. The agnostic list + cursor live in the
//! `hjkl-quickfix` crate; ex commands arrive as `ExEffect::Quickfix(QfCommand)`.

use hjkl_ex::QfCommand;
use hjkl_quickfix::{QfEntry, QfKind};

impl crate::app::App {
    /// Dispatch a quickfix ex command (`:copen`, `:cnext`, `:grep`, …).
    pub(crate) fn handle_quickfix_command(&mut self, cmd: QfCommand) {
        match cmd {
            QfCommand::Grep(pat) => self.quickfix_run_grep(&pat),
            QfCommand::Make(extra) => self.quickfix_run_make(&extra),
            QfCommand::Open => {
                if self.quickfix.is_empty() {
                    self.bus.info("quickfix list is empty");
                } else {
                    self.quickfix_open = true;
                }
            }
            QfCommand::Close => self.quickfix_open = false,
            QfCommand::Next => {
                self.quickfix.next();
                self.quickfix_after_nav();
            }
            QfCommand::Prev => {
                self.quickfix.prev();
                self.quickfix_after_nav();
            }
            QfCommand::First => {
                self.quickfix.first();
                self.quickfix_after_nav();
            }
            QfCommand::Last => {
                self.quickfix.last();
                self.quickfix_after_nav();
            }
            QfCommand::Nth(n) => {
                // `0` means "current" (bare `:cc`); otherwise 1-based.
                if n > 0 {
                    self.quickfix.nth(n);
                }
                self.quickfix_after_nav();
            }
        }
    }

    /// `]q` / `[q` and the `:cnext`/`:cprev` family route here after moving the
    /// list cursor: keep the popup view in sync and jump the editor.
    fn quickfix_after_nav(&mut self) {
        if self.quickfix.is_empty() {
            self.bus.info("quickfix list is empty");
            return;
        }
        self.quickfix_jump_to_current();
    }

    /// Open the current entry's file and place the cursor on it. No-op when the
    /// list is empty.
    pub(crate) fn quickfix_jump_to_current(&mut self) {
        let Some(entry) = self.quickfix.current() else {
            return;
        };
        let path = entry.path.to_string_lossy().to_string();
        let (row, col) = (entry.row, entry.col);
        self.do_edit(&path, false);
        self.active_editor_mut().jump_cursor(row, col);
        self.sync_after_engine_mutation();
    }

    /// `:grep <pat>` — run the detected grep backend (blocking), parse hits into
    /// the quickfix list, and open the popup. Reuses `hjkl-picker`'s rg parsers.
    pub(crate) fn quickfix_run_grep(&mut self, pat: &str) {
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
        self.quickfix.set(entries);
        if n == 0 {
            self.quickfix_open = false;
            self.bus.info(format!("no matches for \"{pat}\""));
        } else {
            self.quickfix_open = true;
            self.bus.info(format!("{n} matches"));
        }
    }

    /// `:make [extra]` — run `makeprg` (with `extra` appended) blocking, parse
    /// stdout+stderr through the errorformat, populate the quickfix list, and
    /// open the popup. Blocking: the TUI is frozen for the build's duration
    /// (cargo check can take seconds). Async `:make` is a possible follow-up.
    pub(crate) fn quickfix_run_make(&mut self, extra: &str) {
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
        self.quickfix.set(entries);
        if n == 0 {
            self.quickfix_open = false;
            self.bus.info(format!("{makeprg}: no errors"));
        } else {
            self.quickfix_open = true;
            self.bus.info(format!("{n} entries"));
        }
    }

    /// Move the popup highlight down (popup `j` / `<Down>`). Does NOT jump —
    /// vim's quickfix window only jumps on `<CR>`. The render `ListState`
    /// auto-scrolls to keep the selected entry visible.
    pub(crate) fn quickfix_popup_down(&mut self) {
        self.quickfix.next();
    }

    /// Move the popup highlight up (popup `k` / `<Up>`).
    pub(crate) fn quickfix_popup_up(&mut self) {
        self.quickfix.prev();
    }
}
