//! Pinning tests for #151 Phase D Stage 2b: `BufferSlot` no longer owns an
//! `Editor`, just a document handle (`BufferSlot::view`, a `View` sharing
//! the `Arc<Mutex<Buffer>>` with every window's own `View`) plus a settings
//! template. These tests pin the invariants that removal depends on:
//!
//! - Content mutated through a window editor is visible through the slot
//!   handle (`BufferSlot::buffer()`), and vice versa — they share one
//!   `Buffer`.
//! - The slot handle's edit-channel drains (`take_dirty`,
//!   `take_content_edits`, `take_content_reset`) observe edits made via a
//!   window editor's normal dispatch path (`mutate_edit`), and each drains
//!   exactly once.

use super::*;

/// Edits made via the focused window's editor (the normal dispatch path,
/// funnelled through `Editor::mutate_edit`) are visible through the slot's
/// document handle, and the slot handle's dirty/content-edit channels drain
/// exactly what the window editor produced.
#[test]
fn window_edit_is_visible_and_drainable_through_slot_handle() {
    use hjkl_buffer::{Edit, Position};

    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "hello\nworld");

    // seed_buffer's raw `BufferEdit::replace_all` doesn't route through
    // `Editor::mutate_edit`, so it never touches the dirty/content-edit
    // channels (see `View::replace_all` vs `Editor::set_content`). Drain any
    // residual state before the real assertions so this test only pins
    // what the edit below produces.
    let _ = app.slots()[0].take_dirty();
    let _ = app.slots()[0].take_content_edits();
    let _ = app.slots()[0].take_content_reset();

    // Mutate through the window editor's normal edit funnel
    // (`Editor::mutate_edit` — what every real keystroke and ex-command
    // edit goes through). Driven directly rather than via
    // `dispatch_insert_key`/`sync_after_engine_mutation` (used by the
    // `dik`/`enter_insert` helpers elsewhere in this test suite) because
    // that app-level sync step itself drains dirty/content-edits as part of
    // normal dispatch — this test needs the channels UN-drained so it can
    // assert the slot handle is what does the draining.
    app.active_editor_mut().mutate_edit(Edit::InsertStr {
        at: Position::new(0, 0),
        text: "X".to_string(),
    });

    // Content is visible through the slot handle — same shared `Buffer` as
    // the window editor that made the edit.
    assert_eq!(
        app.slots()[0].buffer().as_string(),
        "Xhello\nworld",
        "edit via the window editor must be visible through the slot handle"
    );
    // Window editor and slot handle read the identical content (same Arc).
    assert_eq!(
        app.active_editor().buffer().as_string(),
        app.slots()[0].buffer().as_string(),
        "window editor and slot handle must agree — they share one Buffer"
    );

    // The slot handle's dirty flag observed the window editor's edit.
    assert!(
        app.slots()[0].take_dirty(),
        "slot handle's take_dirty must see the window editor's edit"
    );
    assert!(
        !app.slots()[0].take_dirty(),
        "take_dirty is a one-shot drain — a second call must return false"
    );

    // The slot handle's content-edit channel observed the window editor's
    // edit too, and also drains exactly once.
    let edits = app.slots()[0].take_content_edits();
    assert!(
        !edits.is_empty(),
        "slot handle's take_content_edits must see the window editor's edit"
    );
    assert!(
        app.slots()[0].take_content_edits().is_empty(),
        "take_content_edits is a one-shot drain — a second call must be empty"
    );
}

/// Edits made directly through the slot's document handle (no window in the
/// loop at all) are immediately visible through the focused window's
/// editor — the two are different `View` instances sharing one `Buffer`.
#[test]
fn slot_handle_edit_is_visible_through_window_editor() {
    use hjkl_engine::BufferEdit;

    let mut app = App::new(None, false, None, None).unwrap();
    seed_buffer(&mut app, "before");

    // Mutate straight through the slot's own View — no Editor involved.
    BufferEdit::replace_all(app.slots_mut()[0].buffer_mut(), "after");

    assert_eq!(
        app.active_editor().buffer().as_string(),
        "after",
        "an edit made through the slot handle must be visible through the \
         focused window's editor (same shared Buffer)"
    );

    // `take_content_reset` also observes a slot-handle-driven reset when
    // explicitly signalled through the shared Buffer's channel — mirrors
    // what `BufferSlot::set_content` does internally.
    app.slots_mut()[0]
        .buffer_mut()
        .set_pending_content_reset(true);
    assert!(
        app.slots()[0].take_content_reset(),
        "content-reset signalled via the slot handle must drain as true"
    );
    assert!(
        !app.slots()[0].take_content_reset(),
        "take_content_reset is a one-shot drain — a second call must be false"
    );
}
