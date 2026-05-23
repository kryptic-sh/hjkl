use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hjkl_engine::VimMode;
use std::time::Duration;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn op_kind_to_operator(k: hjkl_vim::OperatorKind) -> hjkl_engine::Operator {
    match k {
        hjkl_vim::OperatorKind::Delete => hjkl_engine::Operator::Delete,
        hjkl_vim::OperatorKind::Yank => hjkl_engine::Operator::Yank,
        hjkl_vim::OperatorKind::Change => hjkl_engine::Operator::Change,
        hjkl_vim::OperatorKind::Indent => hjkl_engine::Operator::Indent,
        hjkl_vim::OperatorKind::Outdent => hjkl_engine::Operator::Outdent,
        hjkl_vim::OperatorKind::Uppercase => hjkl_engine::Operator::Uppercase,
        hjkl_vim::OperatorKind::Lowercase => hjkl_engine::Operator::Lowercase,
        hjkl_vim::OperatorKind::ToggleCase => hjkl_engine::Operator::ToggleCase,
        hjkl_vim::OperatorKind::Reflow => hjkl_engine::Operator::Reflow,
        hjkl_vim::OperatorKind::AutoIndent => hjkl_engine::Operator::AutoIndent,
        hjkl_vim::OperatorKind::Filter => hjkl_engine::Operator::Filter,
        hjkl_vim::OperatorKind::Comment => hjkl_engine::Operator::Comment,
    }
}

fn ctrl_key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn type_str(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_command_field_key(key(KeyCode::Char(c)));
    }
}

fn type_search(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_search_field_key(key(KeyCode::Char(c)));
    }
}

fn seed_buffer(app: &mut App, content: &str) {
    BufferEdit::replace_all(app.active_mut().editor.buffer_mut(), content);
}

/// Helper: bump mtime by writing a file then sleeping briefly so the
/// filesystem timestamp advances past the stored baseline.
fn write_and_wait(path: &std::path::Path, content: &str) {
    std::fs::write(path, content).unwrap();
    // Give the FS time to advance mtime past what we stored at load.
    std::thread::sleep(Duration::from_millis(50));
}

fn inject_split_rect(
    layout: &mut window::LayoutTree,
    id: window::WindowId,
    rect: ratatui::layout::Rect,
) {
    let lr = window::rect_to_layout(rect);
    if let window::LayoutTree::Split {
        a, b, last_rect, ..
    } = layout
        && (a.contains(id) || b.contains(id))
    {
        *last_rect = Some(lr);
        if let window::LayoutTree::Split { .. } = a.as_mut() {
            inject_split_rect(a, id, rect);
        }
        if let window::LayoutTree::Split { .. } = b.as_mut() {
            inject_split_rect(b, id, rect);
        }
    }
}

fn pub_diags_params(file_url: &str, diags: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "uri": file_url,
        "diagnostics": diags,
    })
}

/// Returns the file:// URL string for an absolute path. Cross-platform via
/// hjkl_lsp::uri::from_path (handles Windows drive letters and URL-escaping).
fn file_url(path: &std::path::Path) -> String {
    hjkl_lsp::uri::from_path(path).unwrap().to_string()
}

/// Cross-platform temp path builder. Replaces hardcoded `/tmp/...` so tests
/// pass on Windows CI runners.
fn tmp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(name)
}

fn make_location(uri: &str, row: u32, col: u32) -> lsp_types::Location {
    lsp_types::Location {
        uri: uri.parse::<lsp_types::Uri>().expect("valid URI"),
        range: lsp_types::Range {
            start: lsp_types::Position {
                line: row,
                character: col,
            },
            end: lsp_types::Position {
                line: row,
                character: col + 1,
            },
        },
    }
}

fn ok_val(v: serde_json::Value) -> Result<serde_json::Value, hjkl_lsp::RpcError> {
    Ok(v)
}

fn err_val(msg: &str) -> Result<serde_json::Value, hjkl_lsp::RpcError> {
    Err(hjkl_lsp::RpcError {
        code: -32601,
        message: msg.to_string(),
    })
}

fn make_completion_item(label: &str) -> crate::completion::CompletionItem {
    crate::completion::CompletionItem::new(label)
}

fn synthesize_completion_response(labels: &[&str]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = labels
        .iter()
        .map(|l| serde_json::json!({ "label": l }))
        .collect();
    serde_json::json!(items)
}

#[allow(clippy::mutable_key_type)]
fn make_workspace_edit(
    uri: &str,
    start_line: u32,
    start_char: u32,
    end_line: u32,
    end_char: u32,
    new_text: &str,
) -> lsp_types::WorkspaceEdit {
    let url = uri.parse::<lsp_types::Uri>().expect("valid URI");
    let mut changes = std::collections::HashMap::new();
    changes.insert(
        url,
        vec![lsp_types::TextEdit {
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line: start_line,
                    character: start_char,
                },
                end: lsp_types::Position {
                    line: end_line,
                    character: end_char,
                },
            },
            new_text: new_text.to_string(),
        }],
    );
    lsp_types::WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    }
}

fn drive_key(app: &mut App, ct_key: KeyEvent) {
    // App-level pending-state reducer (hjkl-vim): takes priority over everything.
    if let Some(state) = app.pending_state {
        use hjkl_vim::{Key as VimKey, Outcome};
        let vim_key = match ct_key.code {
            KeyCode::Char(c) => Some(VimKey::Char(c)),
            KeyCode::Esc => Some(VimKey::Esc),
            KeyCode::Enter => Some(VimKey::Enter),
            KeyCode::Backspace => Some(VimKey::Backspace),
            KeyCode::Tab => Some(VimKey::Tab),
            _ => None,
        };
        if let Some(vk) = vim_key {
            match hjkl_vim::step(state, vk) {
                Outcome::Wait(new_state) => {
                    app.pending_state = Some(new_state);
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::ReplaceChar { ch, count }) => {
                    app.pending_state = None;
                    app.active_mut().editor.replace_char_at(ch, count);
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::FindChar {
                    ch,
                    forward,
                    till,
                    count,
                }) => {
                    app.pending_state = None;
                    app.active_mut().editor.find_char(ch, forward, till, count);
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::AfterGChord { ch, count }) => {
                    app.pending_state = None;
                    // App-level g actions (gt, gd, gi, etc.) take priority.
                    match ch {
                        't' => {
                            app.dispatch_action(
                                crate::keymap_actions::AppAction::Tabnext,
                                count as u32,
                            );
                            return;
                        }
                        'T' => {
                            app.dispatch_action(
                                crate::keymap_actions::AppAction::Tabprev,
                                count as u32,
                            );
                            return;
                        }
                        'd' => {
                            app.dispatch_action(
                                crate::keymap_actions::AppAction::LspGotoDef,
                                count as u32,
                            );
                            return;
                        }
                        'D' => {
                            app.dispatch_action(
                                crate::keymap_actions::AppAction::LspGotoDecl,
                                count as u32,
                            );
                            return;
                        }
                        'r' => {
                            app.dispatch_action(
                                crate::keymap_actions::AppAction::LspGotoRef,
                                count as u32,
                            );
                            return;
                        }
                        'i' => {
                            app.dispatch_action(
                                crate::keymap_actions::AppAction::LspGotoImpl,
                                count as u32,
                            );
                            return;
                        }
                        'y' => {
                            app.dispatch_action(
                                crate::keymap_actions::AppAction::LspGotoTypeDef,
                                count as u32,
                            );
                            return;
                        }
                        _ => {}
                    }
                    // Chord-init case-ops: intercept u/U/~/q and set
                    // reducer AfterOp instead of calling after_g.
                    let case_op_kind = match ch {
                        'u' => Some(hjkl_vim::OperatorKind::Lowercase),
                        'U' => Some(hjkl_vim::OperatorKind::Uppercase),
                        '~' => Some(hjkl_vim::OperatorKind::ToggleCase),
                        'q' => Some(hjkl_vim::OperatorKind::Reflow),
                        _ => None,
                    };
                    if let Some(op) = case_op_kind {
                        app.pending_state = Some(hjkl_vim::PendingState::AfterOp {
                            op,
                            count1: count,
                            inner_count: 0,
                        });
                        return;
                    }
                    app.active_mut().editor.after_g(ch, count);
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::AfterZChord { ch, count }) => {
                    app.pending_state = None;
                    app.active_mut().editor.after_z(ch, count);
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpMotion {
                    op,
                    motion_key,
                    total_count,
                }) => {
                    app.pending_state = None;
                    app.active_mut().editor.apply_op_motion(
                        op_kind_to_operator(op),
                        motion_key,
                        total_count,
                    );
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpDouble { op, total_count }) => {
                    app.pending_state = None;
                    app.active_mut()
                        .editor
                        .apply_op_double(op_kind_to_operator(op), total_count);
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpTextObj {
                    op,
                    ch,
                    inner,
                    total_count,
                }) => {
                    app.pending_state = None;
                    app.active_mut().editor.apply_op_text_obj(
                        op_kind_to_operator(op),
                        ch,
                        inner,
                        total_count,
                    );
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpG {
                    op,
                    ch,
                    total_count,
                }) => {
                    app.pending_state = None;
                    app.active_mut()
                        .editor
                        .apply_op_g(op_kind_to_operator(op), ch, total_count);
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::ApplyOpFind {
                    op,
                    ch,
                    forward,
                    till,
                    total_count,
                }) => {
                    app.pending_state = None;
                    app.active_mut().editor.apply_op_find(
                        op_kind_to_operator(op),
                        ch,
                        forward,
                        till,
                        total_count,
                    );
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::SetPendingRegister { reg }) => {
                    app.pending_state = None;
                    app.active_mut().editor.set_pending_register(reg);
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::SetMark { ch }) => {
                    app.pending_state = None;
                    app.active_mut().editor.set_mark_at_cursor(ch);
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::GotoMarkLine { ch }) => {
                    app.pending_state = None;
                    let _ = app.active_mut().editor.try_goto_mark_line(ch);
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::GotoMarkChar { ch }) => {
                    app.pending_state = None;
                    let _ = app.active_mut().editor.try_goto_mark_char(ch);
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::StartMacroRecord { reg }) => {
                    app.pending_state = None;
                    app.active_mut().editor.start_macro_record(reg);
                    return;
                }
                Outcome::Commit(hjkl_vim::EngineCmd::PlayMacro { reg, count }) => {
                    app.pending_state = None;
                    let inputs = app.active_mut().editor.play_macro(reg, count);
                    for input in inputs {
                        let ct_key = engine_input_to_key_event(input);
                        if ct_key.code != KeyCode::Null {
                            drive_key(app, ct_key);
                        }
                    }
                    app.active_mut().editor.end_macro_replay();
                    app.sync_viewport_from_editor();
                    return;
                }
                Outcome::Cancel => {
                    app.pending_state = None;
                    return;
                }
                Outcome::Forward => {
                    // Fall through with state intact.
                }
            }
        }
        // Unrecognised key variant — fall through.
    }
    // Engine pending bypass: if the engine is mid-chord, skip the trie.
    if app.active().editor.is_chord_pending() {
        hjkl_vim_tui::handle_key(&mut app.active_mut().editor, ct_key);
        app.sync_viewport_from_editor();
        return;
    }
    // Try the keymap trie.
    let Some(km_ev) = crate::keymap_translate::from_crossterm(&ct_key) else {
        // Untranslatable key — forward direct to engine.
        hjkl_vim_tui::handle_key(&mut app.active_mut().editor, ct_key);
        app.sync_viewport_from_editor();
        return;
    };
    let mut replay = Vec::new();
    let consumed = app.dispatch_keymap(km_ev, 1, &mut replay);
    if consumed {
        return;
    }
    // Unbound: forward all replay keys (including multi-key) to the engine.
    for ev in &replay {
        let back = crate::keymap_translate::to_crossterm(ev);
        hjkl_vim_tui::handle_key(&mut app.active_mut().editor, back);
    }
    app.sync_viewport_from_editor();
}

fn km_prefix(app: &App, notation: &str) -> Vec<hjkl_keymap::KeyEvent> {
    let leader = app.config.editor.leader;
    hjkl_keymap::Chord::parse(notation, leader)
        .expect("test chord must parse")
        .0
}

fn feed_km_key(app: &mut App, ct_key: KeyEvent) -> bool {
    let Some(km_ev) = crate::keymap_translate::from_crossterm(&ct_key) else {
        return false;
    };
    let mut replay = Vec::new();
    app.dispatch_keymap(km_ev, 1, &mut replay)
}

fn drive_chars(app: &mut App, s: &str) {
    for c in s.chars() {
        drive_key(app, key(KeyCode::Char(c)));
    }
}

fn km_char(c: char) -> hjkl_keymap::KeyEvent {
    hjkl_keymap::KeyEvent::new(
        hjkl_keymap::KeyCode::Char(c),
        hjkl_keymap::KeyModifiers::empty(),
    )
}

fn win_cursor_row(app: &App) -> usize {
    let fw = app.focused_window();
    app.windows[fw].as_ref().unwrap().cursor_row
}

fn win_cursor_col(app: &App) -> usize {
    let fw = app.focused_window();
    app.windows[fw].as_ref().unwrap().cursor_col
}

/// Window cache must mirror engine state after every dispatch.
/// Bug class: any sync-missing arm leaves these diverged. Call from
fn assert_window_synced_to_engine(app: &App) {
    let fw = app.focused_window();
    let win = app.windows[fw].as_ref().unwrap();
    let (e_row, e_col) = app.active().editor.cursor();
    let e_top = app.active().editor.host().viewport().top_row;
    assert_eq!(
        win.cursor_row, e_row,
        "window.cursor_row out of sync with engine cursor"
    );
    assert_eq!(
        win.cursor_col, e_col,
        "window.cursor_col out of sync with engine cursor"
    );
    assert_eq!(
        win.top_row, e_top,
        "window.top_row out of sync with engine viewport"
    );
}

fn ck(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn macro_key_seq(app: &mut App, keys: &[KeyEvent]) {
    use crate::app::event_loop::KeyOutcome;
    for &k in keys {
        // Mirror the live event loop exactly: handle_keypress first (which
        // itself calls route_chord_key + the Insert-mode completion-aware
        // inline dispatcher), and only fall through to dispatch_insert_key /
        // hjkl_vim_tui::handle_key when handle_keypress returns FallThrough.
        // Anything less leaves the test exercising a different code path than
        // production and masks bugs in the handle_keypress inline dispatcher.
        match app.handle_keypress(k) {
            KeyOutcome::Break | KeyOutcome::Continue => {}
            KeyOutcome::FallThrough => {
                if app.active().editor.vim_mode() == VimMode::Insert {
                    app.dispatch_insert_key(k);
                } else {
                    hjkl_vim_tui::handle_key(&mut app.active_mut().editor, k);
                }
            }
        }
        app.sync_viewport_from_editor();
    }
}

fn seed_numbered_lines(app: &mut App, count: usize) {
    let content: String = (1..=count)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    seed_buffer(app, &content);
    app.active_mut().editor.jump_cursor(0, 0);
    app.sync_viewport_from_editor();
}

/// Drive raw keys through `route_chord_key` (recording-aware path).
fn rck(app: &mut App, keys: &[char]) {
    for &c in keys {
        if !app.route_chord_key(ck(c)) {
            hjkl_vim_tui::handle_key(&mut app.active_mut().editor, ck(c));
        }
        app.sync_viewport_from_editor();
    }
}

fn enter_insert(app: &mut App) {
    app.active_mut().editor.enter_insert_i(1);
    app.sync_after_engine_mutation();
    assert_eq!(
        app.active().editor.vim_mode(),
        VimMode::Insert,
        "enter_insert: must be in Insert mode"
    );
}

/// Call `dispatch_insert_key` and sync after.
fn dik(app: &mut App, key: KeyEvent) {
    app.dispatch_insert_key(key);
    app.sync_after_engine_mutation();
}

fn setup_three_slot_app() -> App {
    let path_a = std::env::temp_dir().join("hjkl_4c_a.txt");
    let path_b = std::env::temp_dir().join("hjkl_4c_b.txt");
    let path_c = std::env::temp_dir().join("hjkl_4c_c.txt");
    for p in [&path_a, &path_b, &path_c] {
        std::fs::write(p, "x\n").unwrap();
    }
    let mut app = App::new(Some(path_a), false, None, None).unwrap();
    app.dispatch_ex(&format!("e {}", path_b.display()));
    app.dispatch_ex(&format!("e {}", path_c.display()));
    // active_index == 2
    app
}

fn tab_key() -> KeyEvent {
    KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)
}

fn shift_tab_key() -> KeyEvent {
    KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)
}

pub mod cmdline_window;
pub mod ex;
pub mod formatter;
pub mod keymap;
pub mod lsp;
pub mod marks_registers;
pub mod misc;
pub mod mouse;
pub mod pickers;
pub mod prompt;
pub mod render_recording;
pub mod splits_windows;
pub mod syntax;
pub mod visual;
pub mod which_key;
