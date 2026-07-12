//! Insert-mode behaviour: scrolloff correction, nomodifiable/readonly gating,
//! undo granularity, and multibyte autoclose.
//!
//! Relocated from hjkl-engine's inline test modules (#267). The insert
//! primitives these drive (`insert_char`, `enter_insert_i`,
//! `leave_insert_to_normal`, ...) now live on [`hjkl_vim::VimEditorExt`], and a
//! blanket trait impl on `hjkl_engine::Editor` is unreachable from an in-crate
//! unit test — the `crate::Editor` identity there is a distinct type. A
//! `tests/` target links hjkl-engine as an external crate, so the impl
//! resolves.

mod insert_mode_scrolloff_tests {
    use hjkl_buffer::Buffer;
    use hjkl_engine::Editor;
    use hjkl_engine::FsmMode as Mode;
    use hjkl_engine::types::{DefaultHost, Host, Options};
    use hjkl_vim::VimEditorExt;

    fn ed_with_lines(line_count: usize) -> Editor<Buffer, DefaultHost> {
        let text = (0..line_count)
            .map(|i| format!("row{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let buf = Buffer::from_str(&text);
        let mut e = Editor::new(buf, DefaultHost::new(), Options::default());
        // Viewport: 20 rows tall, starts at top.
        let vp = e.host_mut().viewport_mut();
        vp.width = 80;
        vp.height = 20;
        vp.top_row = 0;
        vp.top_col = 0;
        e.set_viewport_height(20);
        e.vim.mode = Mode::Insert;
        e
    }

    /// Regression: holding Enter in insert mode used to scroll the cursor
    /// off the viewport because `insert_newline` (called from the app's
    /// `dispatch_insert_key`) bypasses the FSM `step` that runs
    /// `ensure_cursor_in_scrolloff`. The post-mutation helper now runs
    /// scrolloff for every insert primitive — the cursor must stay
    /// within `scrolloff` rows of the bottom edge.
    #[test]
    fn insert_newline_keeps_cursor_in_scrolloff() {
        let mut e = ed_with_lines(200);
        // Park cursor at the bottom edge of the viewport (row 19).
        e.set_cursor_doc(19, 0);
        // Press Enter 50 times. Cursor moves down each newline; without
        // scrolloff the cursor would slide off the bottom of the
        // viewport at row 20+ and the user would type blind.
        for _ in 0..50 {
            e.insert_newline();
        }
        let (cursor_row, _) = e.cursor();
        let vp = e.host().viewport();
        let cursor_screen_row = cursor_row.saturating_sub(vp.top_row);
        let scrolloff = e.settings().scrolloff;
        let margin = scrolloff.min(vp.height as usize - 1) / 2;
        let max_screen_row = vp.height as usize - 1 - margin;
        assert!(
            cursor_screen_row <= max_screen_row,
            "cursor screen row {cursor_screen_row} exceeded scrolloff bound {max_screen_row} \
             (cursor_row={cursor_row}, vp.top_row={vp_top}, vp.height={vp_h})",
            vp_top = vp.top_row,
            vp_h = vp.height,
        );
    }

    /// Same check for `insert_arrow(Down)` — cursor-only motion that also
    /// must trigger scrolloff.
    #[test]
    fn insert_arrow_down_keeps_cursor_in_scrolloff() {
        let mut e = ed_with_lines(200);
        e.set_cursor_doc(19, 0);
        for _ in 0..50 {
            e.insert_arrow(hjkl_engine::vim::InsertDir::Down);
        }
        let (cursor_row, _) = e.cursor();
        let vp = e.host().viewport();
        let cursor_screen_row = cursor_row.saturating_sub(vp.top_row);
        let scrolloff = e.settings().scrolloff;
        let margin = scrolloff.min(vp.height as usize - 1) / 2;
        let max_screen_row = vp.height as usize - 1 - margin;
        assert!(
            cursor_screen_row <= max_screen_row,
            "cursor screen row {cursor_screen_row} exceeded scrolloff bound {max_screen_row}"
        );
    }

    /// Scrolloff must be measured in SCREEN rows, not doc rows: a closed
    /// fold between `top_row` and the cursor collapses its hidden body to
    /// one screen row. The old doc-row arithmetic left the cursor only a
    /// few SCREEN rows below the top (it scrolled as if the fold's hidden
    /// rows still occupied screen space), violating the top margin. The
    /// fold-aware path keeps the cursor's screen row inside
    /// `[margin, height - 1 - margin]`.
    #[test]
    fn scrolloff_is_fold_aware_screen_rows() {
        let mut e = ed_with_lines(200);
        // Close a fold whose body sits between the viewport top and the
        // cursor: rows 11..=25 are hidden (15 doc rows collapse to 0).
        e.buffer_mut().add_fold(10, 25, true);
        // Jump below the fold. The doc-based viewport pre-scroll parks the
        // cursor near the *top* of the screen because the fold ate the
        // space above it; scrolloff must pull `top_row` back so the cursor
        // is at least `margin` screen rows from the top.
        e.set_cursor_doc(30, 0);
        e.ensure_cursor_in_scrolloff();

        let vp = e.host().viewport();
        let (cursor_row, _) = e.cursor();
        // Fold-aware cursor screen row = count of VISIBLE rows in
        // [top_row, cursor_row).
        let screen_row = (vp.top_row..cursor_row)
            .filter(|&r| !e.buffer().is_row_hidden(r))
            .count();
        let height = vp.height as usize;
        let margin = e.settings().scrolloff.min(height.saturating_sub(1) / 2);
        let bottom_bound = height - 1 - margin;
        // Old (doc-row) code produced screen_row = 4 here — below the top
        // margin of 5. The fix keeps it within the screen-row band.
        assert!(
            screen_row >= margin,
            "cursor screen row {screen_row} is inside the top margin {margin} \
             (top_row={top}, cursor_row={cursor_row})",
            top = vp.top_row,
        );
        assert!(
            screen_row <= bottom_bound,
            "cursor screen row {screen_row} exceeds bottom bound {bottom_bound}"
        );
        // Cursor itself must never be on a hidden row.
        assert!(!e.buffer().is_row_hidden(cursor_row));
    }

    /// A `G`-style jump to the bottom of a fold-heavy buffer must land the
    /// cursor on the last screen row (bottom-margin clamp) and keep it
    /// visible — exercising the O(height) fold-aware path that replaced the
    /// O(n²) per-step walk that made `G` laggy on real (open-fold) source.
    #[test]
    fn scrolloff_fold_big_jump_lands_at_bottom() {
        let mut e = ed_with_lines(400);
        // Several OPEN auto-fold-style ranges scattered through the file —
        // open folds don't hide rows but still route through the fold path
        // (this is the real Rust-file case: many folds, all open).
        for start in (0..390).step_by(10) {
            e.buffer_mut().add_fold(start, start + 5, false);
        }
        // Jump to the last line from the top.
        e.set_cursor_doc(399, 0);
        e.ensure_cursor_in_scrolloff();

        let vp = e.host().viewport();
        let (cursor_row, _) = e.cursor();
        let height = vp.height as usize;
        let screen_row = (vp.top_row..cursor_row)
            .filter(|&r| !e.buffer().is_row_hidden(r))
            .count();
        // Cursor visible and at the bottom (all folds open → screen == doc rows,
        // so the bottom row sits at height-1).
        assert!(
            screen_row < height,
            "cursor off-screen: screen_row={screen_row}"
        );
        assert_eq!(
            screen_row,
            height - 1,
            "G should bottom-align the cursor (top_row={}, cursor={cursor_row})",
            vp.top_row,
        );
    }

    /// Perf guard: scrolloff on a fold-heavy buffer must be O(height), not
    /// O(n²) in the jump distance. A `G`-to-bottom jump over 50 k rows with a
    /// fold every 10 lines must finish well under budget. The old
    /// re-walk-per-step path was ~50k × ~50k line reads = seconds-to-minutes
    /// even in debug; the O(height) path is microseconds. Budget is generous
    /// (200 ms) so it never false-fails on slow CI but still catches a
    /// reintroduced per-step rescan, which would blow past it by orders of
    /// magnitude.
    #[test]
    fn scrolloff_fold_big_jump_is_under_200ms() {
        let mut e = ed_with_lines(50_000);
        for start in (0..49_990).step_by(10) {
            e.buffer_mut().add_fold(start, start + 5, false);
        }
        e.set_cursor_doc(49_999, 0);
        let t = std::time::Instant::now();
        e.ensure_cursor_in_scrolloff();
        let elapsed = t.elapsed();
        assert!(
            elapsed.as_millis() < 200,
            "fold-heavy G-to-bottom took {elapsed:?}; budget 200 ms (catches \
             reintroduction of the O(n²) per-step screen-row rescan)"
        );
    }
}

mod modifiable_readonly_tests {
    use hjkl_buffer::Buffer;
    use hjkl_engine::Editor;
    use hjkl_engine::types::{DefaultHost, Options};
    use hjkl_vim::VimEditorExt;

    fn make_ed(content: &str) -> Editor<Buffer, DefaultHost> {
        let buf = Buffer::from_str(content);
        Editor::new(buf, DefaultHost::default(), Options::default())
    }

    // ── nomodifiable ──────────────────────────────────────────────────────────

    /// `nomodifiable` must block insert-mode entry: pressing `i` leaves mode Normal.
    #[test]
    fn nomodifiable_blocks_insert_entry() {
        let mut ed = make_ed("hello");
        ed.settings_mut().modifiable = false;
        ed.enter_insert_i(1);
        assert_eq!(
            ed.vim_mode(),
            hjkl_engine::VimMode::Normal,
            "nomodifiable must keep mode Normal after `i`"
        );
    }

    /// `nomodifiable` must block all edits via mutate_edit.
    #[test]
    fn nomodifiable_blocks_mutate_edit() {
        let mut ed = make_ed("hello");
        ed.settings_mut().modifiable = false;
        let result = ed.mutate_edit(hjkl_buffer::Edit::InsertStr {
            at: hjkl_buffer::Position::new(0, 0),
            text: "XXX".to_string(),
        });
        assert!(
            matches!(result, hjkl_buffer::Edit::InsertStr { ref text, .. } if text.is_empty()),
            "nomodifiable must swallow the edit"
        );
        assert_eq!(
            ed.buffer().content_joined().as_str(),
            "hello",
            "buffer must be unchanged"
        );
    }

    /// `nomodifiable` blocks Replace-mode entry too.
    #[test]
    fn nomodifiable_blocks_replace_mode_entry() {
        let mut ed = make_ed("hello");
        ed.settings_mut().modifiable = false;
        ed.enter_replace_mode(1);
        assert_eq!(
            ed.vim_mode(),
            hjkl_engine::VimMode::Normal,
            "nomodifiable must keep mode Normal after `R`"
        );
    }

    // ── readonly (modifiable=true) ────────────────────────────────────────────

    /// `readonly` does NOT block edits — the buffer is fully editable.
    #[test]
    fn readonly_allows_edits_via_mutate_edit() {
        let mut ed = make_ed("hello");
        ed.settings_mut().readonly = true;
        assert!(ed.is_readonly(), "readonly flag must be set");
        // mutate_edit must proceed normally when readonly is set.
        ed.mutate_edit(hjkl_buffer::Edit::InsertStr {
            at: hjkl_buffer::Position::new(0, 0),
            text: "X".to_string(),
        });
        assert_eq!(
            ed.buffer().content_joined().as_str(),
            "Xhello",
            "readonly must not block edits"
        );
    }

    /// `readonly` does NOT block insert-mode entry.
    #[test]
    fn readonly_allows_insert_mode_entry() {
        let mut ed = make_ed("hello");
        ed.settings_mut().readonly = true;
        ed.enter_insert_i(1);
        assert_eq!(
            ed.vim_mode(),
            hjkl_engine::VimMode::Insert,
            "readonly must allow entering Insert mode"
        );
    }

    // ── is_modifiable accessor ────────────────────────────────────────────────

    #[test]
    fn is_modifiable_default_true() {
        let ed = make_ed("");
        assert!(ed.is_modifiable());
    }

    #[test]
    fn is_modifiable_reflects_setting() {
        let mut ed = make_ed("");
        ed.settings_mut().modifiable = false;
        assert!(!ed.is_modifiable());
        ed.settings_mut().modifiable = true;
        assert!(ed.is_modifiable());
    }
}

mod undo_granularity_tests {
    use hjkl_buffer::Buffer;
    use hjkl_engine::Editor;
    use hjkl_engine::UndoGranularity;
    use hjkl_engine::types::{DefaultHost, Options};
    use hjkl_vim::VimEditorExt;

    fn make_ed(content: &str) -> Editor<Buffer, DefaultHost> {
        let buf = Buffer::from_str(content);
        Editor::new(buf, DefaultHost::default(), Options::default())
    }

    /// Helper: type a string char-by-char in insert mode.
    fn type_str(ed: &mut Editor<Buffer, DefaultHost>, s: &str) {
        for ch in s.chars() {
            ed.insert_char(ch);
        }
    }

    /// Helper: return current buffer content as a plain String.
    fn content(ed: &Editor<Buffer, DefaultHost>) -> String {
        ed.buffer().content_joined().to_string()
    }

    // ── InsertSession (vim default) ───────────────────────────────────────────

    /// With the default `InsertSession` granularity, a single `u` reverts the
    /// entire insert session — vim byte-identical behaviour.
    #[test]
    fn insert_session_granularity_single_undo_reverts_all() {
        let mut ed = make_ed("");
        assert_eq!(
            ed.settings().undo_granularity,
            UndoGranularity::InsertSession,
            "default must be InsertSession"
        );

        // Enter insert, type "foo bar baz", leave.
        ed.enter_insert_i(1);
        type_str(&mut ed, "foo bar baz");
        ed.leave_insert_to_normal();

        assert_eq!(content(&ed), "foo bar baz");

        // One undo step → back to empty (full session reverted).
        ed.undo();
        assert_eq!(
            content(&ed),
            "",
            "InsertSession: one undo must revert the entire session"
        );
    }

    /// A second `u` after the first in InsertSession mode has nothing left
    /// on the stack (the initial state was the baseline push_undo snapshot).
    /// The buffer stays empty.
    #[test]
    fn insert_session_granularity_second_undo_is_noop() {
        let mut ed = make_ed("");
        ed.enter_insert_i(1);
        type_str(&mut ed, "hello");
        ed.leave_insert_to_normal();
        ed.undo();
        let after_first = content(&ed);
        ed.undo(); // should be a no-op
        assert_eq!(
            content(&ed),
            after_first,
            "second undo must not change buffer when stack is exhausted"
        );
    }

    // ── Word granularity ─────────────────────────────────────────────────────

    /// With `Word` granularity, typing "foo bar baz" produces three undo
    /// units: "baz", "bar ", "foo".
    ///
    /// Observed chunking (heuristic: break at non-WS char following WS):
    ///
    /// - After 'b' of "bar": prev char was ' ' → break
    /// - After 'b' of "baz": prev char was ' ' → break
    ///
    /// Undo strides: "baz" → "bar " → "foo"
    #[test]
    fn word_granularity_undo_steps_by_word() {
        let mut ed = make_ed("");
        ed.settings_mut().undo_granularity = UndoGranularity::Word;

        ed.enter_insert_i(1);
        type_str(&mut ed, "foo bar baz");
        ed.leave_insert_to_normal();

        assert_eq!(content(&ed), "foo bar baz");

        // First undo: removes "baz" (the last word).
        ed.undo();
        let after1 = content(&ed);
        assert!(
            after1.ends_with("foo bar ") || after1 == "foo bar",
            "after first undo expected 'foo bar[ ]', got {after1:?}"
        );

        // Second undo: removes "bar " (or "bar").
        ed.undo();
        let after2 = content(&ed);
        assert!(
            after2 == "foo" || after2 == "foo ",
            "after second undo expected 'foo[ ]', got {after2:?}"
        );

        // Third undo: removes "foo" → empty.
        ed.undo();
        assert_eq!(content(&ed), "", "after third undo buffer must be empty");
    }

    /// Newline starts a new undo unit under Word granularity.
    #[test]
    fn word_granularity_newline_starts_new_unit() {
        let mut ed = make_ed("");
        ed.settings_mut().undo_granularity = UndoGranularity::Word;

        ed.enter_insert_i(1);
        type_str(&mut ed, "hello");
        ed.insert_newline();
        type_str(&mut ed, "world");
        ed.leave_insert_to_normal();

        // Should have at least "hello\nworld" in buffer.
        let full = content(&ed);
        assert!(
            full.contains("hello") && full.contains("world"),
            "buffer must contain both lines: {full:?}"
        );

        // First undo: should remove at least "world" (the post-newline text).
        ed.undo();
        let after1 = content(&ed);
        assert!(
            !after1.contains("world"),
            "after one undo 'world' should be gone; got {after1:?}"
        );
    }

    /// Verify that switching to Word granularity does NOT alter the behaviour
    /// of existing operations when no insert session is active (undo on an
    /// already-empty stack is a no-op regardless of granularity).
    #[test]
    fn word_granularity_undo_noop_on_empty_stack() {
        let mut ed = make_ed("hello");
        ed.settings_mut().undo_granularity = UndoGranularity::Word;
        // Do NOT enter insert — stack has nothing.
        let before = content(&ed);
        ed.undo();
        assert_eq!(
            content(&ed),
            before,
            "undo with empty stack must be no-op under Word granularity"
        );
    }
}

mod scan_tag_opener_multibyte_tests {
    use hjkl_buffer::Buffer;
    use hjkl_engine::types::Options;
    use hjkl_engine::{DefaultHost, Editor};
    use hjkl_vim::VimEditorExt;

    fn html_editor(content: &str) -> Editor<Buffer, DefaultHost> {
        let buf = Buffer::from_str(content);
        let host = DefaultHost::new();
        let mut ed = Editor::new(buf, host, Options::default());
        ed.settings_mut().filetype = "html".to_string();
        ed.settings_mut().autoclose_tag = true;
        ed.settings_mut().autopair = true;
        ed
    }

    /// Typing `>` after a multibyte char must not panic.
    ///
    /// With "ñ" in the buffer (ñ = 2 UTF-8 bytes), the cursor is at char
    /// col 1 (one past ñ).  `insert_char('>')` calls `scan_tag_opener` with
    /// `col = cursor.col = 1`.  Before the fix, `&line[..1]` landed inside
    /// the 2-byte ñ sequence → panic "byte index 1 is not a char boundary".
    #[test]
    fn autoclose_gt_after_multibyte_no_panic() {
        let mut ed = html_editor("ñ");
        // Cursor starts at col 0 (on ñ). Enter insert at end-of-line.
        ed.enter_insert_i(1);
        // Move to end (col 1, after ñ).
        ed.jump_cursor(0, 1);
        // Insert '>' — must not panic.
        ed.insert_char('>');
        // The `>` should be in the buffer (no autoclose tag fires for bare ">").
        let rope = ed.buffer().rope();
        let line = hjkl_buffer::rope_line_str(&rope, 0);
        assert!(line.contains('>'), "inserted > must appear in buffer");
    }

    /// Same repro via the direct tag-autoclose path.
    ///
    /// "ä<div" has a multibyte char at the start.  Positioning the cursor
    /// at char col 5 (after 'v') and inserting '>' exercises the
    /// scan_tag_opener branch.  Before the fix, `col = cursor.col = 5` and
    /// `&line[..5]` is byte index 5, which falls inside 'ä' (2 bytes at
    /// positions 0-1) — wait, 'ä'=2 bytes then '<','d','i','v' are 1 byte
    /// each, so byte 5 = 'v' (valid boundary).  Use a CJK char (3 bytes)
    /// to force a panic at a narrower position:
    ///
    /// "中<div>": 中 = 3 bytes; after '>', char col 5 → byte index 5.
    /// Bytes: 中=0,1,2  <=3  d=4  i=5  v=6  >=7.  Char index 4 = 'i', byte 4
    /// is safe. Need char 2 to map to byte 5 → that's inside '<'.
    ///
    /// Simplest panic case: "ñ>" (ñ=2 bytes, >=1 byte):
    /// char 0=ñ, char 1=>; cursor.col=1, &line[..1] = byte 1 = 0xb1 inside ñ → PANIC.
    #[test]
    fn autoclose_gt_direct_after_multibyte_no_panic() {
        // "ñ>" already in buffer — cursor at char col 1 (the '>').
        // We'll test by inserting '>' after 'ñ' from scratch.
        let mut ed = html_editor("ñ");
        ed.enter_insert_i(1);
        ed.jump_cursor(0, 1); // char col 1 = one past ñ
        // Insert '>' — before fix: scan_tag_opener("ñ>", 1) → &"ñ>"[..1] panics.
        ed.insert_char('>');
        let rope = ed.buffer().rope();
        let line = hjkl_buffer::rope_line_str(&rope, 0);
        assert!(
            line.contains('>'),
            "inserted > must appear in buffer, got: {line:?}"
        );
    }
}
