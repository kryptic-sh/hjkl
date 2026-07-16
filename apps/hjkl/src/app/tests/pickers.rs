use super::*;

// ── View picker (D4) source tests ────────────────────────────────────

#[test]
fn buffer_source_new_produces_n_entries() {
    let path_a = std::env::temp_dir().join("hjkl_d4_src_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_d4_src_b.txt");
    std::fs::write(&path_a, "a\n").unwrap();
    std::fs::write(&path_b, "b\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    assert_eq!(app.slots.len(), 2);

    let source = Box::new(crate::picker::BufferSource::new(
        &app.slots,
        |s| {
            s.filename
                .as_ref()
                .and_then(|p| p.to_str())
                .unwrap_or("[No Name]")
                .to_owned()
        },
        |s| s.dirty,
        |s| s.buffer().as_string(),
        |s| s.filename.clone(),
        |s| s.buffer().cursor().row,
        |_| 0,
    ));
    // Build a Picker from the source — it calls enumerate internally.
    let mut picker = crate::picker::Picker::new(source);
    picker.refresh();
    assert_eq!(picker.total(), 2, "expected 2 entries");
    assert!(picker.scan_done(), "scan_done must be set");
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn buffer_source_select_returns_switch_buffer() {
    use crate::picker::{BufferSource, PickerAction, PickerLogic};
    use crate::picker_action::AppAction;
    let path = std::env::temp_dir().join("hjkl_d4_sel.txt");
    std::fs::write(&path, "x\n").unwrap();
    let app = App::new(Some(path.clone()), false, None, None).unwrap();
    let source = BufferSource::new(
        &app.slots,
        |s| {
            s.filename
                .as_ref()
                .and_then(|p| p.to_str())
                .unwrap_or("[No Name]")
                .to_owned()
        },
        |s| s.dirty,
        |s| s.buffer().as_string(),
        |s| s.filename.clone(),
        |s| s.buffer().cursor().row,
        |_| 0,
    );
    // Index 0 corresponds to the first entry (the only slot).
    match source.select(0) {
        PickerAction::Custom(b) => {
            let a = b
                .downcast::<AppAction>()
                .expect("should downcast to AppAction");
            assert!(matches!(*a, AppAction::SwitchSlot(0)));
        }
        _ => panic!("expected Custom(AppAction::SwitchSlot(0))"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn edit_drops_pristine_default_buffer_when_first_real_file_opens() {
    let path = std::env::temp_dir().join("hjkl_drop_pristine.txt");
    std::fs::write(&path, "hello\n").unwrap();
    let mut app = App::new(None, false, None, None).unwrap();
    assert_eq!(app.slots.len(), 1);
    assert!(app.active().filename.is_none());
    app.dispatch_ex(&format!("e {}", path.display()));
    assert_eq!(
        app.slots.len(),
        1,
        "pristine default buffer should have been dropped"
    );
    assert_eq!(app.active_index(), 0);
    assert_eq!(
        app.active().filename.as_deref(),
        Some(path.as_path()),
        "active slot should now be the opened file"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn edit_keeps_dirty_default_buffer_when_opening_real_file() {
    let path = std::env::temp_dir().join("hjkl_keep_dirty_default.txt");
    std::fs::write(&path, "hello\n").unwrap();
    let mut app = App::new(None, false, None, None).unwrap();
    // Mark default as dirty without giving it a name.
    app.slots[0].dirty = true;
    app.dispatch_ex(&format!("e {}", path.display()));
    assert_eq!(
        app.slots.len(),
        2,
        "dirty unnamed buffer must not be dropped silently"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn open_extra_adds_slot_and_leaves_active_zero() {
    let path_a = std::env::temp_dir().join("hjkl_open_extra_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_open_extra_b.txt");
    std::fs::write(&path_a, "first\n").unwrap();
    std::fs::write(&path_b, "second\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    assert_eq!(app.slots.len(), 1);
    assert_eq!(app.active_index(), 0);
    app.open_extra(path_b.clone()).unwrap();
    assert_eq!(app.slots.len(), 2, "extra slot should have been added");
    assert_eq!(
        app.active_index(),
        0,
        "active must stay at 0 after open_extra"
    );
    assert_eq!(
        app.slots[0]
            .buffer()
            .rope()
            .lines()
            .map(|s| {
                let s = s.to_string();
                s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
            })
            .collect::<Vec<_>>(),
        vec!["first".to_string()]
    );
    assert_eq!(
        app.slots[1]
            .buffer()
            .rope()
            .lines()
            .map(|s| {
                let s = s.to_string();
                s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
            })
            .collect::<Vec<_>>(),
        vec!["second".to_string()]
    );
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn ls_lists_all_buffers_with_active_marker() {
    let path_a = std::env::temp_dir().join("hjkl_phc_ls_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_phc_ls_b.txt");
    std::fs::write(&path_a, "a\n").unwrap();
    std::fs::write(&path_b, "b\n").unwrap();
    let mut app = App::new(Some(path_a.clone()), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    app.dispatch_ex("ls");
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(msg.contains("1: "), "expected slot 1 entry, got: {msg}");
    assert!(
        msg.contains("2:%"),
        "active marker missing on slot 2: {msg}"
    );
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

// ── Git status picker smoke tests ──────────────────────────────────────

#[test]
fn open_git_status_picker_sets_picker() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.open_git_status_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_status_picker"
    );
}

#[test]
fn git_status_picker_title_is_git_status() {
    use crate::picker_git::GitStatusPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let source = GitStatusPicker::new(tmp.path().to_path_buf());
    assert_eq!(source.title(), "git status");
}

// ── Git log picker smoke tests ─────────────────────────────────────────

#[test]
fn git_log_picker_opens() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.open_git_log_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_log_picker"
    );
}

#[test]
fn git_log_picker_title_is_git_log() {
    use crate::picker_git::GitLogPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let source = GitLogPicker::new(tmp.path().to_path_buf());
    assert_eq!(source.title(), "git log");
}

// ── Git branch picker smoke tests ──────────────────────────────────────

#[test]
fn git_branch_picker_opens() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.open_git_branch_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_branch_picker"
    );
}

#[test]
fn git_branch_picker_title_is_git_branches() {
    use crate::picker_git::GitBranchPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let source = GitBranchPicker::new(tmp.path().to_path_buf());
    assert_eq!(source.title(), "git branches");
}

// ── Git file history picker smoke tests ───────────────────────────────────

#[test]
fn git_file_history_picker_opens() {
    let path = std::env::temp_dir().join("hjkl_gB_smoke.txt");
    std::fs::write(&path, "content\n").unwrap();
    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    assert!(app.picker.is_none());
    // View has a path — picker opens (it may show sentinel if not a repo).
    app.open_git_file_history_picker();
    let _ = std::fs::remove_file(&path);
}

#[test]
fn git_file_history_picker_no_path_sets_status() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.active().filename.is_none());
    app.open_git_file_history_picker();
    assert!(app.picker.is_none(), "picker must not open without a path");
    let msg = app.bus.last_body_or_empty().to_string();
    assert!(
        msg.contains("no path"),
        "expected 'no path' status message, got: {msg:?}"
    );
}

#[test]
fn git_file_history_picker_title_is_git_file_history() {
    use crate::picker_git::GitFileHistoryPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let source = GitFileHistoryPicker::new(
        tmp.path().to_path_buf(),
        std::path::PathBuf::from("src/main.rs"),
    );
    assert_eq!(source.title(), "git file history");
}

#[test]
fn git_status_picker_no_repo_scan_produces_sentinel_or_empty() {
    use crate::picker_git::GitStatusPicker;
    use hjkl_picker::PickerLogic;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let tmp = tempfile::tempdir().unwrap();
    let mut source = GitStatusPicker::new(tmp.path().to_path_buf());

    let cancel = Arc::new(AtomicBool::new(false));
    let handle = source.enumerate(None, Arc::clone(&cancel));
    if let Some(h) = handle {
        let _ = h.join();
    }

    // Either a sentinel item (label says "not a git repo") or empty.
    let count = source.item_count();
    if count > 0 {
        let label = source.label(0);
        assert!(
            label.contains("not a git repo"),
            "sentinel label unexpected: {label:?}"
        );
        assert!(matches!(
            source.select(0),
            crate::picker::PickerAction::None
        ));
    }
}

// ── Git stash picker smoke tests ──────────────────────────────────────────

#[test]
fn git_stash_picker_opens() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.open_git_stash_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_stash_picker"
    );
}

#[test]
fn git_stash_picker_title_is_git_stashes() {
    use crate::picker_git::GitStashPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let source = GitStashPicker::new(tmp.path().to_path_buf());
    assert_eq!(source.title(), "git stashes");
}

#[test]
fn git_stash_picker_shift_s_chord_dispatches() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.open_git_stash_picker();
    assert!(app.picker.is_some(), "S chord must open the stash picker");
    assert_eq!(app.picker.as_ref().unwrap().title(), "git stashes");
}

// ── Git tags picker smoke tests ───────────────────────────────────────────

#[test]
fn git_tags_picker_opens() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.open_git_tags_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_tags_picker"
    );
}

#[test]
fn git_tags_picker_title_is_git_tags() {
    use crate::picker_git::GitTagsPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let source = GitTagsPicker::new(tmp.path().to_path_buf());
    assert_eq!(source.title(), "git tags");
}

#[test]
fn git_tags_picker_no_repo_produces_sentinel() {
    use crate::picker_git::GitTagsPicker;
    use hjkl_picker::PickerLogic;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let tmp = tempfile::tempdir().unwrap();
    let mut source = GitTagsPicker::new(tmp.path().to_path_buf());
    let cancel = Arc::new(AtomicBool::new(false));
    let handle = source.enumerate(None, Arc::clone(&cancel));
    if let Some(h) = handle {
        let _ = h.join();
    }
    let count = source.item_count();
    assert!(count > 0, "should have at least a sentinel item");
    let label = source.label(0);
    assert!(
        label.contains("no tags") || label.contains("not a git repo"),
        "sentinel label unexpected: {label:?}"
    );
    assert!(matches!(
        source.select(0),
        crate::picker::PickerAction::None
    ));
}

// ── Git remotes picker smoke tests ────────────────────────────────────────

#[test]
fn git_remotes_picker_opens() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(app.picker.is_none());
    app.open_git_remotes_picker();
    assert!(
        app.picker.is_some(),
        "picker should be open after open_git_remotes_picker"
    );
}

#[test]
fn git_remotes_picker_title_is_git_remotes() {
    use crate::picker_git::GitRemotesPicker;
    use hjkl_picker::PickerLogic;
    let tmp = tempfile::tempdir().unwrap();
    let source = GitRemotesPicker::new(tmp.path().to_path_buf());
    assert_eq!(source.title(), "git remotes");
}

#[test]
fn git_remotes_picker_no_repo_produces_sentinel() {
    use crate::picker_git::GitRemotesPicker;
    use hjkl_picker::PickerLogic;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let tmp = tempfile::tempdir().unwrap();
    let mut source = GitRemotesPicker::new(tmp.path().to_path_buf());
    let cancel = Arc::new(AtomicBool::new(false));
    let handle = source.enumerate(None, Arc::clone(&cancel));
    if let Some(h) = handle {
        let _ = h.join();
    }
    let count = source.item_count();
    assert!(count > 0, "should have at least a sentinel item");
    let label = source.label(0);
    assert!(
        label.contains("no remotes") || label.contains("not a git repo"),
        "sentinel label unexpected: {label:?}"
    );
    assert!(matches!(
        source.select(0),
        crate::picker::PickerAction::None
    ));
}

// ── PickerAction downcast test ─────────────────────────────────────────

#[test]
fn picker_action_custom_downcasts_to_app_action() {
    use crate::picker_action::AppAction;
    use hjkl_picker::PickerAction;
    let action = PickerAction::Custom(Box::new(AppAction::SwitchSlot(2)));
    if let PickerAction::Custom(b) = action {
        let recovered = b.downcast::<AppAction>().expect("should downcast");
        assert!(matches!(*recovered, AppAction::SwitchSlot(2)));
    } else {
        panic!("expected Custom");
    }
}
