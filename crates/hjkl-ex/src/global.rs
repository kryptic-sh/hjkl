//! `:[range]global/pat/cmd` and `:[range]vglobal/pat/cmd` — Phase 8a, extended
//! for B4/B14 (round-2a) to support the `d` / `s` / `j` / `y` sub-commands
//! with vim's two-pass execution model and per-line register/cursor
//! semantics.
//!
//! # Two-pass execution (vim semantics)
//!
//! Vim's `:g` first scans the WHOLE scope and marks every matching (or, for
//! `:v`/`:g!`, non-matching) line, THEN executes the sub-command once per
//! marked line, in ascending document order — critically, marking happens
//! before any mutation, so a sub-command that changes the line count (`d`,
//! `j`) doesn't corrupt which lines were originally selected.
//!
//! Real vim tracks marks with a buffer-local flag bit per line that survives
//! edits. hjkl-buffer has no such mechanism, so this module reconstructs the
//! same effect with plain row arithmetic: rows are marked by their ORIGINAL
//! (pre-mutation) index, then replayed ascending while accumulating a signed
//! `shift` — the cumulative row-count delta from every earlier sub-command
//! invocation. `actual_row = orig_row as i64 + shift`. This works for any
//! sub-command that changes the total row count uniformly (delete removes
//! rows, join merges rows) as well as ones that don't (substitute, yank).
//!
//! # Sub-commands
//!
//! `d` [register], `s/pat/rep/[flags]` (including empty-pattern `s//rep/`,
//! which reuses the `:g` pattern itself — vim sets the "last search pattern"
//! to the `:g` pattern before running the sub-command loop), `j` [count],
//! `y` [register] [count]. Each delegates to the SAME handler as the
//! standalone `:d` / `:s` / `:j` / `:y` ex commands (`crate::builtins`), so
//! register/cursor semantics fixed there (B5/B6/B7) apply for free here too.
//!
//! `:g/pat/normal ...` is explicitly OUT OF SCOPE (see `DIVERGE.md`) — hjkl
//! has no general `:normal {cmd}` ex command yet. It errors cleanly rather
//! than silently no-op'ing.

use crate::{effect::ExEffect, range::LineRange};
use hjkl_engine::Host;

/// Split `s` at the FIRST unescaped occurrence of `sep`, treating `\<sep>`
/// as a literal occurrence. Returns `(before, after)` with the separator
/// itself consumed, or `None` if `sep` never appears unescaped.
///
/// Unlike a general "split on every occurrence" helper, this stops after
/// the first match: `:g/pattern/cmd` only uses the separator to delimit
/// PATTERN from CMD — the cmd tail (e.g. `s/foo/bar/g`) commonly contains
/// the SAME character again as its own delimiter, and re-splitting on every
/// occurrence would shred it (this was a real bug caught while adding `:g`
/// sub-command support — a naive `split('/').collect()` turned
/// `:g/foo/s//X/` into five parts instead of pattern="foo", cmd="s//X/").
fn split_first_unescaped(s: &str, sep: char) -> Option<(String, String)> {
    let mut cur = String::new();
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(&(_, next)) if next == sep => {
                    cur.push(sep);
                    chars.next();
                }
                _ => cur.push('\\'),
            }
            continue;
        }
        if c == sep {
            let rest_start = i + c.len_utf8();
            return Some((cur, s[rest_start..].to_string()));
        }
        cur.push(c);
    }
    None
}

/// Execute `cmd` (the sub-command tail after the `:g/pat/` delimiter, e.g.
/// `"d"`, `"d a"`, `"s/foo/bar/g"`, `"j"`, `"y a 2"`) at `row` (0-based).
/// Delegates to the standalone ex-command handlers with a single-line range
/// `[row+1, row+1]` (1-based), so each runs exactly as `:{row+1}{cmd}` would.
fn dispatch_sub_command<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    cmd: &str,
    row: usize,
) -> Option<ExEffect> {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return Some(ExEffect::Error("global: missing sub-command".into()));
    }
    let range = Some(LineRange::single(row + 1));
    let (head, rest) = cmd.split_at(1);
    match head {
        "d" => crate::builtins::delete_handler(editor, rest.trim_start(), range),
        "y" => crate::builtins::yank_handler(editor, rest.trim_start(), range),
        "j" => crate::builtins::join_handler(editor, rest.trim_start(), range),
        // `s` takes the REST verbatim (starting with the delimiter, e.g.
        // "/foo/bar/g") or empty/flags-only for the bare-repeat form (B17) —
        // exactly what `substitute_handler` expects as `args`.
        "s" => crate::builtins::substitute_handler(editor, rest, range),
        _ if cmd.starts_with("normal") => Some(ExEffect::Error(
            ":g/pat/normal is not supported (see DIVERGE.md)".into(),
        )),
        _ => Some(ExEffect::Error(format!(
            ":g supports d/s/j/y today, got `{cmd}`"
        ))),
    }
}

/// Run `:[range]g/pat/cmd` (or its negated variants).
///
/// Two-pass: marks every matching row in `range` (whole buffer when `None`)
/// against the ORIGINAL buffer, then executes `cmd` on each marked row in
/// ascending order, tracking a signed row-count shift so later marks stay
/// valid as earlier invocations add/remove rows. See the module doc for the
/// full design rationale.
pub(crate) fn global_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    range: Option<LineRange>,
    negate: bool,
) -> Option<ExEffect> {
    let mut chars = args.chars();
    let sep = match chars.next() {
        Some(c) => c,
        None => return Some(ExEffect::Error("empty :g pattern".into())),
    };
    if sep.is_alphanumeric() || sep == '\\' {
        return Some(ExEffect::Error(
            "global needs a separator, e.g. :g/foo/d".into(),
        ));
    }
    let rest: String = chars.collect();
    let Some((pattern, cmd)) = split_first_unescaped(&rest, sep) else {
        return Some(ExEffect::Error("global needs /pattern/cmd".into()));
    };
    let cmd = cmd.trim().to_string();

    use hjkl_engine::search::{CaseMode, resolve_case_mode};
    let s = editor.settings();
    let base = CaseMode::from_options(s.ignore_case, s.smartcase);
    let (stripped, mode) = resolve_case_mode(&pattern, base, &editor.last_substitute_replacement());
    let compile_src = if mode == CaseMode::Insensitive {
        format!("(?i){stripped}")
    } else {
        stripped
    };
    let regex = match regex::Regex::new(&compile_src) {
        Ok(r) => r,
        Err(e) => return Some(ExEffect::Error(format!("bad pattern: {e}"))),
    };

    editor.push_undo();

    // Pass 1: mark rows against the ORIGINAL (pre-mutation) buffer. Default
    // scope is the whole buffer when no range was supplied.
    let (scope_start, scope_end) = match range {
        Some(r) => {
            let start = r.start_one_based().saturating_sub(1);
            let total = editor.buffer().row_count();
            let end = (r.end_one_based().saturating_sub(1)).min(total.saturating_sub(1));
            (start, end)
        }
        None => {
            let total = editor.buffer().row_count();
            (0, total.saturating_sub(1))
        }
    };

    let row_count = editor.buffer().row_count();
    let bot = scope_end.min(row_count.saturating_sub(1));
    let mut targets: Vec<usize> = Vec::new();
    for row in scope_start..=bot {
        let line = hjkl_buffer::rope_line_str(&editor.buffer().rope(), row);
        let matches = regex.is_match(&line);
        if matches != negate {
            targets.push(row);
        }
    }
    if targets.is_empty() {
        editor.pop_last_undo();
        return Some(ExEffect::Substituted {
            count: 0,
            lines_changed: 0,
        });
    }
    let total_targets = targets.len();

    // Vim: `:g` sets the "last search pattern" to its own pattern argument
    // BEFORE running the sub-command loop, so an empty-pattern `s//rep/`
    // inside reuses it (apply_substitute already falls back to
    // `editor.last_search()` for `pattern: None`).
    editor.set_last_search(Some(pattern), true);

    // Pass 2: execute `cmd` on each marked row, ascending, tracking a
    // signed shift so later marks (recorded against the ORIGINAL buffer)
    // stay valid as earlier invocations change the row count.
    let mut shift: i64 = 0;
    let mut last_row: Option<usize> = None;
    for orig_row in targets {
        let before_total = editor.buffer().row_count() as i64;
        let actual_row = orig_row as i64 + shift;
        if actual_row < 0 || actual_row as usize >= before_total as usize {
            continue;
        }
        let actual_row = actual_row as usize;

        editor.jump_cursor(actual_row, 0);
        let effect = dispatch_sub_command(editor, &cmd, actual_row);
        if let Some(ExEffect::Error(e)) = effect {
            editor.mark_content_dirty();
            return Some(ExEffect::Error(e));
        }

        let after_total = editor.buffer().row_count() as i64;
        shift += after_total - before_total;
        last_row = Some(actual_row);
    }

    editor.mark_content_dirty();

    // Cursor: the row of the LAST sub-command invocation, clamped to the
    // (possibly shrunk) buffer, first non-blank — matches vim's behavior
    // for `:g/pat/d` (lands past the last deletion, clamped) and
    // `:g/pat/s//rep/` (lands on the last substituted line), verified
    // against nvim v0.12.4.
    if let Some(row) = last_row {
        let total = editor.buffer().row_count();
        let row = row.min(total.saturating_sub(1));
        let line = hjkl_buffer::rope_line_str(&editor.buffer().rope(), row);
        let first_non_blank = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
        editor.jump_cursor(row, first_non_blank);
    }

    Some(ExEffect::Substituted {
        count: total_targets,
        lines_changed: total_targets,
    })
}

/// `:global/pat/cmd` — matching lines (negate=false).
pub(crate) fn global_match_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    // Strip leading '!' for :global!/pat/cmd form.
    let (negate, body) = if let Some(rest) = args.strip_prefix('!') {
        (true, rest.trim_start())
    } else {
        (false, args)
    };
    global_handler(editor, body, range, negate)
}

/// `:vglobal/pat/cmd` — non-matching lines (negate=true).
pub(crate) fn vglobal_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    global_handler(editor, args, range, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_engine::{DefaultHost, Editor, Options};

    fn make_editor_with_lines(lines: &[&str]) -> Editor<hjkl_buffer::View, DefaultHost> {
        let content = lines.join("\n");
        let buf = hjkl_buffer::View::from_str(&content);
        let host = DefaultHost::new();
        hjkl_vim::vim_editor(buf, host, Options::default())
    }

    #[test]
    fn global_d_deletes_matching_lines() {
        let mut editor = make_editor_with_lines(&["foo", "bar", "foo", "baz"]);
        let result = global_match_handler(&mut editor, "/foo/d", None);
        assert!(
            matches!(result, Some(ExEffect::Substituted { count: 2, .. })),
            "got: {result:?}"
        );
        let lines = buf_lines(&editor);
        assert!(!lines.contains(&"foo".to_string()), "lines: {lines:?}");
        assert!(lines.contains(&"bar".to_string()));
    }

    #[test]
    fn vglobal_d_deletes_non_matching_lines() {
        let mut editor = make_editor_with_lines(&["foo", "bar", "foo", "baz"]);
        let result = vglobal_handler(&mut editor, "/foo/d", None);
        assert!(
            matches!(result, Some(ExEffect::Substituted { .. })),
            "got: {result:?}"
        );
        let lines = buf_lines(&editor);
        // non-foo lines (bar, baz) deleted; only foo remains
        assert!(!lines.contains(&"bar".to_string()), "lines: {lines:?}");
        assert!(!lines.contains(&"baz".to_string()), "lines: {lines:?}");
    }

    #[test]
    fn global_no_match_returns_zero_count() {
        let mut editor = make_editor_with_lines(&["hello", "world"]);
        let result = global_match_handler(&mut editor, "/xyz/d", None);
        assert_eq!(
            result,
            Some(ExEffect::Substituted {
                count: 0,
                lines_changed: 0
            })
        );
    }

    #[test]
    fn global_bad_pattern_returns_error() {
        let mut editor = make_editor_with_lines(&["foo"]);
        let result = global_match_handler(&mut editor, "/[bad/d", None);
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "got: {result:?}"
        );
    }

    #[test]
    fn global_unsupported_cmd_returns_error() {
        let mut editor = make_editor_with_lines(&["foo"]);
        let result = global_match_handler(&mut editor, "/foo/p", None);
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "got: {result:?}"
        );
    }

    #[test]
    fn global_normal_subcommand_errors_cleanly() {
        // :g/pat/normal is out of scope (DIVERGE.md) — must error, not panic
        // or silently no-op.
        let mut editor = make_editor_with_lines(&["foo", "bar"]);
        let result = global_match_handler(&mut editor, "/foo/normal x", None);
        assert!(
            matches!(result, Some(ExEffect::Error(_))),
            "got: {result:?}"
        );
    }

    #[test]
    fn global_range_limits_scope() {
        // Only delete 'foo' in lines 1-2; line 3 foo preserved.
        let mut editor = make_editor_with_lines(&["foo", "foo", "foo"]);
        let range = LineRange::new(1, 2);
        let _result = global_match_handler(&mut editor, "/foo/d", Some(range));
        let lines = buf_lines(&editor);
        // 2 deleted, 1 remaining foo
        assert_eq!(lines.len(), 1, "lines: {lines:?}");
    }

    #[test]
    fn global_bang_form_negates() {
        // :global!/foo/d → delete lines NOT matching foo.
        let mut editor = make_editor_with_lines(&["foo", "bar", "baz"]);
        let result = global_match_handler(&mut editor, "!/foo/d", None);
        assert!(matches!(result, Some(ExEffect::Substituted { .. })));
        let lines = buf_lines(&editor);
        assert!(lines.iter().all(|l| l == "foo"), "lines: {lines:?}");
    }

    #[test]
    fn global_s_substitutes_on_matching_lines() {
        let mut editor = make_editor_with_lines(&["foo1", "bar", "foo2"]);
        let result = global_match_handler(&mut editor, "/foo/s//X/", None);
        assert!(
            matches!(result, Some(ExEffect::Substituted { count: 2, .. })),
            "got: {result:?}"
        );
        let lines = buf_lines(&editor);
        assert_eq!(lines, vec!["X1", "bar", "X2"]);
    }

    #[test]
    fn global_d_writes_unnamed_register_to_last_deleted_line() {
        let mut editor = make_editor_with_lines(&["foo1", "bar", "foo2"]);
        let _result = global_match_handler(&mut editor, "/foo/d", None);
        let last_delete = editor.with_registers(|r| r.read('"').unwrap().text.clone());
        assert_eq!(last_delete, "foo2\n");
    }

    fn buf_lines(editor: &Editor<hjkl_buffer::View, DefaultHost>) -> Vec<String> {
        let rope = editor.buffer().rope();
        (0..rope.len_lines())
            .map(|i| hjkl_buffer::rope_line_str(&rope, i))
            .collect()
    }
}
