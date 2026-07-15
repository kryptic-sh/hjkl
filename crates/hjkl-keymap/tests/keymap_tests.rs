//! Integration tests for hjkl-keymap.

use hjkl_keymap::{Chord, ChordParseError, KeyCode, KeyEvent, KeyModifiers, KeyResolve, Keymap};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

/// Test-local mode discriminator. `Mode` is now a trait — consumers pick their
/// own concrete type. These two variants cover what the test suite needs.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
enum TestMode {
    Normal,
    Insert,
}

use TestMode as Mode;

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
    let mut km: Keymap<&str, TestMode> = Keymap::new(' ');
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
    let mut km: Keymap<u32, TestMode> = Keymap::new(' ');
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
    let mut km: Keymap<u32, TestMode> = Keymap::new(' ');
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
    let mut km: Keymap<u32, TestMode> = Keymap::new(' ');
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
    let mut km: Keymap<u32, TestMode> = Keymap::new(' ');
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
    let mut km: Keymap<u32, TestMode> = Keymap::new(' ');
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
    let mut km: Keymap<&str, TestMode> = Keymap::new(' ');
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
    let mut km: Keymap<&str, TestMode> = Keymap::new(' ');
    // No keys fed — buffer is empty.
    let result = km.pop(Mode::Normal);
    assert_eq!(result, None);
}

// ── Mode isolation ────────────────────────────────────────────────────────────

#[test]
fn normal_binding_not_visible_from_insert() {
    let mut km: Keymap<u32, TestMode> = Keymap::new(' ');
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
    let mut km: Keymap<&str, TestMode> = Keymap::new(' ');
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
    let mut km: Keymap<&str, TestMode> = Keymap::new(' ');
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
    // View = "<leader>" (prefix-only — only "<leader>g" is bound).
    // timeout_resolve must NOT drain: user is mid-chord.
    let mut km: Keymap<&str, TestMode> = Keymap::new(' ');
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
    // View = "g" where both "g" (terminal) and "gd" (deeper) are bound.
    // timeout_resolve must fire the shorter "g" binding.
    let mut km: Keymap<&str, TestMode> = Keymap::new(' ');
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
    assert!(
        km.pending(Mode::Normal).is_empty(),
        "buffer must be drained"
    );
}

#[test]
fn timeout_resolve_empty_buffer_returns_unbound_empty() {
    let mut km: Keymap<&str, TestMode> = Keymap::new(' ');
    km.add(Mode::Normal, "<leader>g", "git", "git").unwrap();
    let r = km.timeout_resolve(Mode::Normal);
    assert!(matches!(r, KeyResolve::Unbound(ref v) if v.is_empty()));
}

// ── Predicate-gated bindings (Phase 1 — issue #120) ──────────────────────────

/// Predicate false → binding is unbound; key not consumed.
#[test]
fn predicate_false_binding_is_unbound() {
    let mut km: Keymap<u32, TestMode> = Keymap::new(' ');
    km.add_if(Mode::Normal, "x", 42, "gated", || false).unwrap();

    let now = Instant::now();
    let r = km.feed(Mode::Normal, char_ev('x'), now);
    assert!(
        matches!(r, KeyResolve::Unbound(_)),
        "predicate=false must yield Unbound, got {r:?}"
    );
}

/// Predicate true → binding fires normally.
#[test]
fn predicate_true_binding_fires() {
    let mut km: Keymap<u32, TestMode> = Keymap::new(' ');
    km.add_if(Mode::Normal, "x", 99, "gated", || true).unwrap();

    let now = Instant::now();
    let r = km.feed(Mode::Normal, char_ev('x'), now);
    assert!(
        matches!(r, KeyResolve::Match(ref b) if b.action == 99),
        "predicate=true must yield Match, got {r:?}"
    );
}

/// Predicate state changes between resolve calls: first false → unbound,
/// then true → fires.
#[test]
fn predicate_state_change_between_resolves() {
    let flag = Arc::new(AtomicBool::new(false));

    let mut km: Keymap<u32, TestMode> = Keymap::new(' ');
    let flag_clone = Arc::clone(&flag);
    km.add_if(Mode::Normal, "x", 7, "togglable", move || {
        flag_clone.load(Ordering::SeqCst)
    })
    .unwrap();

    let now = Instant::now();

    // Predicate false: unbound.
    let r = km.feed(Mode::Normal, char_ev('x'), now);
    assert!(
        matches!(r, KeyResolve::Unbound(_)),
        "expected Unbound when predicate=false, got {r:?}"
    );

    // Flip flag.
    flag.store(true, Ordering::SeqCst);

    // Predicate true: fires.
    let r = km.feed(Mode::Normal, char_ev('x'), now);
    assert!(
        matches!(r, KeyResolve::Match(ref b) if b.action == 7),
        "expected Match when predicate=true, got {r:?}"
    );
}

/// Ambiguous chord where the complete-arm has a false predicate: the
/// ambiguous terminal is treated as absent, so the result is Pending
/// (not Ambiguous) — the chord must be extended to resolve.
#[test]
fn ambiguous_predicate_false_complete_arm_falls_through_to_pending() {
    let mut km: Keymap<u32, TestMode> = Keymap::new(' ');
    // Both "g" (terminal, predicate false) and "gd" (deeper, unconditional).
    km.add_if(Mode::Normal, "g", 10, "g-gated", || false)
        .unwrap();
    km.add(Mode::Normal, "gd", 20, "gd unconditional").unwrap();

    let now = Instant::now();
    // Feeding "g": exact match exists but predicate is false; deeper binding
    // exists. The result must be Pending (not Ambiguous) because the
    // complete arm is invisible.
    let r = km.feed(Mode::Normal, char_ev('g'), now);
    assert!(
        matches!(r, KeyResolve::Pending),
        "false-predicate complete arm must yield Pending (not Ambiguous), got {r:?}"
    );

    // Extending to "gd" must fire the unconditional binding.
    let r = km.feed(Mode::Normal, char_ev('d'), now);
    assert!(
        matches!(r, KeyResolve::Match(ref b) if b.action == 20),
        "gd must fire unconditionally, got {r:?}"
    );
}

// ── Non-vim mode discriminators (issue #1) ───────────────────────────────────

#[test]
fn keymap_works_with_helix_style_mode_set() {
    // Helix has Normal / Select / Insert — no OpPending, no CommandLine.
    // Proves the generic Mode trait accommodates non-vim modal vocabularies.
    #[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
    enum HelixMode {
        Normal,
        Select,
        Insert,
    }

    let mut km: Keymap<&str, HelixMode> = Keymap::new(' ');
    km.add(HelixMode::Normal, "w", "next-word", "next word")
        .unwrap();
    km.add(
        HelixMode::Select,
        "w",
        "select-next-word",
        "select next word",
    )
    .unwrap();

    let now = Instant::now();
    let r = km.feed(HelixMode::Normal, char_ev('w'), now);
    assert!(matches!(r, KeyResolve::Match(ref b) if b.action == "next-word"));

    let r = km.feed(HelixMode::Select, char_ev('w'), now);
    assert!(matches!(r, KeyResolve::Match(ref b) if b.action == "select-next-word"));

    // Insert mode has no bindings; should return Unbound.
    let r = km.feed(HelixMode::Insert, char_ev('w'), now);
    assert!(matches!(r, KeyResolve::Unbound(_)));
}
