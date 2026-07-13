//! Blame overlay × vim-mode interaction.
//!
//! Relocated from hjkl-engine's inline `blame_view_mode_tests` (#267): the mode
//! transitions these assert on (`set_mode`, `enter_visual_char`,
//! `exit_visual_to_normal`) now live on [`hjkl_vim::VimEditorExt`], and a trait
//! method is unreachable from an in-crate unit test — the `crate::Editor`
//! identity there differs from the `hjkl_engine::Editor` the blanket impl
//! targets. A `tests/` target links hjkl-engine as an external crate, so the
//! impl resolves here.

use hjkl_buffer::Buffer;
use hjkl_engine::types::{DefaultHost, Options};
use hjkl_engine::{Editor, ViewMode, VimMode};
use hjkl_vim::VimEditorExt;

fn make_ed(content: &str) -> Editor<Buffer, DefaultHost> {
    let buf = Buffer::from_str(content);
    hjkl_vim::vim_editor(buf, DefaultHost::default(), Options::default())
}

#[test]
fn enter_blame_sets_view_in_normal() {
    let mut ed = make_ed("hello\nworld");
    assert!(!ed.is_blame());
    assert_eq!(ed.view_mode(), ViewMode::Normal);
    ed.enter_blame();
    assert!(ed.is_blame());
    assert_eq!(ed.view_mode(), ViewMode::Blame);
}

#[test]
fn exit_blame_clears_view() {
    let mut ed = make_ed("hello");
    ed.enter_blame();
    ed.exit_blame();
    assert!(!ed.is_blame());
    assert_eq!(ed.view_mode(), ViewMode::Normal);
}

#[test]
fn enter_blame_is_noop_outside_normal() {
    let mut ed = make_ed("hello");
    ed.set_mode(VimMode::Insert);
    ed.enter_blame();
    assert!(!ed.is_blame(), "BLAME is Normal-only");
    assert_eq!(ed.view_mode(), ViewMode::Normal);
}

#[test]
fn entering_visual_drops_blame() {
    let mut ed = make_ed("hello\nworld");
    ed.enter_blame();
    assert!(ed.is_blame());
    // Mouse drag and keyboard `v` both funnel through this.
    ed.enter_visual_char();
    assert!(!ed.is_blame());
    assert_eq!(ed.view_mode(), ViewMode::Normal);
    // Returning to Normal must NOT resurrect the overlay.
    ed.exit_visual_to_normal();
    assert!(!ed.is_blame());
}

#[test]
fn entering_insert_drops_blame() {
    let mut ed = make_ed("hello");
    ed.enter_blame();
    ed.enter_insert_i(1);
    assert!(!ed.is_blame());
    ed.leave_insert_to_normal();
    assert!(
        !ed.is_blame(),
        "overlay must not resurrect on Esc-to-Normal"
    );
}

#[test]
fn is_blame_masked_while_in_visual() {
    // Even before the overlay flag is dropped, is_blame() is masked on the
    // input mode so the renderer never frames blame outside Normal.
    let mut ed = make_ed("hello");
    ed.enter_blame();
    ed.set_mode(VimMode::Visual);
    assert!(!ed.is_blame());
}

#[test]
fn mutation_blocked_while_blame() {
    let mut ed = make_ed("hello");
    ed.enter_blame();
    let result = ed.mutate_edit(hjkl_buffer::Edit::InsertStr {
        at: hjkl_buffer::Position::new(0, 0),
        text: "XXX".to_string(),
    });
    // BLAME swallows the edit and hands back a self-inverse no-op.
    assert!(
        matches!(result, hjkl_buffer::Edit::InsertStr { ref text, .. } if text.is_empty()),
        "edit must be swallowed while BLAME is active"
    );
}
