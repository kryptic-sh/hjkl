use super::*;

#[test]
fn poll_grammar_loads_clears_expired_error() {
    let mut app = App::new(None, false, None, None).unwrap();
    // Inject an error that is already expired (timestamp 10 s in the past).
    app.grammar_load_error = Some(GrammarLoadError {
        name: "fake-lang".to_string(),
        message: "connection refused".to_string(),
        at: std::time::Instant::now() - Duration::from_secs(10),
    });
    let needs_redraw = app.poll_grammar_loads();
    assert!(needs_redraw, "expired error should request redraw");
    assert!(
        app.grammar_load_error.is_none(),
        "expired error should be cleared"
    );
}
