use super::*;

#[test]
fn poll_grammar_loads_with_no_pending_events_returns_false() {
    let mut app = App::new(None, false, None, None).unwrap();
    // No grammar loads queued — poll should return false (no redraw needed).
    let needs_redraw = app.poll_grammar_loads();
    assert!(!needs_redraw, "empty event queue should not request redraw");
}

#[test]
fn syntax_off_clears_installed_spans_and_disables_recompute() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Seed buffer content so row 0 exists, then install spans we can clear.
    seed_buffer(&mut app, "fn main() {}\n");
    app.active_editor_mut()
        .install_ratatui_syntax_spans(&[vec![(0, 1, ratatui::style::Style::default())]]);
    let has_spans_before = app
        .active_editor()
        .buffer_spans()
        .iter()
        .any(|row| !row.is_empty());
    assert!(has_spans_before, "precondition: spans must be non-empty");

    app.dispatch_ex("syntax off");

    assert!(!app.syntax_enabled, ":syntax off must flip the flag");
    let has_spans_after = app
        .active_editor()
        .buffer_spans()
        .iter()
        .any(|row| !row.is_empty());
    assert!(
        !has_spans_after,
        ":syntax off must drop installed spans (got: {:?})",
        app.active_editor().buffer_spans()
    );
}

#[test]
fn syntax_on_re_enables_flag() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("syntax off");
    assert!(!app.syntax_enabled);
    app.dispatch_ex("syntax on");
    assert!(app.syntax_enabled, ":syntax on must flip the flag back");
}

#[test]
fn syntax_enable_alias_works() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("syntax off");
    app.dispatch_ex("syntax enable");
    assert!(app.syntax_enabled, ":syntax enable is an alias for on");
}

#[test]
fn syntax_disable_alias_works() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("syntax disable");
    assert!(!app.syntax_enabled, ":syntax disable is an alias for off");
}

#[test]
fn syntax_unknown_subcommand_is_noop_and_keeps_state() {
    let mut app = App::new(None, false, None, None).unwrap();
    let before = app.syntax_enabled;
    // vim-permissive: subcommands like `:syntax sync` / `:syntax clear`
    // are accepted without error and leave state alone.
    app.dispatch_ex("syntax sync");
    assert_eq!(app.syntax_enabled, before);
}

#[test]
fn syn_abbrev_resolves_to_syntax() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("syn off");
    assert!(!app.syntax_enabled, ":syn off must resolve to :syntax off");
}

/// Regression (audit R2 fix 2): a terminal resize must request a syntax
/// recompute so newly exposed rows (from a grow) get spans on the very next
/// frame instead of staying unhighlighted until an unrelated keypress
/// happens to set `pending_recompute`. Both `Event::Resize` arms in
/// `App::run` now share `App::handle_resize`, which this test drives
/// directly (the arms live inline in the terminal-backed loop and aren't
/// otherwise reachable from a unit test).
#[test]
fn resize_sets_pending_recompute() {
    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "fn main() {}\n");
    app.pending_recompute = false;

    app.handle_resize(120, 40);

    assert!(
        app.pending_recompute,
        "resize must request a recompute so newly exposed rows get syntax spans"
    );
    let vp = app.active_editor().host().viewport();
    assert_eq!(vp.width, 120);
    assert_eq!(vp.height, 40 - STATUS_LINE_HEIGHT);
}

// ── Open-time filetype detection through the seam ────────────────────────────

/// Build a slot for a temp file with `content`, through the real `build_slot`
/// path. The language directory has remote loading disabled so no test in the
/// hermetic CI lane can trigger a network clone+compile — installed grammars
/// still attach (Cached), missing ones resolve to Unknown without a load.
fn slot_with_file(content: &str, name: &str, modeline: bool) -> BufferSlot {
    let td = tempfile::tempdir().unwrap();
    let path = td.path().join(name);
    std::fs::write(&path, content).unwrap();

    let mut directory = hjkl_lang::LanguageDirectory::new().expect("language directory");
    directory.set_allow_remote(false);
    let directory = std::sync::Arc::new(directory);
    let theme = std::sync::Arc::new(hjkl_bonsai::DotFallbackTheme::dark());
    let mut syntax = crate::syntax::layer_with_theme(theme, directory);

    build_slot(
        &mut syntax,
        1,
        Some(path),
        &hjkl_app::config::Config::default(),
        Some((modeline, 5)),
        &hjkl_app::swap::SwapRoot::Xdg,
    )
    .expect("build_slot must succeed for a readable temp file")
}

#[test]
fn extensionless_shebang_sets_filetype() {
    let slot = slot_with_file("#!/usr/bin/env python3\nprint('hi')\n", "script", true);
    assert_eq!(
        slot.settings.filetype, "python",
        "an extensionless file with a python shebang must detect as python"
    );
}

#[test]
fn extensionless_modeline_ft_sets_filetype() {
    let slot = slot_with_file("# vim: ft=bash\nprintf 'hi'\n", "script", true);
    assert_eq!(
        slot.settings.filetype, "bash",
        "a modeline ft= must detect for extensionless files"
    );
}

#[test]
fn modeline_shell_filetypes_preserve_settings_identity() {
    for filetype in ["sh", "zsh"] {
        let slot = slot_with_file(&format!("# vim: ft={filetype}\necho hi\n"), "script", true);
        assert_eq!(
            slot.settings.filetype, filetype,
            "a modeline ft={filetype} must preserve the Neovim filetype",
        );
    }
}

#[test]
fn modeline_beats_shebang() {
    // vim parity: an explicit modeline ft= outranks the shebang heuristic,
    // even when the shebang contradicts it.
    let slot = slot_with_file("#!/usr/bin/env bash\n# vi: ft=python\n", "script", true);
    assert_eq!(slot.settings.filetype, "python");
}

#[test]
fn modeline_disabled_ignores_ft() {
    let slot = slot_with_file("# vim: ft=bash\n", "script", false);
    assert_eq!(
        slot.settings.filetype, "",
        "with modeline off, a modeline ft= must not set the filetype"
    );
}

#[test]
fn known_basename_sets_filetype() {
    let slot = slot_with_file("all:\n\techo hi\n", "Makefile", true);
    assert_eq!(slot.settings.filetype, "make");
}

#[test]
fn extension_still_wins_through_the_seam() {
    let slot = slot_with_file("fn main() {}\n", "main.rs", true);
    assert_eq!(
        slot.settings.filetype, "rust",
        "a known extension must still resolve through the seam"
    );
}

#[test]
fn extensionless_no_signal_stays_plain() {
    let slot = slot_with_file("just some prose\n", "notes", true);
    assert_eq!(
        slot.settings.filetype, "",
        "a file with no signal must stay plain text"
    );
}

#[test]
fn unvalidated_modeline_ft_flows_through() {
    // A modeline ft= that names no grammar is still the file's filetype
    // (vim semantics) — the grammar attach resolves to Unknown downstream.
    let slot = slot_with_file("# vim: ft=bogus_lang\n", "script", true);
    assert_eq!(slot.settings.filetype, "bogus_lang");
}

// ── Live `:set filetype=` propagation ────────────────────────────────────────

#[test]
fn set_ft_updates_slot_template_and_editor() {
    // vim's FileType-autocmd behaviour: `:set ft=…` must reach the slot's
    // settings template (the seam's source of truth) and re-attach, not just
    // sit on the focused window's editor. `bogus_lang` names no grammar, so
    // the attach resolves to Unknown without touching the network — the
    // hermetic-lane contract.
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.active().settings.filetype, "");

    app.dispatch_ex("set ft=bogus_lang");

    assert_eq!(
        app.active().settings.filetype,
        "bogus_lang",
        ":set ft= must write the slot's settings template"
    );
    assert_eq!(
        app.active_editor().settings().filetype,
        "bogus_lang",
        ":set ft= must keep the focused window's editor in sync"
    );
}

#[test]
fn set_shell_filetypes_preserves_settings_identity() {
    let mut app = App::new(None, false, None, None).unwrap();

    for filetype in ["sh", "zsh"] {
        app.dispatch_ex(&format!("set ft={filetype}"));
        assert_eq!(
            app.active().settings.filetype,
            filetype,
            ":set ft={filetype} must preserve the Neovim filetype",
        );
    }
}

#[test]
fn set_filetype_alias_and_clear_work() {
    let mut app = App::new(None, false, None, None).unwrap();

    app.dispatch_ex("set filetype=bogus_lang");
    assert_eq!(app.active().settings.filetype, "bogus_lang");

    // `:set ft=` (empty) clears the filetype; the slot template follows.
    app.dispatch_ex("set ft=");
    assert_eq!(
        app.active().settings.filetype,
        "",
        ":set ft= must clear the slot's filetype"
    );
}

#[test]
fn set_ft_query_does_not_reattach() {
    // `:set ft?` reads the option — it must not clobber the detected
    // filetype with a re-attach.
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("set ft=bogus_lang");
    app.dispatch_ex("set ft?");
    assert_eq!(app.active().settings.filetype, "bogus_lang");
}
