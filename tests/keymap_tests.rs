//! Integration tests for hjkl-keymap.

use hjkl_keymap::{
    Chord, ChordParseError, KeyCode, KeyEvent, KeyModifiers, KeyResolve, Keymap, Mode,
};
use std::time::Instant;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn char_ev(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn ctrl_ev(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CTRL)
}

// ── Chord parse round-trips ───────────────────────────────────────────────────

#[test]
fn round_trip_leader_gs() {
    let leader = ' ';
    let chord = Chord::parse("<leader>gs", leader).unwrap();
    assert_eq!(chord.to_notation(leader), "<leader>gs");
}

#[test]
fn round_trip_ctrl_x() {
    let leader = ' ';
    let chord = Chord::parse("<C-x>", leader).unwrap();
    assert_eq!(chord.to_notation(leader), "<C-x>");
}

#[test]
fn round_trip_shift_tab() {
    let leader = ' ';
    let chord = Chord::parse("<S-Tab>", leader).unwrap();
    assert_eq!(chord.to_notation(leader), "<S-Tab>");
}

#[test]
fn round_trip_ctrl_shift_tab() {
    let leader = ' ';
    let chord = Chord::parse("<C-S-Tab>", leader).unwrap();
    // Modifiers can be in either order in the representation.
    let notation = chord.to_notation(leader);
    assert!(
        notation == "<C-S-Tab>" || notation == "<S-C-Tab>",
        "unexpected notation: {notation}"
    );
}

#[test]
fn round_trip_mixed() {
    let leader = ' ';
    let chord = Chord::parse("<C-w>h", leader).unwrap();
    assert_eq!(chord.to_notation(leader), "<C-w>h");
}

#[test]
fn parse_error_unclosed() {
    let result = Chord::parse("<C-w", ' ');
    assert!(matches!(result, Err(ChordParseError::UnclosedAngle(_))));
}

// ── Keymap::add + feed leaf chord ─────────────────────────────────────────────

#[test]
fn leaf_chord_matches() {
    let mut km: Keymap<&str> = Keymap::new(' ');
    km.add(Mode::Normal, "gd", "goto_def", "goto definition")
        .unwrap();

    let now = Instant::now();
    let r1 = km.feed(Mode::Normal, char_ev('g'), now);
    assert!(
        matches!(r1, KeyResolve::Pending),
        "expected Pending after 'g'"
    );

    let r2 = km.feed(Mode::Normal, char_ev('d'), now);
    assert!(
        matches!(r2, KeyResolve::Match(b) if b.action == "goto_def"),
        "expected Match(goto_def)"
    );
}

// ── Pending then match ────────────────────────────────────────────────────────

#[test]
fn pending_then_match() {
    let mut km: Keymap<u32> = Keymap::new(' ');
    km.add(Mode::Normal, "gd", 1, "gd").unwrap();

    let now = Instant::now();
    let r = km.feed(Mode::Normal, char_ev('g'), now);
    assert!(matches!(r, KeyResolve::Pending));

    let r = km.feed(Mode::Normal, char_ev('d'), now);
    assert!(matches!(r, KeyResolve::Match(b) if b.action == 1));
}

// ── Unbound dead-end ─────────────────────────────────────────────────────────

#[test]
fn unbound_gj_when_only_gd_bound() {
    let mut km: Keymap<u32> = Keymap::new(' ');
    km.add(Mode::Normal, "gd", 1, "gd").unwrap();

    let now = Instant::now();
    km.feed(Mode::Normal, char_ev('g'), now);
    let r = km.feed(Mode::Normal, char_ev('j'), now);

    match r {
        KeyResolve::Unbound(keys) => {
            assert_eq!(keys.len(), 2, "expected both g and j in unbound list");
            assert_eq!(keys[0], char_ev('g'));
            assert_eq!(keys[1], char_ev('j'));
        }
        other => panic!("expected Unbound, got {other:?}"),
    }
}

// ── Ambiguous resolution ─────────────────────────────────────────────────────

#[test]
fn ambiguous_when_both_g_and_gd_bound() {
    let mut km: Keymap<u32> = Keymap::new(' ');
    km.add(Mode::Normal, "g", 10, "g action").unwrap();
    km.add(Mode::Normal, "gd", 20, "gd action").unwrap();

    let now = Instant::now();
    let r = km.feed(Mode::Normal, char_ev('g'), now);
    assert!(
        matches!(r, KeyResolve::Ambiguous),
        "expected Ambiguous after g"
    );

    // Feed 'd' — should match gd.
    let r = km.feed(Mode::Normal, char_ev('d'), now);
    assert!(matches!(r, KeyResolve::Match(b) if b.action == 20));
}

#[test]
fn timeout_after_ambiguous_resolves_shorter() {
    let mut km: Keymap<u32> = Keymap::new(' ');
    km.add(Mode::Normal, "g", 10, "g action").unwrap();
    km.add(Mode::Normal, "gd", 20, "gd action").unwrap();

    let now = Instant::now();
    let r = km.feed(Mode::Normal, char_ev('g'), now);
    assert!(matches!(r, KeyResolve::Ambiguous));

    // Timeout without another key — should resolve to Match(g action).
    let r = km.timeout_resolve(Mode::Normal);
    assert!(matches!(r, KeyResolve::Match(b) if b.action == 10));
}

// ── children for which-key ────────────────────────────────────────────────────

#[test]
fn children_lists_gd_gt() {
    let mut km: Keymap<u32> = Keymap::new(' ');
    km.add(Mode::Normal, "gd", 1, "goto def").unwrap();
    km.add(Mode::Normal, "gt", 2, "next tab").unwrap();
    km.add(Mode::Normal, "gT", 3, "prev tab").unwrap();

    let prefix = Chord::parse("g", ' ').unwrap();
    let mut children = km.children(Mode::Normal, &prefix);
    children.sort_by_key(|(ev, _)| match ev.code {
        hjkl_keymap::KeyCode::Char(c) => c,
        _ => '\0',
    });

    assert_eq!(children.len(), 3);
    let codes: Vec<char> = children
        .iter()
        .filter_map(|(ev, _)| {
            if let hjkl_keymap::KeyCode::Char(c) = ev.code {
                Some(c)
            } else {
                None
            }
        })
        .collect();
    assert!(codes.contains(&'d'), "missing gd");
    assert!(codes.contains(&'t'), "missing gt");
    assert!(codes.contains(&'T'), "missing gT");
}

// ── Keymap::pop ───────────────────────────────────────────────────────────────

#[test]
fn pop_removes_last_key() {
    let mut km: Keymap<&str> = Keymap::new(' ');
    km.add(Mode::Normal, "<leader>gs", "git_status", "git status")
        .unwrap();

    let now = Instant::now();
    // Feed leader then 'g' — buffer has two keys.
    km.feed(Mode::Normal, char_ev(' '), now);
    km.feed(Mode::Normal, char_ev('g'), now);
    assert_eq!(km.pending(Mode::Normal).len(), 2);

    // Pop should remove 'g' and return it.
    let removed = km.pop(Mode::Normal);
    assert_eq!(removed, Some(char_ev('g')));
    assert_eq!(km.pending(Mode::Normal).len(), 1);
    assert_eq!(km.pending(Mode::Normal)[0], char_ev(' '));
}

#[test]
fn pop_returns_none_on_empty_buffer() {
    let mut km: Keymap<&str> = Keymap::new(' ');
    // No keys fed — buffer is empty.
    let result = km.pop(Mode::Normal);
    assert_eq!(result, None);
}

// ── Mode isolation ────────────────────────────────────────────────────────────

#[test]
fn normal_binding_not_visible_from_insert() {
    let mut km: Keymap<u32> = Keymap::new(' ');
    km.add(Mode::Normal, "gd", 1, "goto def").unwrap();

    let now = Instant::now();
    km.feed(Mode::Insert, char_ev('g'), now);
    let r = km.feed(Mode::Insert, char_ev('d'), now);

    assert!(
        matches!(r, KeyResolve::Unbound(_)),
        "insert mode should not see normal binding"
    );
}

// ── Leader chords ────────────────────────────────────────────────────────────

#[test]
fn leader_chord_resolves() {
    let mut km: Keymap<&str> = Keymap::new(' ');
    km.add(Mode::Normal, "<leader>gs", "git_status", "git status")
        .unwrap();

    let now = Instant::now();
    km.feed(Mode::Normal, char_ev(' '), now); // leader
    km.feed(Mode::Normal, char_ev('g'), now);
    let r = km.feed(Mode::Normal, char_ev('s'), now);

    assert!(matches!(r, KeyResolve::Match(b) if b.action == "git_status"));
}

#[test]
fn ctrl_w_chord_resolves() {
    let mut km: Keymap<&str> = Keymap::new(' ');
    km.add(Mode::Normal, "<C-w>h", "focus_left", "focus left")
        .unwrap();

    let now = Instant::now();
    km.feed(Mode::Normal, ctrl_ev('w'), now);
    let r = km.feed(Mode::Normal, char_ev('h'), now);

    assert!(matches!(r, KeyResolve::Match(b) if b.action == "focus_left"));
}

// ── timeout_resolve semantics ──────────────────────────────────────────────────

#[test]
fn timeout_resolve_keeps_buffer_when_pure_prefix() {
    // Buffer = "<leader>" (prefix-only — only "<leader>g" is bound).
    // timeout_resolve must NOT drain: user is mid-chord.
    let mut km: Keymap<&str> = Keymap::new(' ');
    km.add(Mode::Normal, "<leader>g", "git", "git submenu")
        .unwrap();

    let now = Instant::now();
    let leader = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
    km.feed(Mode::Normal, leader, now);
    assert_eq!(km.pending(Mode::Normal).len(), 1);

    let r = km.timeout_resolve(Mode::Normal);
    assert!(
        matches!(r, KeyResolve::Unbound(ref v) if v.is_empty()),
        "pure-prefix timeout_resolve must return Unbound(empty), got {r:?}"
    );
    assert_eq!(
        km.pending(Mode::Normal).len(),
        1,
        "buffer must be preserved for pure-prefix state"
    );
}

#[test]
fn timeout_resolve_fires_ambiguous_shorter_binding() {
    // Buffer = "g" where both "g" (terminal) and "gd" (deeper) are bound.
    // timeout_resolve must fire the shorter "g" binding.
    let mut km: Keymap<&str> = Keymap::new(' ');
    km.add(Mode::Normal, "g", "g_action", "g").unwrap();
    km.add(Mode::Normal, "gd", "gd_action", "gd").unwrap();

    let now = Instant::now();
    km.feed(Mode::Normal, char_ev('g'), now);
    assert_eq!(km.pending(Mode::Normal).len(), 1);

    let r = km.timeout_resolve(Mode::Normal);
    assert!(
        matches!(r, KeyResolve::Match(ref b) if b.action == "g_action"),
        "ambiguous timeout_resolve must fire the terminal binding, got {r:?}"
    );
    assert!(km.pending(Mode::Normal).is_empty(), "buffer must be drained");
}

#[test]
fn timeout_resolve_empty_buffer_returns_unbound_empty() {
    let mut km: Keymap<&str> = Keymap::new(' ');
    km.add(Mode::Normal, "<leader>g", "git", "git").unwrap();
    let r = km.timeout_resolve(Mode::Normal);
    assert!(matches!(r, KeyResolve::Unbound(ref v) if v.is_empty()));
}
