//! Integration tests that exercise the vim FSM through ex commands.
//!
//! Migrated from `hjkl-engine/src/vim.rs` when ex was relocated to
//! this crate. Live here because ex commands now live here; cargo
//! dev-dep cycles between hjkl-engine and hjkl-editor produce
//! duplicate type IDs that block the in-engine version of these
//! tests from compiling.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hjkl_editor::runtime::ex;
use hjkl_engine::{Editor, KeybindingMode};

fn editor_with(content: &str) -> Editor<'static> {
    let mut e = Editor::new(KeybindingMode::Vim);
    e.set_content(content);
    e
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn run_keys(e: &mut Editor<'_>, s: &str) {
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            // Read the named token until '>'.
            let mut name = String::new();
            for nc in chars.by_ref() {
                if nc == '>' {
                    break;
                }
                name.push(nc);
            }
            match name.as_str() {
                "Esc" => {
                    e.handle_key(key(KeyCode::Esc));
                }
                "CR" | "Enter" => {
                    e.handle_key(key(KeyCode::Enter));
                }
                "BS" | "Backspace" => {
                    e.handle_key(key(KeyCode::Backspace));
                }
                "Tab" => {
                    e.handle_key(key(KeyCode::Tab));
                }
                other => panic!("unsupported key macro <{other}> in test stream"),
            }
        } else {
            e.handle_key(key(KeyCode::Char(c)));
        }
    }
}

#[test]
fn gqq_reflows_current_line_to_textwidth() {
    let mut e = editor_with("alpha beta gamma delta epsilon zeta eta theta iota");
    ex::run(&mut e, "set tw=20");
    assert_eq!(e.settings().textwidth, 20);
    run_keys(&mut e, "gqq");
    for line in e.buffer().lines() {
        assert!(line.chars().count() <= 20, "line too long: {line:?}");
    }
    assert!(e.buffer().lines().len() > 1);
}

#[test]
fn gq_motion_reflows_paragraph() {
    let mut e = editor_with("one two three\nfour five six\nseven eight\n\ntail");
    ex::run(&mut e, "set tw=15");
    e.jump_cursor(0, 0);
    run_keys(&mut e, "gq}");
    assert_eq!(e.buffer().lines().last().unwrap(), "tail");
}

#[test]
fn gq_preserves_paragraph_breaks() {
    let mut e = editor_with("alpha beta gamma\n\ndelta epsilon zeta");
    ex::run(&mut e, "set tw=10");
    run_keys(&mut e, "ggVGgq");
    let blanks = e.buffer().lines().iter().filter(|l| l.is_empty()).count();
    assert_eq!(blanks, 1);
}

#[test]
fn gqq_undo_restores_original_line() {
    let mut e = editor_with("a b c d e f g h i j k l m n o p");
    ex::run(&mut e, "set tw=10");
    let before: Vec<String> = e.buffer().lines().to_vec();
    run_keys(&mut e, "gqq");
    hjkl_engine::do_undo(&mut e);
    assert_eq!(e.buffer().lines(), before);
}

#[test]
fn capital_mark_shows_in_marks_listing() {
    let mut e = editor_with("a\nb\nc");
    e.jump_cursor(2, 0);
    run_keys(&mut e, "mZ");
    e.jump_cursor(0, 0);
    run_keys(&mut e, "ma");
    let info = match ex::run(&mut e, "marks") {
        ex::ExEffect::Info(s) => s,
        other => panic!("expected Info, got {other:?}"),
    };
    assert!(info.contains(" a "));
    assert!(info.contains(" Z "));
}
