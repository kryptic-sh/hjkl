//! Golden snapshot tests for ex command output.
//!
//! These exercise user-visible text the host renders verbatim
//! (`:registers`, `:marks`, `:set` info banners, …). The goal is to
//! catch unintended formatting churn — the kind of change that breaks
//! consumer scrapers / status-line panels but slips past unit tests
//! that only check substrings.
//!
//! Snapshots live in `tests/snapshots/`. Update via `cargo insta
//! review` (interactive) or `INSTA_UPDATE=always cargo test --test
//! golden_ex` (batch). See `CONTRIBUTING.md` for the workflow.

#![cfg(feature = "crossterm")]

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

#[test]
fn registers_listing_layout_is_stable() {
    // Yank a line into the unnamed register so the listing has at
    // least one entry to format.
    let mut e = editor_with("hello world\n");
    e.handle_key(key(KeyCode::Char('y')));
    e.handle_key(key(KeyCode::Char('y')));

    let info = match ex::run(&mut e, "registers") {
        ex::ExEffect::Info(s) => s,
        other => panic!("expected Info, got {other:?}"),
    };
    insta::assert_snapshot!("registers_listing", info);
}

#[test]
fn marks_listing_layout_is_stable() {
    let mut e = editor_with("alpha\nbeta\ngamma\ndelta\n");
    // Set a few marks via the FSM so the listing has content.
    e.jump_cursor(1, 2);
    e.handle_key(key(KeyCode::Char('m')));
    e.handle_key(key(KeyCode::Char('a')));
    e.jump_cursor(3, 0);
    e.handle_key(key(KeyCode::Char('m')));
    e.handle_key(key(KeyCode::Char('Z')));

    let info = match ex::run(&mut e, "marks") {
        ex::ExEffect::Info(s) => s,
        other => panic!("expected Info, got {other:?}"),
    };
    insta::assert_snapshot!("marks_listing", info);
}

#[test]
fn set_listing_layout_is_stable() {
    // Bare `:set` dumps all current option values; format matters
    // for status-line scrapers.
    let mut e = editor_with("");
    let info = match ex::run(&mut e, "set") {
        ex::ExEffect::Info(s) => s,
        other => panic!("expected Info, got {other:?}"),
    };
    insta::assert_snapshot!("set_listing", info);
}
