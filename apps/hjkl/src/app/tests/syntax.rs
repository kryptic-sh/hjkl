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
    app.active_mut()
        .editor
        .install_ratatui_syntax_spans(vec![vec![(0, 1, ratatui::style::Style::default())]]);
    let has_spans_before = app
        .active()
        .editor
        .buffer_spans()
        .iter()
        .any(|row| !row.is_empty());
    assert!(has_spans_before, "precondition: spans must be non-empty");

    app.dispatch_ex("syntax off");

    assert!(!app.syntax_enabled, ":syntax off must flip the flag");
    let has_spans_after = app
        .active()
        .editor
        .buffer_spans()
        .iter()
        .any(|row| !row.is_empty());
    assert!(
        !has_spans_after,
        ":syntax off must drop installed spans (got: {:?})",
        app.active().editor.buffer_spans()
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

#[test]
fn perf_stats_record_accumulates_and_resets() {
    use std::time::Duration;
    let mut stats = crate::app::PerfStats::default();
    stats.record("a::path", Duration::from_micros(100));
    stats.record("a::path", Duration::from_micros(50));
    stats.record("b::path", Duration::from_micros(200));
    let entries = stats.entries();
    let a = entries.iter().find(|e| e.name == "a::path").unwrap();
    assert_eq!(a.count, 2);
    assert_eq!(a.last_us, 50);
    assert_eq!(a.max_us, 100);
    assert_eq!(a.total_us, 150);
    let top = stats.top_by_last(1);
    assert_eq!(top[0].name, "b::path", "highest last_us must sort first");
    stats.reset();
    assert!(stats.entries().is_empty(), "reset must clear all entries");
}
