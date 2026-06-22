use super::*;
use std::time::{Duration, Instant};

fn make_app() -> App {
    App::new(None, false, None, None).unwrap()
}

#[test]
fn scroll_anim_none_returns_none() {
    let app = make_app();
    assert!(app.scroll_anim_render_top(0).is_none());
    assert!(!app.scroll_anim_expired());
}

#[test]
fn scroll_anim_wrong_win_id_returns_none() {
    let mut app = make_app();
    app.scroll_anim = Some(crate::app::ScrollAnim {
        win_id: 99,
        start_top: 0,
        target_top: 20,
        started_at: Instant::now(),
        duration: Duration::from_millis(200),
    });
    // win_id 0 != 99
    assert!(app.scroll_anim_render_top(0).is_none());
}

#[test]
fn scroll_anim_mid_flight_returns_some_between_start_and_target() {
    let mut app = make_app();
    app.scroll_anim = Some(crate::app::ScrollAnim {
        win_id: 0,
        start_top: 0,
        target_top: 20,
        started_at: Instant::now(),
        duration: Duration::from_millis(200),
    });
    // t ≈ 0, so rendered top should be near start (0), certainly < target (20)
    let top = app.scroll_anim_render_top(0);
    assert!(top.is_some(), "mid-flight should return Some");
    assert!(top.unwrap() < 20, "rendered top must be < target");
    assert!(!app.scroll_anim_expired());
}

#[test]
fn scroll_anim_zero_duration_expired() {
    let mut app = make_app();
    app.scroll_anim = Some(crate::app::ScrollAnim {
        win_id: 0,
        start_top: 0,
        target_top: 20,
        started_at: Instant::now(),
        duration: Duration::ZERO,
    });
    // zero duration → immediately expired → render_top returns None
    assert!(app.scroll_anim_render_top(0).is_none());
    assert!(app.scroll_anim_expired());
}
