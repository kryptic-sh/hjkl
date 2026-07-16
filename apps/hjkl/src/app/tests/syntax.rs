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
        .install_ratatui_syntax_spans(vec![vec![(0, 1, ratatui::style::Style::default())]]);
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
