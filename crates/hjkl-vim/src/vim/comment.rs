//! Vim FSM: comment.
//!
//! Split out of the monolithic `vim.rs` (#267 follow-up).

use hjkl_vim_types::{InsertReason, InsertSession, LastChange, Mode};

use hjkl_engine::rope_util::rope_row_range_str;

use super::*;
use crate::vim_state::{vim, vim_mut};
use hjkl_engine::Editor;
use hjkl_engine::buf_helpers::{buf_line, buf_line_chars, buf_set_cursor_rc};

/// Return the ordered (longest-first) list of line-comment prefixes for
/// `lang`. Each prefix includes one trailing space (e.g. `"// "`).
/// The same table lives in `hjkl-lang::comment` for the `gc` toggle (#187).
pub(crate) fn comment_prefixes_for_lang(lang: &str) -> &'static [&'static str] {
    match lang {
        "rust" => &["/// ", "//! ", "// "],
        "c" | "cpp" => &["// "],
        "python" | "sh" | "bash" | "zsh" | "fish" | "toml" | "yaml" => &["# "],
        "lua" => &["-- "],
        "sql" => &["-- "],
        "vim" | "viml" => &["\" "],
        _ => &[],
    }
}
/// Detect whether `line` starts with a known comment prefix for `lang`.
///
/// Returns `Some((indent, prefix))` where `indent` is the leading whitespace
/// of the line and `prefix` is the canonical (with trailing space) comment
/// marker. Returns `None` when the line is not a recognised comment.
pub(crate) fn detect_comment_on_line(lang: &str, line: &str) -> Option<(String, &'static str)> {
    let indent_end = line
        .char_indices()
        .find(|(_, c)| *c != ' ' && *c != '\t')
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    let indent = line[..indent_end].to_string();
    let rest = &line[indent_end..];
    for &prefix in comment_prefixes_for_lang(lang) {
        if rest.starts_with(prefix) {
            return Some((indent, prefix));
        }
        // Also match the bare prefix (line that is exactly `//` with no
        // trailing content).
        let bare = prefix.trim_end_matches(' ');
        if rest == bare || rest.starts_with(&format!("{bare} ")) {
            return Some((indent, prefix));
        }
    }
    None
}
/// Given the current `row` in `buffer` and the active `settings`, return the
/// string to prepend on the new line when comment-continuation fires.
///
/// Returns `Some("<indent><prefix>")` when the row is a comment line and
/// continuation is appropriate, `None` otherwise. The caller appends the
/// string after the `\n` they are about to insert.
pub(crate) fn continue_comment(
    buffer: &hjkl_buffer::View,
    settings: &hjkl_engine::Settings,
    row: usize,
) -> Option<String> {
    if settings.filetype.is_empty() {
        return None;
    }
    let line = hjkl_engine::buf_helpers::buf_line(buffer, row)?;
    let (indent, prefix) = detect_comment_on_line(&settings.filetype, &line)?;
    Some(format!("{indent}{prefix}"))
}
/// Strip one indent unit from the beginning of `line` and insert `ch`
/// instead. Returns `true` when it consumed the keystroke (dedent +
/// insert), `false` when the caller should insert normally.
///
/// Dedent fires when:
///   - `smartindent` is on
///   - `ch` is `}` / `)` / `]`
///   - all bytes BEFORE the cursor on the current line are whitespace
///   - there is at least one full indent unit of leading whitespace
pub(crate) fn try_dedent_close_bracket<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    cursor: hjkl_buffer::Position,
    ch: char,
) -> bool {
    use hjkl_buffer::{Edit, MotionKind, Position};

    if !ed.settings().smartindent {
        return false;
    }
    if !matches!(ch, '}' | ')' | ']') {
        return false;
    }

    let line = match buf_line(ed.buffer(), cursor.row) {
        Some(l) => l.to_string(),
        None => return false,
    };

    // All chars before cursor must be whitespace.
    let before: String = line.chars().take(cursor.col).collect();
    if !before.chars().all(|c| c == ' ' || c == '\t') {
        return false;
    }
    if before.is_empty() {
        // Nothing to strip — just insert normally (cursor at col 0).
        return false;
    }

    // Compute indent unit.
    let unit_len: usize = if ed.settings().expandtab {
        if ed.settings().softtabstop > 0 {
            ed.settings().softtabstop
        } else {
            ed.settings().shiftwidth
        }
    } else {
        // Tab: one literal tab character.
        1
    };

    // Check there's at least one full unit to strip.
    let strip_len = if ed.settings().expandtab {
        // Count leading spaces; need at least `unit_len`.
        let spaces = before.chars().filter(|c| *c == ' ').count();
        if spaces < unit_len {
            return false;
        }
        unit_len
    } else {
        // noexpandtab: strip one leading tab.
        if !before.starts_with('\t') {
            return false;
        }
        1
    };

    // Delete the leading `strip_len` chars of the current line.
    ed.mutate_edit(Edit::DeleteRange {
        start: Position::new(cursor.row, 0),
        end: Position::new(cursor.row, strip_len),
        kind: MotionKind::Char,
    });
    // Insert the close bracket at column 0 (after the delete the cursor
    // is still positioned at the end of the remaining whitespace; the
    // delete moved the text so the cursor is now at col = before.len() -
    // strip_len).
    let new_col = cursor.col.saturating_sub(strip_len);
    ed.mutate_edit(Edit::InsertChar {
        at: Position::new(cursor.row, new_col),
        ch,
    });
    true
}
pub(crate) fn finish_insert_session<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) {
    let Some(session) = vim_mut(ed).insert_session.take() else {
        return;
    };
    let after_rope = hjkl_engine::types::Query::rope(ed.buffer());
    // Clamp both slices to their respective bounds — the buffer may have
    // grown (Enter splits rows) or shrunk (Backspace joins rows) during
    // the session, so row_max can overshoot either side.
    let before_n = session.before_rope.len_lines();
    let after_n = after_rope.len_lines();
    let after_end = session.row_max.min(after_n.saturating_sub(1));
    let before_end = session.row_max.min(before_n.saturating_sub(1));
    let before = if before_end >= session.row_min && session.row_min < before_n {
        rope_row_range_str(&session.before_rope, session.row_min, before_end)
    } else {
        String::new()
    };
    let after = if after_end >= session.row_min && session.row_min < after_n {
        rope_row_range_str(&after_rope, session.row_min, after_end)
    } else {
        String::new()
    };
    // `R` overstrike keeps the line length the same, so `extract_inserted`
    // (which only reports net growth) misses the typed text. Use the changed
    // run instead so dot-repeat retypes it.
    let inserted = if matches!(session.reason, InsertReason::Replace) {
        changed_run(&before, &after)
    } else {
        extract_inserted(&before, &after)
    };
    // vim `".` register — text of the most recent insert.
    if !vim(ed).replaying && !inserted.is_empty() {
        vim_mut(ed).last_insert_text = Some(inserted.clone());
    }
    let open_line = matches!(session.reason, InsertReason::Open { .. });
    if session.count > 1 && !vim(ed).replaying {
        use hjkl_buffer::{Edit, Position};
        if open_line {
            // `[count]o` / `[count]O` open `count` SEPARATE lines, each with the
            // typed text. Read the just-opened line's content directly (the
            // row-range extract above is unreliable across the open boundary)
            // and stack `count - 1` further lines below it.
            let (start_row, _) = ed.cursor();
            let typed = buf_line(ed.buffer(), start_row).unwrap_or_default();
            for at_row in start_row..start_row + (session.count - 1) {
                let end = buf_line_chars(ed.buffer(), at_row);
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(at_row, end),
                    text: format!("\n{typed}"),
                });
            }
        } else if !inserted.is_empty() {
            // `[count]i` / `[count]A` repeat the typed text inline.
            for _ in 0..session.count - 1 {
                let (row, col) = ed.cursor();
                ed.mutate_edit(Edit::InsertStr {
                    at: Position::new(row, col),
                    text: inserted.clone(),
                });
            }
        }
    }
    if let InsertReason::BlockEdge {
        top,
        bot,
        col,
        pad,
        cursor_col,
    } = session.reason
    {
        // `I` / `A` from VisualBlock: replicate text across rows; cursor
        // stays at the block's LEFT column (vim leaves cursor there) —
        // NOT `col`, which for `A` is the append/typed column, one past
        // the block's right edge on a block wider than one column.
        // Ragged only ever applies to `A` (`pad == true`) — `I` is always
        // anchored at the block's LEFT column, unaffected by `$`.
        //
        // `cursor_col` is set to left-edge + 1 by both construction sites
        // (see their doc comments) because `leave_insert_to_normal_bridge`
        // unconditionally steps the cursor back one column right after
        // this returns — that step-back is what actually lands on the
        // left edge.
        let to_eol = pad && vim(ed).block_to_eol;
        if !inserted.is_empty() && !vim(ed).replaying {
            // `[count]I` / `[count]A` repeat the typed text `count` times on
            // EVERY row. The generic count-repeat branch above already
            // stacked the extra copies onto the TOP row (so `inserted` here
            // is one copy); replicate the fully-repeated run onto the rest.
            let repeated = inserted.repeat(session.count.max(1));
            if top < bot {
                replicate_block_text(ed, &repeated, top, bot, col, pad, to_eol);
                buf_set_cursor_rc(ed.buffer_mut(), top, cursor_col);
                ed.push_buffer_cursor_to_textarea();
            }
            // Record for dot-repeat (`:h v_.`, block `I`/`A`). `cols` is the
            // block width `A` appends past: `cursor_col == left + 1` and
            // (non-ragged) `col == right + 1`, so `cols == col + 1 -
            // cursor_col`. `I` doesn't need a width (it re-inserts at the
            // cursor column), so its `cols` is a placeholder.
            let cols = if pad {
                (col + 1).saturating_sub(cursor_col)
            } else {
                1
            };
            vim_mut(ed).last_change = Some(LastChange::VisualBlockInsert {
                text: repeated,
                rows: bot - top + 1,
                cols,
                to_eol,
                append: pad,
            });
        }
        return;
    }
    if let InsertReason::BlockChange { top, bot, col } = session.reason {
        // `c` from VisualBlock: replicate text across rows; cursor advances
        // to `col + ins_chars` (pre-step-back) so the Esc step-back lands
        // on the last typed char (col + ins_chars - 1), matching nvim.
        // Like `I`, vim `v_b_c` (`:h v_b_c`) skips rows that don't reach
        // the block's left column rather than padding them, and the
        // replicated text always lands at the LEFT column regardless of
        // a ragged right edge — verified against `nvim --headless`.
        if !inserted.is_empty() && !vim(ed).replaying {
            if top < bot {
                replicate_block_text(ed, &inserted, top, bot, col, false, false);
                let ins_chars = inserted.chars().count();
                let line_len = buf_line_chars(ed.buffer(), top);
                let target_col = (col + ins_chars).min(line_len);
                buf_set_cursor_rc(ed.buffer_mut(), top, target_col);
                ed.push_buffer_cursor_to_textarea();
            }
            // Patch the retyped text into the `VisualOp{Change, Block}` entry
            // recorded at the block-`c` start (mirrors the charwise/linewise
            // `AfterChange` patch, which `BlockChange` bypasses via its early
            // return).
            if let Some(LastChange::VisualOp { inserted: ins, .. }) =
                vim_mut(ed).last_change.as_mut()
            {
                *ins = Some(inserted);
            }
        }
        return;
    }
    if vim(ed).replaying {
        return;
    }
    match session.reason {
        InsertReason::Enter(entry) => {
            vim_mut(ed).last_change = Some(LastChange::InsertAt {
                entry,
                inserted,
                count: session.count,
            });
        }
        InsertReason::Open { above } => {
            vim_mut(ed).last_change = Some(LastChange::OpenLine { above, inserted });
        }
        InsertReason::AfterChange => {
            if let Some(
                LastChange::OpMotion { inserted: ins, .. }
                | LastChange::OpTextObj { inserted: ins, .. }
                | LastChange::LineOp { inserted: ins, .. }
                | LastChange::GnOp { inserted: ins, .. }
                | LastChange::VisualOp { inserted: ins, .. },
            ) = vim_mut(ed).last_change.as_mut()
            {
                *ins = Some(inserted);
            }
            // Vim `:h '[` / `:h ']`: on change, `[` = start of the
            // changed range (stashed before the cut), `]` = the cursor
            // at Esc time (last inserted char, before the step-back).
            // When nothing was typed cursor still sits at the change
            // start, satisfying vim's "both at start" parity for `c<m><Esc>`.
            if let Some(start) = vim_mut(ed).change_mark_start.take() {
                let end = ed.cursor();
                ed.set_mark('[', start);
                ed.set_mark(']', end);
            }
        }
        InsertReason::DeleteToEol => {
            vim_mut(ed).last_change = Some(LastChange::DeleteToEol {
                inserted: Some(inserted),
            });
        }
        InsertReason::ReplayOnly => {}
        InsertReason::BlockEdge { .. } => unreachable!("handled above"),
        InsertReason::BlockChange { .. } => unreachable!("handled above"),
        InsertReason::Replace => {
            // `R` overstrike: dot-repeat re-overtypes the same text at the
            // cursor (vim parity — not a delete-to-EOL).
            vim_mut(ed).last_change = Some(LastChange::ReplaceMode { text: inserted });
        }
    }
}
pub(crate) fn begin_insert<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    count: usize,
    reason: InsertReason,
) {
    // `nomodifiable`: silently refuse to enter insert/replace; stay in current mode.
    if !ed.settings().modifiable {
        return;
    }
    // BLAME view: pressing `i` exits blame (drops the overlay) but stays Normal.
    if ed.view_mode() == hjkl_engine::ViewMode::Blame {
        ed.set_view_mode(hjkl_engine::ViewMode::Normal);
        return;
    }
    let record = !matches!(reason, InsertReason::ReplayOnly);
    if record {
        ed.push_undo();
    }
    let reason = if vim(ed).replaying {
        InsertReason::ReplayOnly
    } else {
        reason
    };
    let (row, col) = ed.cursor();
    vim_mut(ed).insert_session = Some(InsertSession {
        count,
        row_min: row,
        row_max: row,
        before_rope: hjkl_engine::types::Query::rope(ed.buffer()),
        reason,
        start_row: row,
        start_col: col,
    });
    vim_mut(ed).mode = Mode::Insert;
    // Phase 6.3: keep current_mode in sync for callers that bypass step().
    vim_mut(ed).current_mode = hjkl_engine::VimMode::Insert;
    drop_blame_if_left_normal(ed);
}
/// `:set undobreak` semantics for insert-mode motions. When the
/// toggle is on, a non-character keystroke that moves the cursor
/// (arrow keys, Home/End, mouse click) ends the current undo group
/// and starts a new one mid-session. After this, a subsequent `u`
/// in normal mode reverts only the post-break run, leaving the
/// pre-break edits in place — matching vim's behaviour.
///
/// Implementation: snapshot the current buffer onto the undo stack
/// (the new break point) and reset the active `InsertSession`'s
/// `before_lines` so `finish_insert_session`'s diff window only
/// captures the post-break run for `last_change` / dot-repeat.
///
/// During replay we skip the break — replay shouldn't pollute the
/// undo stack with intra-replay snapshots.
pub(crate) fn break_undo_group_in_insert<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
) {
    if !ed.settings().undo_break_on_motion {
        return;
    }
    if vim(ed).replaying {
        return;
    }
    if vim(ed).insert_session.is_none() {
        return;
    }
    ed.push_undo();
    let before_rope = hjkl_engine::types::Query::rope(ed.buffer());
    let row = hjkl_engine::types::Cursor::cursor(ed.buffer()).line as usize;
    if let Some(ref mut session) = vim_mut(ed).insert_session {
        session.before_rope = before_rope;
        session.row_min = row;
        session.row_max = row;
    }
}
/// Word-boundary undo break for [`hjkl_engine::UndoGranularity::Word`].
///
/// Called from [`insert_char_bridge`] (before inserting `next`) and from
/// [`insert_newline_bridge`] (pass `next = '\n'`).
///
/// **Heuristic:** a break is inserted when:
/// - `next` is a non-whitespace char **and** the char immediately before
///   the cursor is whitespace (or the cursor is at column 0 but not the
///   session-start position — i.e. the user has already typed something
///   and then navigated or wrapped to column 0). This corresponds to
///   "the first character of a new word just after whitespace."
/// - `next` is `'\n'` (newline always starts a new undo unit).
///
/// **No break on the session's very first char:** `begin_insert` already
/// pushed an undo snapshot; breaking again would create an empty entry.
/// We detect "first char" by comparing the current cursor position to the
/// session's `(start_row, start_col)`.
///
/// During replay (`vim(ed).replaying`) or when there is no active insert
/// session, this is a complete no-op — the vim path is unchanged.
///
/// When `undo_granularity == InsertSession` this function returns
/// immediately, adding zero calls to the hot path.
pub(crate) fn maybe_word_undo_break<H: hjkl_engine::types::Host>(
    ed: &mut Editor<hjkl_buffer::View, H>,
    next: char,
) {
    use hjkl_engine::UndoGranularity;
    use hjkl_engine::buf_helpers::{buf_cursor_pos, buf_line};

    // Fast-path: default (vim) granularity → no-op.
    if ed.settings().undo_granularity != UndoGranularity::Word {
        return;
    }
    // No-op during replay or when there is no active insert session.
    if vim(ed).replaying {
        return;
    }
    let session = match vim(ed).insert_session.as_ref() {
        Some(s) => s,
        None => return,
    };

    let cursor = buf_cursor_pos(ed.buffer());

    // Skip the very first inserted char (begin_insert already snapshotted).
    let is_first_pos = cursor.row == session.start_row && cursor.col == session.start_col;
    if is_first_pos {
        return;
    }

    // Newline always breaks.
    let should_break = if next == '\n' {
        true
    } else if next.is_whitespace() {
        // Whitespace chars do not start a new word.
        false
    } else {
        // Non-whitespace: break only when the char immediately before the
        // cursor is whitespace (entering a word from whitespace territory).
        let prev_char = buf_line(ed.buffer(), cursor.row)
            .as_deref()
            .and_then(|line| line.chars().nth(cursor.col.wrapping_sub(1)));
        match prev_char {
            // Previous char is whitespace → this is a word start.
            Some(p) if p.is_whitespace() => true,
            // cursor.col == 0 means we arrived here from another line:
            // that too is a word-start boundary (newline already handled
            // above for the \n itself; this handles the first char on the
            // new line after the newline was inserted as a prior break).
            None if cursor.col == 0 => false, // col-0 on first position covered by start check above
            _ => false,
        }
    };

    if should_break {
        // Reuse the existing mid-session break machinery: push a snapshot
        // and reset session.before_rope + row_min/row_max.
        ed.push_undo();
        let before_rope = hjkl_engine::types::Query::rope(ed.buffer());
        let row = cursor.row;
        if let Some(ref mut session) = vim_mut(ed).insert_session {
            session.before_rope = before_rope;
            session.row_min = row;
            session.row_max = row;
        }
    }
}
#[cfg(test)]
mod comment_continuation_tests {
    use super::*;
    use hjkl_buffer::View;
    use hjkl_engine::{DefaultHost, Editor, Options};

    fn make_editor_with_lang(lang: &str, content: &str) -> Editor<View, DefaultHost> {
        let buf = View::from_str(content);
        let host = DefaultHost::new();
        let opts = Options {
            filetype: lang.to_string(),
            formatoptions: "ro".to_string(),
            ..Options::default()
        };
        crate::vim::vim_editor(buf, host, opts)
    }

    #[test]
    fn detect_rust_doc_comment() {
        let result = detect_comment_on_line("rust", "/// foo bar");
        assert!(result.is_some());
        let (indent, prefix) = result.unwrap();
        assert_eq!(indent, "");
        assert_eq!(prefix, "/// ");
    }

    #[test]
    fn detect_rust_inner_doc_comment() {
        let result = detect_comment_on_line("rust", "//! crate docs");
        assert!(result.is_some());
        let (_, prefix) = result.unwrap();
        assert_eq!(prefix, "//! ");
    }

    #[test]
    fn detect_rust_plain_comment() {
        let result = detect_comment_on_line("rust", "// normal comment");
        assert!(result.is_some());
        let (_, prefix) = result.unwrap();
        assert_eq!(prefix, "// ");
    }

    #[test]
    fn detect_indented_comment() {
        let result = detect_comment_on_line("rust", "    // indented");
        assert!(result.is_some());
        let (indent, prefix) = result.unwrap();
        assert_eq!(indent, "    ");
        assert_eq!(prefix, "// ");
    }

    #[test]
    fn detect_python_hash() {
        let result = detect_comment_on_line("python", "# comment");
        assert!(result.is_some());
        let (_, prefix) = result.unwrap();
        assert_eq!(prefix, "# ");
    }

    #[test]
    fn detect_lua_double_dash() {
        let result = detect_comment_on_line("lua", "-- a lua comment");
        assert!(result.is_some());
        let (_, prefix) = result.unwrap();
        assert_eq!(prefix, "-- ");
    }

    #[test]
    fn detect_non_comment_is_none() {
        assert!(detect_comment_on_line("rust", "let x = 1;").is_none());
        assert!(detect_comment_on_line("python", "x = 1").is_none());
    }

    #[test]
    fn detect_bare_double_slash_still_matches() {
        // A line that is exactly `//` with nothing after.
        assert!(detect_comment_on_line("rust", "//").is_some());
    }

    #[test]
    fn rust_doc_before_plain() {
        // `///` must match before `//`.
        let result = detect_comment_on_line("rust", "/// outer doc");
        let (_, prefix) = result.unwrap();
        assert_eq!(prefix, "/// ", "/// must match before //");
    }

    #[test]
    fn continue_comment_returns_prefix_for_comment_row() {
        let ed = make_editor_with_lang("rust", "/// hello\n");
        let cont = continue_comment(ed.buffer(), ed.settings(), 0);
        assert_eq!(cont, Some("/// ".to_string()));
    }

    #[test]
    fn continue_comment_returns_none_for_non_comment() {
        let ed = make_editor_with_lang("rust", "let x = 1;\n");
        let cont = continue_comment(ed.buffer(), ed.settings(), 0);
        assert!(cont.is_none());
    }

    #[test]
    fn continue_comment_returns_none_when_filetype_empty() {
        let buf = View::from_str("// hello\n");
        let host = DefaultHost::new();
        // filetype defaults to "" in Options::default().
        let ed = crate::vim::vim_editor(buf, host, Options::default());
        let cont = continue_comment(ed.buffer(), ed.settings(), 0);
        assert!(cont.is_none());
    }
}
#[cfg(test)]
mod comment_toggle_tests {
    use super::*;
    use hjkl_buffer::View;
    use hjkl_engine::{DefaultHost, Editor, Options};

    fn make_rust_editor(content: &str) -> Editor<View, DefaultHost> {
        let buf = View::from_str(content);
        let host = DefaultHost::new();
        let opts = Options {
            filetype: "rust".to_string(),
            ..Options::default()
        };
        crate::vim::vim_editor(buf, host, opts)
    }

    fn line(ed: &Editor<View, DefaultHost>, row: usize) -> String {
        buf_line(ed.buffer(), row).unwrap_or_default()
    }

    // ── gcc: toggle comment on current line ──────────────────────────────────

    #[test]
    fn gcc_comments_rust_line() {
        let mut ed = make_rust_editor("let x = 1;");
        ed.toggle_comment_range(0, 0);
        assert_eq!(line(&ed, 0), "// let x = 1;");
    }

    #[test]
    fn gcc_uncomments_rust_line() {
        let mut ed = make_rust_editor("// let x = 1;");
        ed.toggle_comment_range(0, 0);
        assert_eq!(line(&ed, 0), "let x = 1;");
    }

    #[test]
    fn gcc_indent_preserving() {
        // Marker inserted after leading whitespace, not at column 0.
        let mut ed = make_rust_editor("    let x = 1;");
        ed.toggle_comment_range(0, 0);
        assert_eq!(line(&ed, 0), "    // let x = 1;");
    }

    #[test]
    fn gcc_indent_preserving_uncomment() {
        let mut ed = make_rust_editor("    // let x = 1;");
        ed.toggle_comment_range(0, 0);
        assert_eq!(line(&ed, 0), "    let x = 1;");
    }

    // ── Multi-line toggle ────────────────────────────────────────────────────

    #[test]
    fn toggle_multi_line_all_uncommented() {
        let content = "let a = 1;\nlet b = 2;\nlet c = 3;";
        let mut ed = make_rust_editor(content);
        ed.toggle_comment_range(0, 2);
        assert_eq!(line(&ed, 0), "// let a = 1;");
        assert_eq!(line(&ed, 1), "// let b = 2;");
        assert_eq!(line(&ed, 2), "// let c = 3;");
    }

    #[test]
    fn toggle_multi_line_all_commented() {
        let content = "// let a = 1;\n// let b = 2;\n// let c = 3;";
        let mut ed = make_rust_editor(content);
        ed.toggle_comment_range(0, 2);
        assert_eq!(line(&ed, 0), "let a = 1;");
        assert_eq!(line(&ed, 1), "let b = 2;");
        assert_eq!(line(&ed, 2), "let c = 3;");
    }

    // ── Mixed state → all gets commented (vim-commentary parity) ────────────

    #[test]
    fn toggle_mixed_state_comments_all() {
        // 3 uncommented + 2 commented → all 5 get commented.
        let content = "let a = 1;\n// let b = 2;\nlet c = 3;\n// let d = 4;\nlet e = 5;";
        let mut ed = make_rust_editor(content);
        ed.toggle_comment_range(0, 4);
        for r in 0..5 {
            assert!(
                line(&ed, r).trim_start().starts_with("//"),
                "row {r} not commented: {:?}",
                line(&ed, r)
            );
        }
    }

    // ── Blank lines skipped ──────────────────────────────────────────────────

    #[test]
    fn blank_lines_not_commented() {
        let content = "let a = 1;\n\nlet b = 2;";
        let mut ed = make_rust_editor(content);
        ed.toggle_comment_range(0, 2);
        assert_eq!(line(&ed, 0), "// let a = 1;");
        assert_eq!(line(&ed, 1), ""); // blank — untouched
        assert_eq!(line(&ed, 2), "// let b = 2;");
    }

    // ── Python hash comments ─────────────────────────────────────────────────

    #[test]
    fn python_comment_toggle() {
        let buf = View::from_str("x = 1\ny = 2");
        let host = DefaultHost::new();
        let opts = Options {
            filetype: "python".to_string(),
            ..Options::default()
        };
        let mut ed = crate::vim::vim_editor(buf, host, opts);
        ed.toggle_comment_range(0, 1);
        assert_eq!(line(&ed, 0), "# x = 1");
        assert_eq!(line(&ed, 1), "# y = 2");
        // Toggle back.
        ed.toggle_comment_range(0, 1);
        assert_eq!(line(&ed, 0), "x = 1");
        assert_eq!(line(&ed, 1), "y = 2");
    }

    // ── commentstring override ───────────────────────────────────────────────

    #[test]
    fn commentstring_override_via_setting() {
        let buf = View::from_str("hello world");
        let host = DefaultHost::new();
        let opts = Options {
            filetype: "rust".to_string(),
            ..Options::default()
        };
        let mut ed = crate::vim::vim_editor(buf, host, opts);
        // Override with a custom marker.
        ed.settings_mut().commentstring = "# %s".to_string();
        ed.toggle_comment_range(0, 0);
        assert_eq!(line(&ed, 0), "# hello world");
    }

    // ── Unknown language → no-op ─────────────────────────────────────────────

    #[test]
    fn unknown_lang_no_op() {
        let buf = View::from_str("hello");
        let host = DefaultHost::new();
        let opts = Options::default(); // filetype = ""
        let mut ed = crate::vim::vim_editor(buf, host, opts);
        ed.toggle_comment_range(0, 0);
        // Should be unchanged — no comment string for "".
        assert_eq!(line(&ed, 0), "hello");
    }
}
