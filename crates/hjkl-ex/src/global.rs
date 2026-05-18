//! `:[range]global/pat/cmd` and `:[range]vglobal/pat/cmd` — Phase 8a.
//!
//! Currently supports only `:g/pat/d` (delete matching lines) and
//! `:g!/pat/d` / `:v/pat/d` (delete non-matching lines).
//! Ported verbatim from `hjkl_editor::ex::apply_global`.

use crate::{effect::ExEffect, range::LineRange};
use hjkl_engine::Host;

/// Split `s` by `sep`, treating `\<sep>` as a literal occurrence.
fn split_unescaped(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                if next == sep {
                    cur.push(sep);
                    chars.next();
                } else {
                    cur.push('\\');
                    cur.push(next);
                    chars.next();
                }
            } else {
                cur.push('\\');
            }
        } else if c == sep {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// Run `:[range]g/pat/d` (or its negated variants).
///
/// Walks the rows in `range` (whole buffer when None), collects matches,
/// then drops them in reverse so row indices stay valid through the cascade
/// of deletes. Ported verbatim from `hjkl_editor::ex::apply_global`.
pub(crate) fn global_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    args: &str,
    range: Option<LineRange>,
    negate: bool,
) -> Option<ExEffect> {
    use hjkl_buffer::{Edit, MotionKind, Position};

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
    let parts = split_unescaped(&rest, sep);
    if parts.len() < 2 {
        return Some(ExEffect::Error("global needs /pattern/cmd".into()));
    }
    let pattern = parts[0].clone();
    let cmd = parts[1].trim();
    if cmd != "d" {
        return Some(ExEffect::Error(format!(
            ":g supports only `d` today, got `{cmd}`"
        )));
    }
    let regex = match regex::Regex::new(&pattern) {
        Ok(r) => r,
        Err(e) => return Some(ExEffect::Error(format!("bad pattern: {e}"))),
    };

    editor.push_undo();

    // Identify rows to drop. Default to whole buffer when no range supplied.
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
        let line = editor.buffer().line(row).unwrap_or_default();
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
    let count = targets.len();
    for row in targets.iter().rev() {
        let row = *row;
        if editor.buffer().row_count() == 1 {
            let line_chars = editor
                .buffer()
                .line(0)
                .map(|l| l.chars().count())
                .unwrap_or(0);
            if line_chars > 0 {
                editor.mutate_edit(Edit::DeleteRange {
                    start: Position::new(0, 0),
                    end: Position::new(0, line_chars),
                    kind: MotionKind::Char,
                });
            }
            continue;
        }
        editor.mutate_edit(Edit::DeleteRange {
            start: Position::new(row, 0),
            end: Position::new(row, 0),
            kind: MotionKind::Line,
        });
    }
    editor.mark_content_dirty();
    Some(ExEffect::Substituted {
        count,
        lines_changed: count,
    })
}

/// `:global/pat/cmd` — delete matching lines (negate=false).
pub(crate) fn global_match_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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

/// `:vglobal/pat/cmd` — delete non-matching lines (negate=true).
pub(crate) fn vglobal_handler<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    args: &str,
    range: Option<LineRange>,
) -> Option<ExEffect> {
    global_handler(editor, args, range, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_engine::{DefaultHost, Editor, Options};

    fn make_editor_with_lines(lines: &[&str]) -> Editor<hjkl_buffer::Buffer, DefaultHost> {
        let content = lines.join("\n");
        let buf = hjkl_buffer::Buffer::from_str(&content);
        let host = DefaultHost::new();
        Editor::new(buf, host, Options::default())
    }

    #[test]
    fn global_d_deletes_matching_lines() {
        let mut editor = make_editor_with_lines(&["foo", "bar", "foo", "baz"]);
        let result = global_match_handler(&mut editor, "/foo/d", None);
        assert!(
            matches!(result, Some(ExEffect::Substituted { count: 2, .. })),
            "got: {result:?}"
        );
        let lines = editor.buffer().lines().to_vec();
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
        let lines = editor.buffer().lines().to_vec();
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
    fn global_range_limits_scope() {
        // Only delete 'foo' in lines 1-2; line 3 foo preserved.
        let mut editor = make_editor_with_lines(&["foo", "foo", "foo"]);
        let range = LineRange::new(1, 2);
        let _result = global_match_handler(&mut editor, "/foo/d", Some(range));
        let lines = editor.buffer().lines().to_vec();
        // 2 deleted, 1 remaining foo
        assert_eq!(lines.len(), 1, "lines: {lines:?}");
    }

    #[test]
    fn global_bang_form_negates() {
        // :global!/foo/d → delete lines NOT matching foo.
        let mut editor = make_editor_with_lines(&["foo", "bar", "baz"]);
        let result = global_match_handler(&mut editor, "!/foo/d", None);
        assert!(matches!(result, Some(ExEffect::Substituted { .. })));
        let lines = editor.buffer().lines().to_vec();
        assert!(lines.iter().all(|l| l == "foo"), "lines: {lines:?}");
    }
}
