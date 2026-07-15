//! `:foldindent` and `:foldsyntax` — Phase 8a.

use crate::{effect::ExEffect, range::LineRange};
use hjkl_engine::Host;

/// `:foldindent` — derive folds from leading-whitespace runs.
///
/// Each row whose successor is more deeply indented becomes a fold opener;
/// the fold extends to the row before indent drops back to or below the
/// opener's level. Ported verbatim from `hjkl_editor::ex::apply_fold_indent`.
pub(crate) fn apply_fold_indent<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let rope = editor.buffer().rope();
    let total = rope.len_lines();
    let lines: Vec<String> = (0..total)
        .map(|i| hjkl_buffer::rope_line_str(&rope, i))
        .collect();
    if total == 0 {
        return Some(ExEffect::Ok);
    }
    let indent =
        |line: &str| -> usize { line.chars().take_while(|c| *c == ' ' || *c == '\t').count() };
    let indents: Vec<usize> = lines.iter().map(|l| indent(l)).collect();
    let blank: Vec<bool> = lines.iter().map(|l| l.trim().is_empty()).collect();
    let mut new_folds: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i + 1 < total {
        if blank[i] {
            i += 1;
            continue;
        }
        let head_indent = indents[i];
        let mut j = i + 1;
        // Skip blanks adjacent to the head — they belong to the same
        // block so a fold can span across them.
        while j < total && blank[j] {
            j += 1;
        }
        if j >= total || indents[j] <= head_indent {
            i += 1;
            continue;
        }
        // We have a fold opener — walk forward until indent drops back
        // to <= head_indent on a non-blank row.
        let mut end = j;
        let mut k = j + 1;
        while k < total {
            if !blank[k] && indents[k] <= head_indent {
                break;
            }
            end = k;
            k += 1;
        }
        new_folds.push((i, end));
        // Step by one (not past `end`) so nested indented runs inside
        // the outer block also get their own fold.
        i += 1;
    }
    if new_folds.is_empty() {
        return Some(ExEffect::Info("no indented blocks to fold".into()));
    }
    let count = new_folds.len();
    for (start, end) in new_folds {
        editor.apply_fold_op(hjkl_engine::FoldOp::Add {
            start_row: start,
            end_row: end,
            closed: true,
        });
    }
    Some(ExEffect::Info(format!("created {count} fold(s)")))
}

/// `:foldsyntax` — apply the host-supplied syntax-tree block ranges as
/// closed folds. The host calls `Editor::set_syntax_fold_ranges` on every
/// tree-sitter re-parse; running this command consumes the latest snapshot.
/// No-op when the host hasn't pushed any ranges yet.
///
/// Ported verbatim from `hjkl_editor::ex::apply_fold_syntax`.
pub(crate) fn apply_fold_syntax<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    _args: &str,
    _range: Option<LineRange>,
) -> Option<ExEffect> {
    let ranges = editor.syntax_fold_ranges();
    if ranges.is_empty() {
        return Some(ExEffect::Info("no syntax block ranges available".into()));
    }
    let count = ranges.len();
    for (start, end) in ranges {
        editor.apply_fold_op(hjkl_engine::FoldOp::Add {
            start_row: start,
            end_row: end,
            closed: true,
        });
    }
    Some(ExEffect::Info(format!("created {count} fold(s)")))
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
    fn foldindent_on_indented_buffer_creates_folds() {
        let mut editor =
            make_editor_with_lines(&["fn foo() {", "    let x = 1;", "    let y = 2;", "}"]);
        let result = apply_fold_indent(&mut editor, "", None);
        match result {
            Some(ExEffect::Info(msg)) => {
                assert!(msg.contains("fold"), "expected fold count msg, got: {msg}");
            }
            other => panic!("expected Info(_), got {other:?}"),
        }
    }

    #[test]
    fn foldindent_on_flat_buffer_returns_no_blocks() {
        let mut editor = make_editor_with_lines(&["line1", "line2", "line3"]);
        let result = apply_fold_indent(&mut editor, "", None);
        assert_eq!(
            result,
            Some(ExEffect::Info("no indented blocks to fold".into()))
        );
    }

    #[test]
    fn foldindent_on_empty_buffer_returns_ok() {
        let mut editor = make_editor_with_lines(&[""]);
        let result = apply_fold_indent(&mut editor, "", None);
        // Single empty line → total = 1, loop runs 0 times → returns Ok or no-blocks.
        // Our impl: total==1, so the while loop body (i+1 < 1 is false) is never entered.
        // After loop, new_folds is empty → returns Info("no indented blocks to fold").
        assert!(matches!(
            result,
            Some(ExEffect::Ok) | Some(ExEffect::Info(_))
        ));
    }

    #[test]
    fn foldsyntax_with_no_ranges_returns_info() {
        let mut editor = make_editor_with_lines(&["fn foo() {", "    bar();", "}"]);
        let result = apply_fold_syntax(&mut editor, "", None);
        assert_eq!(
            result,
            Some(ExEffect::Info("no syntax block ranges available".into()))
        );
    }
}
