//! Harness-enablement probes for the differential fuzzers.
//!
//! - [`undo_history_is_cleared_after_seeding`] — Part 1: nvim's undo history
//!   must be cleared after fixture seeding, so replayed `u` / `<C-r>` diff
//!   honestly against hjkl's in-process editor (whose seed records no undo
//!   entry). Gated on nvim being installed.
//! - [`exfuzz_command_set_dispatches_in_hjkl`] /
//!   [`exfuzz_command_set_runs_in_nvim`] — Part 2: every ex-command shape the
//!   exfuzz generator emits must dispatch on both sides; a shape that errors
//!   here gets dropped from the generator.
//! - [`search_replay_moves_cursor_in_process`] — Part 3: `/` / `?` search
//!   prompts resolve fully inside the engine (the host `prompt_search` hook
//!   is never called), so `/pat<CR>` replays through the in-process driver.
//! - [`fold_keys_replay_in_process`] / [`gq_reflow_replays_in_process`] —
//!   Part 4: `z` fold keys and `gq` reflow keys replay through the in-process
//!   driver. The engine applies fold ops to the in-tree view itself
//!   (`Editor::apply_fold_op`), so the driver needs no host machinery.

use hjkl_compat_oracle::test_host::TestHost;

/// Minimal [`hjkl_compat_oracle::OracleCase`] with no option pins.
fn case(
    initial_buffer: &str,
    keys: &str,
    cursor: (usize, usize),
) -> hjkl_compat_oracle::OracleCase {
    hjkl_compat_oracle::OracleCase {
        name: "probe".to_string(),
        initial_buffer: initial_buffer.to_string(),
        initial_cursor: cursor,
        keys: keys.to_string(),
        expected_buffer: String::new(),
        expected_cursor: None,
        expected_mode: None,
        expected_register: None,
        shiftwidth: None,
        expandtab: None,
        textwidth: None,
        autoindent: None,
        foldmethod: None,
    }
}

/// `case` variant with option pins for fold / reflow probing.
fn pinned_case(
    initial_buffer: &str,
    keys: &str,
    cursor: (usize, usize),
    foldmethod: &str,
    textwidth: Option<usize>,
) -> hjkl_compat_oracle::OracleCase {
    hjkl_compat_oracle::OracleCase {
        name: "fold_probe".to_string(),
        initial_buffer: initial_buffer.to_string(),
        initial_cursor: cursor,
        keys: keys.to_string(),
        expected_buffer: String::new(),
        expected_cursor: None,
        expected_mode: None,
        expected_register: None,
        shiftwidth: Some(4),
        expandtab: Some(true),
        textwidth,
        autoindent: Some(false),
        foldmethod: Some(foldmethod.to_string()),
    }
}

// ── Part 1: undo history cleared after seeding ──────────────────────────────

/// The RPC `set_lines` seed is ONE undoable change in nvim, so without the
/// post-seed undo clear a replayed `u` rolls the buffer back to the pre-seed
/// empty state while hjkl's in-process editor (whose `View::from_str` seed
/// records nothing) treats `u` as a no-op — every `u` diverges.
#[tokio::test(flavor = "multi_thread")]
async fn undo_history_is_cleared_after_seeding() {
    if !hjkl_compat_oracle::nvim_available() {
        eprintln!("skipping: nvim not installed");
        return;
    }

    let mut ucase = case("alpha\nbeta\n", "u<Esc>", (0, 0));
    ucase.name = "undo_cleared_probe".to_string();
    let nvim = hjkl_compat_oracle::nvim_driver::run_case(&ucase)
        .await
        .unwrap();
    assert_eq!(
        nvim.buffer, "alpha\nbeta\n",
        "`u` after seeding must be a no-op (undo history not cleared?)"
    );
    assert_eq!(nvim.cursor, (0, 0));
    assert_eq!(nvim.mode, "normal");

    // The whole point: hjkl's in-process driver must agree — its seed records
    // no undo entry, so `u` is already a no-op there.
    let hjkl = hjkl_compat_oracle::hjkl_driver::run_case(&ucase).unwrap();
    assert_eq!(hjkl.buffer, nvim.buffer);
    assert_eq!(hjkl.cursor, nvim.cursor);
    assert_eq!(hjkl.mode, nvim.mode);

    // Redo arm: `u<C-r>` on the cleared tree is two no-ops and must leave the
    // seeded buffer untouched too.
    let mut redo_case = case("alpha\nbeta\n", "u<C-r><Esc>", (0, 0));
    redo_case.name = "undo_redo_cleared_probe".to_string();
    let nvim_redo = hjkl_compat_oracle::nvim_driver::run_case(&redo_case)
        .await
        .unwrap();
    assert_eq!(nvim_redo.buffer, "alpha\nbeta\n");
    let hjkl_redo = hjkl_compat_oracle::hjkl_driver::run_case(&redo_case).unwrap();
    assert_eq!(hjkl_redo.buffer, nvim_redo.buffer);
}

// ── Part 2: exfuzz command grammar probes ───────────────────────────────────

/// Every ex-command shape `examples/exfuzz.rs` may generate. A shape that
/// errors on either side here gets dropped from the generator.
const EXFUZZ_COMMANDS: &[&str] = &[
    // Line ops (bare and counted).
    "d",
    "y",
    "j",
    "2d",
    "3y",
    "2j",
    // Substitution, first match and global flag.
    "s/foo/bar/",
    "s/foo/bar/g",
    // Global / vglobal delete.
    "g/foo/d",
    "v/foo/d",
    // :normal with 1-3 concatenated keys from {j, k, x, dd, A!}.
    // (`p` is deliberately NOT in the set: `:normal p` with an empty
    // register errors on nvim with E353 — a shape that errors on one side
    // gets dropped. `A!<Esc>` is likewise out: nvim does NOT expand `<Esc>`
    // key-notation inside a `nvim.command` string — it inserts the literal
    // text — while hjkl's `decode_macro` does, so the shape could never diff
    // honestly. `A!` alone is honest: `:normal` returns to Normal mode at the
    // end of the invocation on both engines.)
    "normal j",
    "normal k",
    "normal x",
    "normal dd",
    "normal A!",
    "normal jk",
    "normal xdd",
    "normal A!k",
    "normal jkx",
];

fn exfuzz_probe_case() -> hjkl_compat_oracle::OracleCase {
    hjkl_compat_oracle::OracleCase {
        name: "exfuzz_probe".to_string(),
        initial_buffer: "foo bar\nbaz qux\nhello world\n".to_string(),
        initial_cursor: (0, 0),
        keys: String::new(),
        expected_buffer: String::new(),
        expected_cursor: None,
        expected_mode: None,
        expected_register: None,
        shiftwidth: Some(4),
        expandtab: Some(true),
        textwidth: None,
        autoindent: Some(false),
        foldmethod: Some("manual".to_string()),
    }
}

/// Build an in-process editor exactly like `hjkl_driver::run_case` does.
fn build_editor(
    case: &hjkl_compat_oracle::OracleCase,
) -> hjkl_engine::Editor<hjkl_buffer::View, TestHost> {
    let buffer = hjkl_buffer::View::from_str(&case.initial_buffer);
    let mut editor = hjkl_vim::vim_editor(buffer, TestHost::new(), hjkl_engine::Options::default());
    editor.set_viewport_height(24);
    let (init_row, init_col) = case.initial_cursor;
    editor.jump_cursor(init_row, init_col);
    editor
}

/// hjkl side: every shape must dispatch through `hjkl_ex::try_dispatch` —
/// return `Some(effect)` and not an error (an `Error` effect is a shape that
/// would report as a divergence instead of a real diff).
#[test]
fn exfuzz_command_set_dispatches_in_hjkl() {
    let probe_case = exfuzz_probe_case();
    let reg = hjkl_ex::default_registry::<TestHost>();
    for cmd in EXFUZZ_COMMANDS {
        let mut editor = build_editor(&probe_case);
        let result = hjkl_ex::try_dispatch(&reg, &mut editor, cmd);
        assert!(
            result.is_some() && !matches!(result, Some(hjkl_ex::ExEffect::Error(_))),
            "{cmd}: expected Some(non-error) dispatch, got {result:?}"
        );
    }
}

/// nvim side: every shape must run without an RPC error.
#[tokio::test(flavor = "multi_thread")]
async fn exfuzz_command_set_runs_in_nvim() {
    if !hjkl_compat_oracle::nvim_available() {
        eprintln!("skipping: nvim not installed");
        return;
    }
    let probe_case = exfuzz_probe_case();
    for cmd in EXFUZZ_COMMANDS {
        let outcome =
            hjkl_compat_oracle::nvim_driver::run_commands(&probe_case, std::slice::from_ref(cmd))
                .await;
        if let Err(e) = outcome {
            panic!("{cmd}: nvim.command failed: {e}");
        }
    }
}

// ── Part 3: search replay through the in-process driver ─────────────────────

/// The vim FSM resolves `/` / `?` search fully from `search_prompt_state`
/// (`hjkl-vim/src/search_prompt.rs`) — the host's `prompt_search` hook is
/// never called — so `/foo<CR>` should replay through
/// `hjkl_driver::run_case`. This is the probe that gates adding `/` / `?` /
/// `n` / `N` to difffuzz's keystroke alphabet.
#[test]
fn search_replay_moves_cursor_in_process() {
    // `/foo<CR>` from (0,0): lands on the first `foo`, row 1 col 0.
    let out = hjkl_compat_oracle::hjkl_driver::run_case(&case(
        "alpha\nfoo bar\nbaz foo\n",
        "/foo<CR>",
        (0, 0),
    ))
    .unwrap();
    assert_eq!(
        out.cursor,
        (1, 0),
        "`/foo<CR>` must move to the first match"
    );
    assert_eq!(out.mode, "normal");

    // `n` repeats forward: second match at row 2 col 4.
    let out = hjkl_compat_oracle::hjkl_driver::run_case(&case(
        "alpha\nfoo bar\nbaz foo\n",
        "/foo<CR>n",
        (0, 0),
    ))
    .unwrap();
    assert_eq!(out.cursor, (2, 4), "`n` must advance to the next match");

    // `N` repeats backward: back to the first match.
    let out = hjkl_compat_oracle::hjkl_driver::run_case(&case(
        "alpha\nfoo bar\nbaz foo\n",
        "/foo<CR>nN",
        (0, 0),
    ))
    .unwrap();
    assert_eq!(
        out.cursor,
        (1, 0),
        "`N` must move back to the previous match"
    );

    // `?foo<CR>` backward from below the last match lands on it.
    let out = hjkl_compat_oracle::hjkl_driver::run_case(&case(
        "alpha\nfoo bar\nbaz foo\n",
        "?foo<CR>",
        (2, 6),
    ))
    .unwrap();
    assert_eq!(
        out.cursor,
        (2, 4),
        "`?foo<CR>` must move to the previous match"
    );
}

// ── Part 4: fold / gq replay probes ─────────────────────────────────────────

/// Fold keys must replay through the in-process driver and the fold op must
/// actually land in the view: the engine applies `FoldOp`s in-tree via
/// `Editor::apply_fold_op` (hosts only drain `take_fold_ops` for their own
/// observation), so no host machinery is needed. `zfj` creates a closed fold
/// over rows 0-1 and parks the cursor on the fold start; `zc` with no fold
/// under the cursor is a harmless no-op — both match nvim (probed against
/// nvim 0.12.4).
#[test]
fn fold_keys_replay_in_process() {
    let case = pinned_case("a\nb\nc\nd\ne\nf\n", "zfj", (0, 0), "manual", None);
    let out = hjkl_compat_oracle::hjkl_driver::run_case(&case).unwrap();
    assert_eq!(
        out.cursor,
        (0, 0),
        "`zfj` parks the cursor on the fold start"
    );

    let case = pinned_case("a\nb\nc\nd\ne\nf\n", "zfjzc", (0, 0), "manual", None);
    let out = hjkl_compat_oracle::hjkl_driver::run_case(&case).unwrap();
    assert_eq!(out.cursor, (0, 0));

    // `zc` with no folds at all must be a harmless no-op (both engines).
    let case = pinned_case("a\nb\nc\n", "zc", (0, 0), "manual", None);
    let out = hjkl_compat_oracle::hjkl_driver::run_case(&case).unwrap();
    assert_eq!(out.cursor, (0, 0));
}

/// `gqq` must replay in-process with textwidth pinned and reflow the buffer
/// identically to nvim (probed against nvim 0.12.4: same buffer, same
/// cursor).
#[test]
fn gq_reflow_replays_in_process() {
    let case = pinned_case(
        "one two three four five six seven eight\n",
        "gqq",
        (0, 0),
        "manual",
        Some(20),
    );
    let out = hjkl_compat_oracle::hjkl_driver::run_case(&case).unwrap();
    assert_eq!(out.buffer, "one two three four\nfive six seven eight\n");
    assert_eq!(out.cursor, (1, 0));
}
