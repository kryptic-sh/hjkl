use super::*;

#[test]
fn poll_grammar_loads_with_no_pending_events_returns_false() {
    let mut app = App::new(None, false, None, None).unwrap();
    // No grammar loads queued — poll should return false (no redraw needed).
    let needs_redraw = app.poll_grammar_loads();
    assert!(!needs_redraw, "empty event queue should not request redraw");
}
