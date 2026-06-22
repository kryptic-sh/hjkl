use super::*;

use crate::app::keymap::HjklMode;
use hjkl_which_key::{Entry, entries_for};

// ── entries_for integration tests (app + engine merge) ────────────────────────

fn empty_keymap() -> hjkl_keymap::Keymap<crate::keymap_actions::AppAction, HjklMode> {
    hjkl_keymap::Keymap::new(' ')
}

#[test]
fn entries_include_engine_descriptors_at_root() {
    let km = empty_keymap();
    let entries = entries_for(&km, HjklMode::Normal, &[], ' ');
    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    for k in ["h", "j", "k", "l", "i", "a", "w", "b"] {
        assert!(keys.contains(&k), "entries_for missing engine key '{k}'");
    }
}

#[test]
fn entries_include_g_prefix_engine_children() {
    let km = empty_keymap();
    let entries = entries_for(
        &km,
        HjklMode::Normal,
        &[hjkl_keymap::KeyEvent::char('g')],
        ' ',
    );
    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    assert!(!entries.is_empty(), "g-prefix popup should be non-empty");
    assert!(keys.contains(&"g"), "g-prefix missing 'gg' entry");
    assert!(keys.contains(&"j"), "g-prefix missing 'gj' entry");
}

#[test]
fn entries_include_z_prefix_engine_children() {
    let km = empty_keymap();
    let entries = entries_for(
        &km,
        HjklMode::Normal,
        &[hjkl_keymap::KeyEvent::char('z')],
        ' ',
    );
    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    assert!(!entries.is_empty(), "z-prefix popup should be non-empty");
    assert!(keys.contains(&"z"), "z-prefix missing 'zz' entry");
}

#[test]
fn app_entry_shadows_engine_entry() {
    let mut km: hjkl_keymap::Keymap<crate::keymap_actions::AppAction, HjklMode> =
        hjkl_keymap::Keymap::new(' ');
    km.add(
        HjklMode::Normal,
        "i",
        crate::keymap_actions::AppAction::OpenFilePicker,
        "custom insert desc",
    )
    .expect("add failed");
    let entries = entries_for(&km, HjklMode::Normal, &[], ' ');
    let i_entry = entries.iter().find(|e| e.key == "i").expect("missing 'i'");
    assert_eq!(
        i_entry.desc, "custom insert desc",
        "app desc should override engine desc for 'i'"
    );
}

#[test]
fn entries_sorted_by_key() {
    let km = empty_keymap();
    let entries = entries_for(&km, HjklMode::Normal, &[], ' ');
    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "entries should be sorted by key string");
}

// ── #134: RHS desc format ─────────────────────────────────────────────────────

#[test]
fn nmap_leader_x_desc_shows_rhs_notation() {
    // After `:nmap <leader>x :echo hi<CR>`, the entry for `x` under the
    // leader prefix should show `"→ :echo hi<CR>"` (or the truncated form).
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex("nmap <leader>x :echo hi<CR>");

    let leader = app.config.editor.leader;
    // The <leader>x binding lives under the leader key prefix.
    let leader_prefix = vec![hjkl_keymap::KeyEvent::char(leader)];
    let entries = entries_for(&app.app_keymap, HjklMode::Normal, &leader_prefix, leader);

    let x_entry = entries
        .iter()
        .find(|e| e.key == "x")
        .expect("expected 'x' entry under leader prefix after :nmap <leader>x");

    // desc should start with "→ " and contain the rhs notation.
    assert!(
        x_entry.desc.starts_with("→ "),
        "desc should start with '→ ', got: {:?}",
        x_entry.desc
    );
    // The RHS notation encodes the key sequence — `:echo hi<CR>` becomes
    // something like `":echo<leader>hi<CR>"` depending on how spaces are
    // represented. Check that `:echo` and `hi` appear in the desc.
    assert!(
        x_entry.desc.contains(":echo") && x_entry.desc.contains("hi"),
        "desc should contain ':echo' and 'hi', got: {:?}",
        x_entry.desc
    );
}

#[test]
fn nmap_desc_truncated_at_40_chars() {
    // A very long RHS should be truncated at 40 Unicode scalar values + "…".
    let long_rhs: String = "a".repeat(50);
    let mut app = App::new(None, false, None, None).unwrap();
    app.dispatch_ex(&format!("nmap x {long_rhs}"));

    let entries = entries_for(&app.app_keymap, HjklMode::Normal, &[], ' ');
    let x_entry = entries
        .iter()
        .find(|e| e.key == "x")
        .expect("expected 'x' entry after nmap");

    // Total chars: "→ " (2) + 40 'a' + "…" = 43 or less (truncate_desc cuts at 40 total).
    assert!(
        x_entry.desc.chars().count() <= 42,
        "desc should be truncated, got {} chars: {:?}",
        x_entry.desc.chars().count(),
        x_entry.desc
    );
    assert!(
        x_entry.desc.ends_with('…'),
        "truncated desc should end with '…', got: {:?}",
        x_entry.desc
    );
}

// ── Backspace/Esc chord navigation (Phase 4) ──────────────────────────────────

fn nkey(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

#[test]
fn esc_cancels_engine_g_pending() {
    // Regression: Esc in Normal mode must cancel an in-flight engine/app chord
    // (the which-key toggle must not swallow the cancel).
    let mut app = App::new(None, false, None, None).unwrap();
    hjkl_engine::BufferEdit::replace_all(app.active_editor_mut().buffer_mut(), "abc\ndef");
    let _ = app.handle_keypress(nkey('g'));
    assert!(app.any_chord_pending(), "after 'g' a chord must be pending");
    let _ = app.handle_keypress(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !app.any_chord_pending(),
        "Esc must cancel the pending g-chord"
    );
}

#[test]
fn backspace_pops_chord_to_root() {
    let mut app = App::new(None, false, None, None).unwrap();
    hjkl_engine::BufferEdit::replace_all(app.active_editor_mut().buffer_mut(), "abc\ndef");
    let _ = app.handle_keypress(nkey('g'));
    assert!(app.any_chord_pending());
    assert_eq!(app.chord_history.len(), 1, "history holds the 'g'");
    let _ = app.handle_keypress(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert!(
        !app.any_chord_pending(),
        "Backspace at depth 1 pops back to root"
    );
    assert!(app.chord_history.is_empty());
    assert!(
        app.which_key_sticky,
        "popped to root keeps the which-key popup showing root entries"
    );
}

#[test]
fn backspace_pops_one_level_within_g_chord() {
    // `gc` enters the comment operator (engine pending); Backspace pops the `c`
    // back to the `g` level so e.g. `gc<BS>cc` resolves as `gcc`.
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_editor_mut().set_filetype("rust");
    hjkl_engine::BufferEdit::replace_all(app.active_editor_mut().buffer_mut(), "let x = 1;\n");
    let _ = app.handle_keypress(nkey('g'));
    let _ = app.handle_keypress(nkey('c'));
    assert!(app.any_chord_pending(), "gc must leave a chord pending");
    let _ = app.handle_keypress(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert!(
        app.any_chord_pending(),
        "after gc<BS> a chord is still pending (back at the g level)"
    );
    assert_eq!(
        app.chord_history,
        vec![nkey('g')],
        "history popped back to just [g]"
    );
}

// ── Entry struct construction ─────────────────────────────────────────────────

#[test]
fn entry_new_constructor() {
    let e = Entry::new("x", "exit");
    assert_eq!(e.key, "x");
    assert_eq!(e.desc, "exit");
}
