//! `hjkl --embed` — JSON-RPC 2.0 server over stdin/stdout.
//!
//! Newline-delimited. One request per line, one response per line.
//! Errors follow JSON-RPC 2.0: `{"jsonrpc":"2.0","error":{"code":-32601,"message":"..."},"id":...}`.
//! See `docs/embed-rpc.md` for the method catalogue.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::Result;
use hjkl_buffer::Buffer;
use hjkl_editor::buffer::Position;
use hjkl_editor::runtime::ex::{self, ExEffect};
use hjkl_engine::{BufferEdit, DefaultHost, Editor, Options, VimMode};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 error codes
// ---------------------------------------------------------------------------

const ERR_PARSE: i64 = -32700;
const ERR_INVALID_REQUEST: i64 = -32600;
const ERR_METHOD_NOT_FOUND: i64 = -32601;
const ERR_INVALID_PARAMS: i64 = -32602;
/// Server-defined: ex-command failure.
const ERR_EX_COMMAND: i64 = -32000;

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

fn success(id: &Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "result": result,
        "id": id,
    })
}

fn error_resp(id: &Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "error": {
            "code": code,
            "message": message,
        },
        "id": id,
    })
}

fn write_response(stdout: &mut impl Write, v: &Value) -> Result<()> {
    let s = serde_json::to_string(v)?;
    stdout.write_all(s.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Editor construction (mirrors headless.rs)
// ---------------------------------------------------------------------------

fn build_editor(
    maybe_path: Option<&PathBuf>,
) -> Result<(Editor<Buffer, DefaultHost>, Option<PathBuf>)> {
    let mut buffer = Buffer::new();

    if let Some(path) = maybe_path {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                let content = content.strip_suffix('\n').unwrap_or(&content);
                BufferEdit::replace_all(&mut buffer, content);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // New file — start empty.
            }
            Err(e) => {
                return Err(anyhow::anyhow!("hjkl: {}: {e}", path.display()));
            }
        }
    }

    let host = DefaultHost::new();
    let editor = Editor::new(buffer, host, Options::default());
    Ok((editor, maybe_path.cloned()))
}

// ---------------------------------------------------------------------------
// Method dispatch
// ---------------------------------------------------------------------------

fn dispatch(
    editor: &mut Editor<Buffer, DefaultHost>,
    current_filename: &mut Option<PathBuf>,
    should_quit: &mut bool,
    method: &str,
    params: &Value,
    id: &Value,
) -> Value {
    match method {
        "hjkl_input" => {
            let keys = match params_positional_str(params, 0) {
                Ok(k) => k,
                Err(msg) => return error_resp(id, ERR_INVALID_PARAMS, &msg),
            };
            let inputs = hjkl_engine::decode_macro(&keys);
            for input in inputs {
                hjkl_engine::step(editor, input);
            }
            success(id, Value::Null)
        }

        "hjkl_command" => {
            let cmd = match params_positional_str(params, 0) {
                Ok(c) => c,
                Err(msg) => return error_resp(id, ERR_INVALID_PARAMS, &msg),
            };
            let cmd = cmd.strip_prefix(':').unwrap_or(&cmd).to_string();
            let effect = ex::run(editor, &cmd);
            match effect {
                ExEffect::None
                | ExEffect::Ok
                | ExEffect::Info(_)
                | ExEffect::Substituted { .. } => success(id, Value::Null),
                ExEffect::Error(msg) | ExEffect::Unknown(msg) => {
                    error_resp(id, ERR_EX_COMMAND, &msg)
                }
                ExEffect::Save => {
                    if let Err(e) = write_buffer(editor, current_filename) {
                        error_resp(id, ERR_EX_COMMAND, &e.to_string())
                    } else {
                        success(id, Value::Null)
                    }
                }
                ExEffect::SaveAs(path_str) => {
                    let new_path = PathBuf::from(&path_str);
                    if let Err(e) = write_buffer(editor, &Some(new_path.clone())) {
                        error_resp(id, ERR_EX_COMMAND, &e.to_string())
                    } else {
                        *current_filename = Some(new_path);
                        success(id, Value::Null)
                    }
                }
                ExEffect::Quit { save, force: _ } => {
                    if save && let Err(e) = write_buffer(editor, current_filename) {
                        return error_resp(id, ERR_EX_COMMAND, &e.to_string());
                    }
                    *should_quit = true;
                    success(id, Value::Null)
                }
            }
        }

        "hjkl_get_buffer" => {
            let lines: Vec<Value> = editor
                .buffer()
                .lines()
                .iter()
                .map(|l| Value::String(l.clone()))
                .collect();
            success(id, Value::Array(lines))
        }

        "hjkl_set_buffer" => {
            let lines = match params_array(params, 0) {
                Ok(arr) => arr,
                Err(msg) => return error_resp(id, ERR_INVALID_PARAMS, &msg),
            };
            let mut strings: Vec<String> = Vec::with_capacity(lines.len());
            for v in &lines {
                match v.as_str() {
                    Some(s) => strings.push(s.to_string()),
                    None => {
                        return error_resp(
                            id,
                            ERR_INVALID_PARAMS,
                            "hjkl_set_buffer: each element must be a string",
                        );
                    }
                }
            }
            let content = if strings.is_empty() {
                String::new()
            } else {
                let mut s = strings.join("\n");
                s.push('\n');
                s
            };
            editor.set_content(&content);
            success(id, Value::Null)
        }

        "hjkl_get_cursor" => {
            let (row, col) = editor.cursor();
            success(id, json!([row, col]))
        }

        "hjkl_set_cursor" => {
            let row = match params_positional_u64(params, 0) {
                Ok(v) => v as usize,
                Err(msg) => return error_resp(id, ERR_INVALID_PARAMS, &msg),
            };
            let col = match params_positional_u64(params, 1) {
                Ok(v) => v as usize,
                Err(msg) => return error_resp(id, ERR_INVALID_PARAMS, &msg),
            };
            let pos = Position::new(row, col);
            editor.buffer_mut().set_cursor(pos);
            success(id, Value::Null)
        }

        "hjkl_get_mode" => {
            let mode_str = match editor.vim_mode() {
                VimMode::Normal => "normal",
                VimMode::Insert => "insert",
                VimMode::Visual => "visual",
                VimMode::VisualLine => "visual_line",
                VimMode::VisualBlock => "visual_block",
            };
            success(id, Value::String(mode_str.to_string()))
        }

        "hjkl_get_register" => {
            let reg_str = match params_positional_str(params, 0) {
                Ok(s) => s,
                Err(msg) => return error_resp(id, ERR_INVALID_PARAMS, &msg),
            };
            let mut chars = reg_str.chars();
            let c = match chars.next() {
                Some(ch) => ch,
                None => {
                    return error_resp(
                        id,
                        ERR_INVALID_PARAMS,
                        "hjkl_get_register: reg must be a single character",
                    );
                }
            };
            if chars.next().is_some() {
                return error_resp(
                    id,
                    ERR_INVALID_PARAMS,
                    "hjkl_get_register: reg must be a single character",
                );
            }
            match editor.registers().read(c) {
                None => success(id, Value::Null),
                Some(slot) => success(
                    id,
                    json!({
                        "text": slot.text,
                        "linewise": slot.linewise,
                    }),
                ),
            }
        }

        _ => error_resp(
            id,
            ERR_METHOD_NOT_FOUND,
            &format!("method not found: {method}"),
        ),
    }
}

// ---------------------------------------------------------------------------
// Param extractors
// ---------------------------------------------------------------------------

fn params_positional_str(params: &Value, idx: usize) -> std::result::Result<String, String> {
    match params {
        Value::Array(arr) => match arr.get(idx) {
            Some(Value::String(s)) => Ok(s.clone()),
            Some(other) => Err(format!("params[{idx}] must be a string, got {other}")),
            None => Err(format!("params[{idx}] missing")),
        },
        _ => Err("params must be an array".to_string()),
    }
}

fn params_positional_u64(params: &Value, idx: usize) -> std::result::Result<u64, String> {
    match params {
        Value::Array(arr) => match arr.get(idx) {
            Some(Value::Number(n)) => n
                .as_u64()
                .ok_or_else(|| format!("params[{idx}] must be a non-negative integer")),
            Some(other) => Err(format!("params[{idx}] must be a number, got {other}")),
            None => Err(format!("params[{idx}] missing")),
        },
        _ => Err("params must be an array".to_string()),
    }
}

fn params_array(params: &Value, idx: usize) -> std::result::Result<Vec<Value>, String> {
    match params {
        Value::Array(arr) => match arr.get(idx) {
            Some(Value::Array(inner)) => Ok(inner.clone()),
            Some(other) => Err(format!("params[{idx}] must be an array, got {other}")),
            None => Err(format!("params[{idx}] missing")),
        },
        _ => Err("params must be an array".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Buffer write helper (mirrors headless.rs)
// ---------------------------------------------------------------------------

fn write_buffer(
    editor: &Editor<Buffer, DefaultHost>,
    path: &Option<PathBuf>,
) -> std::result::Result<(), String> {
    match path {
        None => Err("E32: No file name".to_string()),
        Some(p) => {
            let lines = editor.buffer().lines();
            let content = if lines.is_empty() {
                String::new()
            } else {
                let mut s = lines.join("\n");
                s.push('\n');
                s
            };
            std::fs::write(p, &content).map_err(|e| format!("hjkl: {}: {e}", p.display()))
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run in embed mode: JSON-RPC 2.0 server over stdin/stdout.
///
/// `files` — files to open. Only the first is loaded; the rest are ignored
/// for now (Phase 2.5 will add multi-buffer support).
pub fn run(files: Vec<PathBuf>) -> Result<i32> {
    let first_file = files.into_iter().next();
    let (mut editor, mut current_filename) = build_editor(first_file.as_ref())?;
    let mut should_quit = false;

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdin_lock = stdin.lock();
    let mut stdout_lock = stdout.lock();

    let mut line = String::new();

    loop {
        line.clear();
        let n = stdin_lock.read_line(&mut line)?;
        if n == 0 {
            // EOF — clean exit.
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Parse as JSON.
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => {
                let resp = error_resp(&Value::Null, ERR_PARSE, "Parse error");
                write_response(&mut stdout_lock, &resp)?;
                continue;
            }
        };

        // Validate JSON-RPC 2.0 structure.
        if req.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            let id = req.get("id").cloned().unwrap_or(Value::Null);
            let resp = error_resp(
                &id,
                ERR_INVALID_REQUEST,
                "Invalid Request: missing jsonrpc:2.0",
            );
            write_response(&mut stdout_lock, &resp)?;
            continue;
        }

        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let is_notification = req.get("id").is_none();

        let method = match req.get("method").and_then(Value::as_str) {
            Some(m) => m.to_string(),
            None => {
                if !is_notification {
                    let resp =
                        error_resp(&id, ERR_INVALID_REQUEST, "Invalid Request: missing method");
                    write_response(&mut stdout_lock, &resp)?;
                }
                continue;
            }
        };

        let params = req.get("params").cloned().unwrap_or(Value::Array(vec![]));

        // Notifications (no id) — dispatch but no response.
        if is_notification {
            dispatch(
                &mut editor,
                &mut current_filename,
                &mut should_quit,
                &method,
                &params,
                &Value::Null,
            );
            if should_quit {
                break;
            }
            continue;
        }

        let resp = dispatch(
            &mut editor,
            &mut current_filename,
            &mut should_quit,
            &method,
            &params,
            &id,
        );
        write_response(&mut stdout_lock, &resp)?;

        if should_quit {
            break;
        }
    }

    Ok(0)
}
