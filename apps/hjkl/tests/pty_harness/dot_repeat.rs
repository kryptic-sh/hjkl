//! E2e tests for dot-repeat count-override behaviour (audit A2).
//!
//! `[count].` must override the count recorded on a `LastChange`, including
//! insert-mode changes (`LastChange::InsertAt`). Regression for a bug where
//! `replay_last_change`'s `InsertAt` arm replayed with the raw recorded
//! count instead of running it through the same `scaled(...)` override every
//! other `LastChange` arm applies.

use super::harness::TerminalSession;

/// `3ihello<Esc>` then `5.` — the explicit count on `.` must override the
/// recorded count of 3, inserting "hello" 5 more times (not 3). Each
/// "hello" contributes exactly one 'h', so counting 'h' characters on the
/// line is a robust way to count insertions: it doesn't depend on the
/// gutter width or on exactly where the cursor sits (and thus where the
/// replayed text gets spliced in) after `Esc`.
#[test]
fn count_override_on_dot_repeats_insert_mode_change() {
    let mut s = TerminalSession::spawn();

    // `3ihello<Esc>` inserts "hello" 3 times.
    s.keys("3ihello<Esc>");
    let after_insert = s.line(0);
    assert_eq!(
        after_insert.matches('h').count(),
        3,
        "3ihello<Esc> must insert 'hello' 3 times, got line: {after_insert:?}"
    );

    // `5.` must override the recorded count of 3 with 5 — vim replaces, it
    // does not multiply. If the override is ignored (the bug), this stays 3.
    s.keys("5.");
    let after_repeat = s.line(0);
    assert_eq!(
        after_repeat.matches('h').count(),
        8,
        "5. must override the recorded count 3 with 5 (3 + 5 = 8 total 'hello' insertions), got line: {after_repeat:?}"
    );
}

/// `2iX<Esc>` then `.` (no explicit count) must reuse the recorded count of
/// 2 — regression guard that the count-override defaults correctly when the
/// user types no count before `.`.
#[test]
fn dot_repeat_without_count_reuses_recorded_count() {
    let mut s = TerminalSession::spawn();

    s.keys("2iX<Esc>");
    let after_insert = s.line(0);
    assert_eq!(
        after_insert.matches('X').count(),
        2,
        "2iX<Esc> must insert 'X' twice, got line: {after_insert:?}"
    );

    s.keys(".");
    let after_repeat = s.line(0);
    assert_eq!(
        after_repeat.matches('X').count(),
        4,
        "bare . must reuse the recorded count 2 (2 + 2 = 4 'X's), got line: {after_repeat:?}"
    );
}
